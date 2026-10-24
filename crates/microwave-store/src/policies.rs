use crate::identity::{conflict, new_id, now};
use crate::objects::create_object_authz;
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPolicy {
    pub id: String,
    pub authz_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPolicyRevision {
    pub id: String,
    pub policy_id: String,
    pub policy_authz_id: String,
    pub name: String,
    pub revision_id: String,
    pub serialized_object: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPolicyGroup {
    pub id: String,
    pub authz_id: String,
    pub name: String,
    pub serialized_object: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPolicyGroupAssociation {
    pub id: String,
    pub policy_group_name: String,
    pub policy_group_authz_id: String,
    pub policy_name: String,
    pub revision_id: String,
    pub serialized_object: String,
}

impl SqliteStore {
    /// Create a revision, creating the policy object on first use.
    pub fn create_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredPolicyRevision, StoreError> {
        validate_policy_name(name)?;
        validate_revision_id(revision_id)?;
        validate_revision_body(body)?;

        let now = now();
        let revision_row_id = new_id();
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let (policy_id, policy_authz_id) = ensure_policy(&tx, org_id, name, creator_authz_id)?;
        insert_revision(
            &tx,
            &revision_row_id,
            org_id,
            &policy_id,
            name,
            revision_id,
            body,
            creator_authz_id,
            &now,
        )?;
        tx.commit()?;

        Ok(StoredPolicyRevision {
            id: revision_row_id,
            policy_id,
            policy_authz_id,
            name: name.into(),
            revision_id: revision_id.into(),
            serialized_object: body.into(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn find_policy(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredPolicy>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, authz_id, name FROM policies WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| {
                    Ok(StoredPolicy {
                        id: row.get(0)?,
                        authz_id: row.get(1)?,
                        name: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn list_policies(&self, org_id: &str) -> Result<Vec<StoredPolicy>, StoreError> {
        let connection = self.connection();
        let mut statement = connection
            .prepare("SELECT id, authz_id, name FROM policies WHERE org_id = ?1 ORDER BY name")?;
        let rows = statement.query_map([org_id], |row| {
            Ok(StoredPolicy {
                id: row.get(0)?,
                authz_id: row.get(1)?,
                name: row.get(2)?,
            })
        })?;
        let mut policies = Vec::new();
        for row in rows {
            policies.push(row?);
        }
        Ok(policies)
    }

    pub fn find_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
    ) -> Result<Option<StoredPolicyRevision>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT pr.id, pr.policy_id, p.authz_id, pr.name, pr.revision_id,
                        pr.serialized_object, pr.created_at, pr.updated_at
                 FROM policy_revisions pr
                 JOIN policies p ON p.id = pr.policy_id
                 WHERE pr.org_id = ?1 AND pr.name = ?2 AND pr.revision_id = ?3",
                params![org_id, name, revision_id],
                policy_revision_from_row,
            )
            .optional()?)
    }

    pub fn list_policy_revisions(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError> {
        self.list_revisions(org_id, Some(name))
    }

    pub fn list_all_policy_revisions(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError> {
        self.list_revisions(org_id, None)
    }

    fn list_revisions(
        &self,
        org_id: &str,
        name: Option<&str>,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT pr.id, pr.policy_id, p.authz_id, pr.name, pr.revision_id,
                    pr.serialized_object, pr.created_at, pr.updated_at
             FROM policy_revisions pr
             JOIN policies p ON p.id = pr.policy_id
             WHERE pr.org_id = ?1 AND (?2 IS NULL OR pr.name = ?2)
             ORDER BY pr.name, pr.revision_id",
        )?;
        let rows = statement.query_map(params![org_id, name], policy_revision_from_row)?;
        let mut revisions = Vec::new();
        for row in rows {
            revisions.push(row?);
        }
        Ok(revisions)
    }

    /// Delete a policy and (by cascade) all its revisions and group associations.
    pub fn delete_policy(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let authz_id: String = tx
            .query_row(
                "SELECT authz_id FROM policies WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("policy {name}")))?;
        tx.execute("DELETE FROM authz_entities WHERE authz_id = ?1", [authz_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Delete a single revision. The containing policy (and its authz object)
    /// survives so that a new revision can be created under the same name.
    pub fn delete_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection();
        let changed = connection.execute(
            "DELETE FROM policy_revisions
             WHERE org_id = ?1 AND name = ?2 AND revision_id = ?3",
            params![org_id, name, revision_id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!(
                "policy {name} revision {revision_id}"
            )));
        }
        Ok(())
    }

    pub fn create_policy_group(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredPolicyGroup, StoreError> {
        validate_policy_name(name)?;
        validate_revision_body(body)?;

        let now = now();
        let id = new_id();
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let authz_id = create_object_authz(&tx, org_id, "policy_groups", creator_authz_id)?;
        tx.execute(
            "INSERT INTO policy_groups
             (id, org_id, name, authz_id, serialized_object, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, org_id, name, authz_id, body, creator_authz_id, now],
        )
        .map_err(|error| conflict(error, format!("policy group {name} already exists")))?;
        tx.commit()?;

        Ok(StoredPolicyGroup {
            id,
            authz_id,
            name: name.into(),
            serialized_object: body.into(),
        })
    }

    pub fn find_policy_group(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredPolicyGroup>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, authz_id, name, serialized_object
                 FROM policy_groups WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                policy_group_from_row,
            )
            .optional()?)
    }

    pub fn list_policy_groups(&self, org_id: &str) -> Result<Vec<StoredPolicyGroup>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, authz_id, name, serialized_object
             FROM policy_groups WHERE org_id = ?1 ORDER BY name",
        )?;
        let rows = statement.query_map([org_id], policy_group_from_row)?;
        let mut groups = Vec::new();
        for row in rows {
            groups.push(row?);
        }
        Ok(groups)
    }

    /// Delete a policy group and its associations. Policy revisions are left
    /// untouched, matching the upstream CASCADE behavior.
    pub fn delete_policy_group(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let authz_id: String = tx
            .query_row(
                "SELECT authz_id FROM policy_groups WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("policy group {name}")))?;
        tx.execute(
            "DELETE FROM policy_group_revision_associations
             WHERE org_id = ?1 AND policy_group_authz_id = ?2",
            params![org_id, authz_id],
        )?;
        tx.execute("DELETE FROM authz_entities WHERE authz_id = ?1", [authz_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Associate a policy revision with a policy group. The policy, policy
    /// group and revision are created on demand. Returns the association and
    /// whether it was newly created.
    #[allow(clippy::too_many_arguments)]
    pub fn associate_policy_revision(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
        revision_id: &str,
        body: &str,
        group_body: &str,
        creator_authz_id: &str,
    ) -> Result<(StoredPolicyGroupAssociation, bool), StoreError> {
        validate_policy_name(group_name)?;
        validate_policy_name(policy_name)?;
        validate_revision_id(revision_id)?;
        validate_revision_body(body)?;
        validate_revision_body(group_body)?;

        let now = now();
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let (policy_id, _) = ensure_policy(&tx, org_id, policy_name, creator_authz_id)?;
        let (_, group_authz_id) =
            ensure_policy_group(&tx, org_id, group_name, group_body, creator_authz_id)?;
        let (revision_row_id, serialized_object) = ensure_policy_revision(
            &tx,
            org_id,
            &policy_id,
            policy_name,
            revision_id,
            body,
            creator_authz_id,
        )?;

        let existing_id: Option<String> = tx
            .query_row(
                "SELECT id FROM policy_group_revision_associations
                 WHERE org_id = ?1 AND policy_group_name = ?2 AND policy_revision_name = ?3",
                params![org_id, group_name, policy_name],
                |row| row.get(0),
            )
            .optional()?;

        let (id, created) = match existing_id {
            Some(id) => {
                tx.execute(
                    "UPDATE policy_group_revision_associations
                     SET policy_revision_id = ?2, policy_revision_revision_id = ?3,
                         last_updated_by = ?4, updated_at = ?5
                     WHERE id = ?1",
                    params![id, revision_row_id, revision_id, creator_authz_id, now],
                )?;
                (id, false)
            }
            None => {
                let id = new_id();
                tx.execute(
                    "INSERT INTO policy_group_revision_associations
                     (id, org_id, policy_revision_id, policy_group_authz_id,
                      policy_revision_name, policy_revision_revision_id, policy_group_name,
                      last_updated_by, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                    params![
                        id,
                        org_id,
                        revision_row_id,
                        group_authz_id,
                        policy_name,
                        revision_id,
                        group_name,
                        creator_authz_id,
                        now
                    ],
                )?;
                (id, true)
            }
        };
        tx.commit()?;

        Ok((
            StoredPolicyGroupAssociation {
                id,
                policy_group_name: group_name.into(),
                policy_group_authz_id: group_authz_id,
                policy_name: policy_name.into(),
                revision_id: revision_id.into(),
                serialized_object,
            },
            created,
        ))
    }

    pub fn find_policy_group_association(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
    ) -> Result<Option<StoredPolicyGroupAssociation>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT a.id, a.policy_group_name, a.policy_group_authz_id,
                        a.policy_revision_name, a.policy_revision_revision_id,
                        pr.serialized_object
                 FROM policy_group_revision_associations a
                 JOIN policy_revisions pr ON pr.id = a.policy_revision_id
                 WHERE a.org_id = ?1 AND a.policy_group_name = ?2
                   AND a.policy_revision_name = ?3",
                params![org_id, group_name, policy_name],
                policy_group_association_from_row,
            )
            .optional()?)
    }

    pub fn list_policy_group_associations(
        &self,
        org_id: &str,
        group_name: &str,
    ) -> Result<Vec<StoredPolicyGroupAssociation>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT a.id, a.policy_group_name, a.policy_group_authz_id,
                    a.policy_revision_name, a.policy_revision_revision_id,
                    pr.serialized_object
             FROM policy_group_revision_associations a
             JOIN policy_revisions pr ON pr.id = a.policy_revision_id
             WHERE a.org_id = ?1 AND a.policy_group_name = ?2
             ORDER BY a.policy_revision_name",
        )?;
        let rows = statement.query_map(
            params![org_id, group_name],
            policy_group_association_from_row,
        )?;
        let mut associations = Vec::new();
        for row in rows {
            associations.push(row?);
        }
        Ok(associations)
    }

    pub fn list_policy_groups_for_revision(
        &self,
        org_id: &str,
        policy_name: &str,
        revision_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT policy_group_name
             FROM policy_group_revision_associations
             WHERE org_id = ?1 AND policy_revision_name = ?2
               AND policy_revision_revision_id = ?3
             ORDER BY rowid",
        )?;
        let rows = statement.query_map(params![org_id, policy_name, revision_id], |row| {
            row.get::<_, String>(0)
        })?;
        let mut groups = Vec::new();
        for row in rows {
            groups.push(row?);
        }
        Ok(groups)
    }

    pub fn delete_policy_group_association(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection();
        let changed = connection.execute(
            "DELETE FROM policy_group_revision_associations
             WHERE org_id = ?1 AND policy_group_name = ?2 AND policy_revision_name = ?3",
            params![org_id, group_name, policy_name],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!(
                "policy {policy_name} in policy group {group_name}"
            )));
        }
        Ok(())
    }
}

fn ensure_policy(
    tx: &Transaction<'_>,
    org_id: &str,
    name: &str,
    creator_authz_id: &str,
) -> Result<(String, String), StoreError> {
    if let Some(policy) = tx
        .query_row(
            "SELECT id, authz_id FROM policies WHERE org_id = ?1 AND name = ?2",
            params![org_id, name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        return Ok(policy);
    }

    let id = new_id();
    let authz_id = create_object_authz(tx, org_id, "policies", creator_authz_id)?;
    let now = now();
    tx.execute(
        "INSERT INTO policies
         (id, org_id, name, authz_id, last_updated_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![id, org_id, name, authz_id, creator_authz_id, now],
    )
    .map_err(|error| conflict(error, format!("policy {name} already exists")))?;
    Ok((id, authz_id))
}

fn ensure_policy_group(
    tx: &Transaction<'_>,
    org_id: &str,
    name: &str,
    body: &str,
    creator_authz_id: &str,
) -> Result<(String, String), StoreError> {
    if let Some(group) = tx
        .query_row(
            "SELECT id, authz_id FROM policy_groups WHERE org_id = ?1 AND name = ?2",
            params![org_id, name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        return Ok(group);
    }

    let id = new_id();
    let authz_id = create_object_authz(tx, org_id, "policy_groups", creator_authz_id)?;
    let now = now();
    tx.execute(
        "INSERT INTO policy_groups
         (id, org_id, name, authz_id, serialized_object, last_updated_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![id, org_id, name, authz_id, body, creator_authz_id, now],
    )
    .map_err(|error| conflict(error, format!("policy group {name} already exists")))?;
    Ok((id, authz_id))
}

fn ensure_policy_revision(
    tx: &Transaction<'_>,
    org_id: &str,
    policy_id: &str,
    name: &str,
    revision_id: &str,
    body: &str,
    creator_authz_id: &str,
) -> Result<(String, String), StoreError> {
    if let Some(revision) = tx
        .query_row(
            "SELECT id, serialized_object FROM policy_revisions
             WHERE org_id = ?1 AND name = ?2 AND revision_id = ?3",
            params![org_id, name, revision_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        return Ok(revision);
    }

    let id = new_id();
    let now = now();
    insert_revision(
        tx,
        &id,
        org_id,
        policy_id,
        name,
        revision_id,
        body,
        creator_authz_id,
        &now,
    )?;
    Ok((id, body.into()))
}

#[allow(clippy::too_many_arguments)]
fn insert_revision(
    tx: &Transaction<'_>,
    id: &str,
    org_id: &str,
    policy_id: &str,
    name: &str,
    revision_id: &str,
    body: &str,
    creator_authz_id: &str,
    now: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO policy_revisions
         (id, org_id, policy_id, revision_id, name, serialized_object,
          last_updated_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![
            id,
            org_id,
            policy_id,
            revision_id,
            name,
            body,
            creator_authz_id,
            now
        ],
    )
    .map_err(|error| {
        conflict(
            error,
            format!("policy {name} revision {revision_id} already exists"),
        )
    })?;
    Ok(())
}

fn validate_policy_name(name: &str) -> Result<(), StoreError> {
    if name.is_empty() || name.len() > 255 || !valid_token(name) {
        return Err(StoreError::Invalid("Field 'name' invalid".into()));
    }
    Ok(())
}

fn validate_revision_id(revision_id: &str) -> Result<(), StoreError> {
    if revision_id.is_empty() || revision_id.len() > 255 || !valid_token(revision_id) {
        return Err(StoreError::Invalid("Field 'revision_id' invalid".into()));
    }
    Ok(())
}

fn valid_token(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | ':' | '.')
    })
}

fn validate_revision_body(body: &str) -> Result<(), StoreError> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(_)) => Ok(()),
        _ => Err(StoreError::Invalid(
            "policy revision must be a JSON object".into(),
        )),
    }
}

fn policy_revision_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredPolicyRevision> {
    Ok(StoredPolicyRevision {
        id: row.get(0)?,
        policy_id: row.get(1)?,
        policy_authz_id: row.get(2)?,
        name: row.get(3)?,
        revision_id: row.get(4)?,
        serialized_object: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn policy_group_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredPolicyGroup> {
    Ok(StoredPolicyGroup {
        id: row.get(0)?,
        authz_id: row.get(1)?,
        name: row.get(2)?,
        serialized_object: row.get(3)?,
    })
}

fn policy_group_association_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredPolicyGroupAssociation> {
    Ok(StoredPolicyGroupAssociation {
        id: row.get(0)?,
        policy_group_name: row.get(1)?,
        policy_group_authz_id: row.get(2)?,
        policy_name: row.get(3)?,
        revision_id: row.get(4)?,
        serialized_object: row.get(5)?,
    })
}
