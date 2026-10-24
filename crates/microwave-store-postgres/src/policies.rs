use async_trait::async_trait;
use microwave_store::{
    PolicyRepository, StoreError, StoredPolicy, StoredPolicyGroup, StoredPolicyGroupAssociation,
    StoredPolicyRevision,
};
use serde_json::Value;
use sqlx::postgres::Postgres;
use sqlx::{Row, Transaction};

use crate::store::{db, new_id, now, unique, PostgresStore};

#[async_trait]
#[allow(clippy::too_many_arguments)]
impl PolicyRepository for PostgresStore {
    async fn create_policy_revision(
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

        let timestamp = now();
        let revision_row_id = new_id();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let (policy_id, policy_authz_id) =
            ensure_policy(&mut transaction, org_id, name, creator_authz_id).await?;
        insert_revision(
            &mut transaction,
            &revision_row_id,
            org_id,
            &policy_id,
            name,
            revision_id,
            body,
            creator_authz_id,
            &timestamp,
        )
        .await?;
        transaction.commit().await.map_err(db)?;

        Ok(StoredPolicyRevision {
            id: revision_row_id,
            policy_id,
            policy_authz_id,
            name: name.into(),
            revision_id: revision_id.into(),
            serialized_object: body.into(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
    }

    async fn find_policy(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredPolicy>, StoreError> {
        let row =
            sqlx::query("SELECT id, authz_id, name FROM policies WHERE org_id = $1 AND name = $2")
                .bind(org_id)
                .bind(name)
                .fetch_optional(self.pool())
                .await
                .map_err(db)?;
        row.map(|row| {
            Ok(StoredPolicy {
                id: row.try_get(0)?,
                authz_id: row.try_get(1)?,
                name: row.try_get(2)?,
            })
        })
        .transpose()
        .map_err(db)
    }

    async fn list_policies(&self, org_id: &str) -> Result<Vec<StoredPolicy>, StoreError> {
        let rows =
            sqlx::query("SELECT id, authz_id, name FROM policies WHERE org_id = $1 ORDER BY name")
                .bind(org_id)
                .fetch_all(self.pool())
                .await
                .map_err(db)?;
        rows.iter()
            .map(|row| {
                Ok(StoredPolicy {
                    id: row.try_get(0)?,
                    authz_id: row.try_get(1)?,
                    name: row.try_get(2)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn find_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
    ) -> Result<Option<StoredPolicyRevision>, StoreError> {
        let row = sqlx::query(
            "SELECT pr.id, pr.policy_id, p.authz_id, pr.name, pr.revision_id,
                    pr.serialized_object, pr.created_at, pr.updated_at
             FROM policy_revisions pr
             JOIN policies p ON p.id = pr.policy_id
             WHERE pr.org_id = $1 AND pr.name = $2 AND pr.revision_id = $3",
        )
        .bind(org_id)
        .bind(name)
        .bind(revision_id)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| policy_revision_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn list_policy_revisions(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError> {
        self.list_revisions(org_id, Some(name)).await
    }

    async fn list_all_policy_revisions(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError> {
        self.list_revisions(org_id, None).await
    }

    async fn delete_policy(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let authz_id: String =
            sqlx::query_scalar("SELECT authz_id FROM policies WHERE org_id = $1 AND name = $2")
                .bind(org_id)
                .bind(name)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(db)?
                .ok_or_else(|| StoreError::NotFound(format!("policy {name}")))?;
        sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
            .bind(&authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn delete_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "DELETE FROM policy_revisions
             WHERE org_id = $1 AND name = $2 AND revision_id = $3",
        )
        .bind(org_id)
        .bind(name)
        .bind(revision_id)
        .execute(self.pool())
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!(
                "policy {name} revision {revision_id}"
            )));
        }
        Ok(())
    }

    async fn create_policy_group(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredPolicyGroup, StoreError> {
        validate_policy_name(name)?;
        validate_revision_body(body)?;

        let timestamp = now();
        let id = new_id();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let authz_id = crate::objects::create_object_authz(
            &mut transaction,
            org_id,
            "policy_groups",
            creator_authz_id,
        )
        .await?;
        sqlx::query(
            "INSERT INTO policy_groups
             (id, org_id, name, authz_id, serialized_object, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
        )
        .bind(&id)
        .bind(org_id)
        .bind(name)
        .bind(&authz_id)
        .bind(body)
        .bind(creator_authz_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, format!("policy group {name} already exists")))?;
        transaction.commit().await.map_err(db)?;

        Ok(StoredPolicyGroup {
            id,
            authz_id,
            name: name.into(),
            serialized_object: body.into(),
        })
    }

    async fn find_policy_group(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredPolicyGroup>, StoreError> {
        let row = sqlx::query(
            "SELECT id, authz_id, name, serialized_object
             FROM policy_groups WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| policy_group_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn list_policy_groups(&self, org_id: &str) -> Result<Vec<StoredPolicyGroup>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, authz_id, name, serialized_object
             FROM policy_groups WHERE org_id = $1 ORDER BY name",
        )
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(policy_group_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn delete_policy_group(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let authz_id: String = sqlx::query_scalar(
            "SELECT authz_id FROM policy_groups WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or_else(|| StoreError::NotFound(format!("policy group {name}")))?;
        sqlx::query(
            "DELETE FROM policy_group_revision_associations
             WHERE org_id = $1 AND policy_group_authz_id = $2",
        )
        .bind(org_id)
        .bind(&authz_id)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
            .bind(&authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn associate_policy_revision(
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

        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let (policy_id, _) =
            ensure_policy(&mut transaction, org_id, policy_name, creator_authz_id).await?;
        let (_, group_authz_id) = ensure_policy_group(
            &mut transaction,
            org_id,
            group_name,
            group_body,
            creator_authz_id,
        )
        .await?;
        let (revision_row_id, serialized_object) = ensure_policy_revision(
            &mut transaction,
            org_id,
            &policy_id,
            policy_name,
            revision_id,
            body,
            creator_authz_id,
        )
        .await?;

        let existing_id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM policy_group_revision_associations
             WHERE org_id = $1 AND policy_group_name = $2 AND policy_revision_name = $3",
        )
        .bind(org_id)
        .bind(group_name)
        .bind(policy_name)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?;

        let (id, created) = match existing_id {
            Some(id) => {
                sqlx::query(
                    "UPDATE policy_group_revision_associations
                     SET policy_revision_id = $2, policy_revision_revision_id = $3,
                         last_updated_by = $4, updated_at = $5
                     WHERE id = $1",
                )
                .bind(&id)
                .bind(&revision_row_id)
                .bind(revision_id)
                .bind(creator_authz_id)
                .bind(&timestamp)
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
                (id, false)
            }
            None => {
                let id = new_id();
                sqlx::query(
                    "INSERT INTO policy_group_revision_associations
                     (id, org_id, policy_revision_id, policy_group_authz_id,
                      policy_revision_name, policy_revision_revision_id, policy_group_name,
                      last_updated_by, created_at, updated_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)",
                )
                .bind(&id)
                .bind(org_id)
                .bind(&revision_row_id)
                .bind(&group_authz_id)
                .bind(policy_name)
                .bind(revision_id)
                .bind(group_name)
                .bind(creator_authz_id)
                .bind(&timestamp)
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
                (id, true)
            }
        };
        transaction.commit().await.map_err(db)?;

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

    async fn find_policy_group_association(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
    ) -> Result<Option<StoredPolicyGroupAssociation>, StoreError> {
        let row = sqlx::query(
            "SELECT a.id, a.policy_group_name, a.policy_group_authz_id,
                    a.policy_revision_name, a.policy_revision_revision_id,
                    pr.serialized_object
             FROM policy_group_revision_associations a
             JOIN policy_revisions pr ON pr.id = a.policy_revision_id
             WHERE a.org_id = $1 AND a.policy_group_name = $2
               AND a.policy_revision_name = $3",
        )
        .bind(org_id)
        .bind(group_name)
        .bind(policy_name)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| policy_group_association_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn list_policy_group_associations(
        &self,
        org_id: &str,
        group_name: &str,
    ) -> Result<Vec<StoredPolicyGroupAssociation>, StoreError> {
        let rows = sqlx::query(
            "SELECT a.id, a.policy_group_name, a.policy_group_authz_id,
                    a.policy_revision_name, a.policy_revision_revision_id,
                    pr.serialized_object
             FROM policy_group_revision_associations a
             JOIN policy_revisions pr ON pr.id = a.policy_revision_id
             WHERE a.org_id = $1 AND a.policy_group_name = $2
             ORDER BY a.policy_revision_name",
        )
        .bind(org_id)
        .bind(group_name)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(policy_group_association_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn list_policy_groups_for_revision(
        &self,
        org_id: &str,
        policy_name: &str,
        revision_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        let groups = sqlx::query_scalar(
            "SELECT policy_group_name
             FROM policy_group_revision_associations
             WHERE org_id = $1 AND policy_revision_name = $2
               AND policy_revision_revision_id = $3
             ORDER BY created_at, policy_group_name",
        )
        .bind(org_id)
        .bind(policy_name)
        .bind(revision_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        Ok(groups)
    }

    async fn delete_policy_group_association(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "DELETE FROM policy_group_revision_associations
             WHERE org_id = $1 AND policy_group_name = $2 AND policy_revision_name = $3",
        )
        .bind(org_id)
        .bind(group_name)
        .bind(policy_name)
        .execute(self.pool())
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!(
                "policy {policy_name} in policy group {group_name}"
            )));
        }
        Ok(())
    }
}

impl PostgresStore {
    async fn list_revisions(
        &self,
        org_id: &str,
        name: Option<&str>,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError> {
        let rows = sqlx::query(
            "SELECT pr.id, pr.policy_id, p.authz_id, pr.name, pr.revision_id,
                    pr.serialized_object, pr.created_at, pr.updated_at
             FROM policy_revisions pr
             JOIN policies p ON p.id = pr.policy_id
             WHERE pr.org_id = $1 AND ($2::text IS NULL OR pr.name = $2)
             ORDER BY pr.name, pr.revision_id",
        )
        .bind(org_id)
        .bind(name)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(policy_revision_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }
}

async fn ensure_policy(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    name: &str,
    creator_authz_id: &str,
) -> Result<(String, String), StoreError> {
    if let Some(policy) = sqlx::query_as::<_, (String, String)>(
        "SELECT id, authz_id FROM policies WHERE org_id = $1 AND name = $2",
    )
    .bind(org_id)
    .bind(name)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?
    {
        return Ok(policy);
    }

    let id = new_id();
    let authz_id =
        crate::objects::create_object_authz(transaction, org_id, "policies", creator_authz_id)
            .await?;
    let timestamp = now();
    sqlx::query(
        "INSERT INTO policies
         (id, org_id, name, authz_id, last_updated_by, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $6)",
    )
    .bind(&id)
    .bind(org_id)
    .bind(name)
    .bind(&authz_id)
    .bind(creator_authz_id)
    .bind(&timestamp)
    .execute(&mut **transaction)
    .await
    .map_err(|error| unique(error, format!("policy {name} already exists")))?;
    Ok((id, authz_id))
}

async fn ensure_policy_group(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    name: &str,
    body: &str,
    creator_authz_id: &str,
) -> Result<(String, String), StoreError> {
    if let Some(group) = sqlx::query_as::<_, (String, String)>(
        "SELECT id, authz_id FROM policy_groups WHERE org_id = $1 AND name = $2",
    )
    .bind(org_id)
    .bind(name)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?
    {
        return Ok(group);
    }

    let id = new_id();
    let authz_id =
        crate::objects::create_object_authz(transaction, org_id, "policy_groups", creator_authz_id)
            .await?;
    let timestamp = now();
    sqlx::query(
        "INSERT INTO policy_groups
         (id, org_id, name, authz_id, serialized_object, last_updated_by, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
    )
    .bind(&id)
    .bind(org_id)
    .bind(name)
    .bind(&authz_id)
    .bind(body)
    .bind(creator_authz_id)
    .bind(&timestamp)
    .execute(&mut **transaction)
    .await
    .map_err(|error| unique(error, format!("policy group {name} already exists")))?;
    Ok((id, authz_id))
}

async fn ensure_policy_revision(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    policy_id: &str,
    name: &str,
    revision_id: &str,
    body: &str,
    creator_authz_id: &str,
) -> Result<(String, String), StoreError> {
    if let Some(revision) = sqlx::query_as::<_, (String, String)>(
        "SELECT id, serialized_object FROM policy_revisions
         WHERE org_id = $1 AND name = $2 AND revision_id = $3",
    )
    .bind(org_id)
    .bind(name)
    .bind(revision_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?
    {
        return Ok(revision);
    }

    let id = new_id();
    let timestamp = now();
    insert_revision(
        transaction,
        &id,
        org_id,
        policy_id,
        name,
        revision_id,
        body,
        creator_authz_id,
        &timestamp,
    )
    .await?;
    Ok((id, body.into()))
}

#[allow(clippy::too_many_arguments)]
async fn insert_revision(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
    org_id: &str,
    policy_id: &str,
    name: &str,
    revision_id: &str,
    body: &str,
    creator_authz_id: &str,
    timestamp: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO policy_revisions
         (id, org_id, policy_id, revision_id, name, serialized_object,
          last_updated_by, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
    )
    .bind(id)
    .bind(org_id)
    .bind(policy_id)
    .bind(revision_id)
    .bind(name)
    .bind(body)
    .bind(creator_authz_id)
    .bind(timestamp)
    .execute(&mut **transaction)
    .await
    .map_err(|error| {
        unique(
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

fn policy_revision_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<StoredPolicyRevision, sqlx::Error> {
    Ok(StoredPolicyRevision {
        id: row.try_get(0)?,
        policy_id: row.try_get(1)?,
        policy_authz_id: row.try_get(2)?,
        name: row.try_get(3)?,
        revision_id: row.try_get(4)?,
        serialized_object: row.try_get(5)?,
        created_at: row.try_get(6)?,
        updated_at: row.try_get(7)?,
    })
}

fn policy_group_from_row(row: &sqlx::postgres::PgRow) -> Result<StoredPolicyGroup, sqlx::Error> {
    Ok(StoredPolicyGroup {
        id: row.try_get(0)?,
        authz_id: row.try_get(1)?,
        name: row.try_get(2)?,
        serialized_object: row.try_get(3)?,
    })
}

fn policy_group_association_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<StoredPolicyGroupAssociation, sqlx::Error> {
    Ok(StoredPolicyGroupAssociation {
        id: row.try_get(0)?,
        policy_group_name: row.try_get(1)?,
        policy_group_authz_id: row.try_get(2)?,
        policy_name: row.try_get(3)?,
        revision_id: row.try_get(4)?,
        serialized_object: row.try_get(5)?,
    })
}
