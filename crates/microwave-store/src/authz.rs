use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub authz_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub authz_id: String,
    pub name: String,
}

/// An ACL as `(permission, actor names, group names)` entries.
pub type Acl = Vec<(Permission, Vec<String>, Vec<String>)>;

/// One permission's ACE with actor names split by actor type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclEntry {
    pub permission: Permission,
    pub users: Vec<String>,
    pub clients: Vec<String>,
    pub groups: Vec<String>,
}

/// The five Chef permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Create,
    Read,
    Update,
    Delete,
    Grant,
}

impl Permission {
    pub const ALL: [Permission; 5] = [
        Permission::Create,
        Permission::Read,
        Permission::Update,
        Permission::Delete,
        Permission::Grant,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "create" => Some(Self::Create),
            "read" => Some(Self::Read),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            "grant" => Some(Self::Grant),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Read => "read",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Grant => "grant",
        }
    }
}

impl SqliteStore {
    pub fn create_actor(&self) -> Result<String, StoreError> {
        self.create_authz_entity("actor")
    }

    pub fn create_object(&self) -> Result<String, StoreError> {
        self.create_authz_entity("object")
    }

    pub fn create_group(&self, _name: &str) -> Result<String, StoreError> {
        self.create_authz_entity("group")
    }

    pub fn create_container(&self) -> Result<String, StoreError> {
        self.create_authz_entity("container")
    }

    pub fn create_container_for_org(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Container, StoreError> {
        let id = Uuid::new_v4().simple().to_string();
        let authz_id = Uuid::new_v4().simple().to_string();
        let now = crate::identity::now();
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES (?1, 'container', ?2, ?3)",
            params![authz_id, org_id, now],
        )?;
        transaction.execute(
            "INSERT INTO containers (id, org_id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, org_id, authz_id, name, now],
        )?;
        transaction.commit()?;
        Ok(Container {
            id,
            authz_id,
            name: name.into(),
        })
    }

    pub fn create_group_for_org(&self, org_id: &str, name: &str) -> Result<Group, StoreError> {
        let id = Uuid::new_v4().simple().to_string();
        let authz_id = Uuid::new_v4().simple().to_string();
        let now = crate::identity::now();
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES (?1, 'group', ?2, ?3)",
            params![authz_id, org_id, now],
        )?;
        transaction.execute(
            "INSERT INTO groups (id, org_id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, org_id, authz_id, name, now],
        )?;
        transaction.commit()?;
        Ok(Group {
            id,
            authz_id,
            name: name.into(),
        })
    }

    pub fn find_container(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<Container>, StoreError> {
        use rusqlite::OptionalExtension;
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, authz_id, name FROM containers WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| {
                    Ok(Container {
                        id: row.get(0)?,
                        authz_id: row.get(1)?,
                        name: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn find_group(&self, org_id: &str, name: &str) -> Result<Option<Group>, StoreError> {
        use rusqlite::OptionalExtension;
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, authz_id, name FROM groups WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| {
                    Ok(Group {
                        id: row.get(0)?,
                        authz_id: row.get(1)?,
                        name: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn associate_user_org(&self, org_id: &str, user_id: &str) -> Result<(), StoreError> {
        let now = crate::identity::now();
        let connection = self.connection();
        connection.execute(
            "INSERT OR IGNORE INTO org_user_associations
             (org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, '', ?3, ?3)",
            params![org_id, user_id, now],
        )?;
        Ok(())
    }

    /// Remove a user's organization association and all of their group
    /// memberships inside that organization.
    pub fn remove_user_from_org(
        &self,
        org_id: &str,
        user_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let removed = transaction.execute(
            "DELETE FROM org_user_associations WHERE org_id = ?1 AND user_id = ?2",
            params![org_id, user_id],
        )?;
        if removed == 0 {
            return Err(StoreError::NotFound(
                "user is not a member of the organization".into(),
            ));
        }
        transaction.execute(
            "DELETE FROM authz_group_actors
             WHERE actor_authz_id = ?2
               AND group_authz_id IN (SELECT authz_id FROM groups WHERE org_id = ?1)",
            params![org_id, actor_authz_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_org(&self, org_id: &str) -> Result<(), StoreError> {
        let connection = self.connection();
        connection.execute("DELETE FROM orgs WHERE id = ?1", params![org_id])?;
        Ok(())
    }

    fn create_authz_entity(&self, entity_type: &str) -> Result<String, StoreError> {
        let authz_id = Uuid::new_v4().simple().to_string();
        let now = crate::identity::now();
        let connection = self.connection();
        connection.execute(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES (?1, ?2, NULL, ?3)",
            params![authz_id, entity_type, now],
        )?;
        Ok(authz_id)
    }

    pub fn add_group_member(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection();
        connection.execute(
            "INSERT OR IGNORE INTO authz_group_actors (group_authz_id, actor_authz_id)
             VALUES (?1, ?2)",
            params![group_authz_id, actor_authz_id],
        )?;
        Ok(())
    }

    pub fn add_group_child(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError> {
        self.ensure_acyclic(parent_group_authz_id, child_group_authz_id)?;

        let connection = self.connection();
        connection.execute(
            "INSERT OR IGNORE INTO authz_group_groups
             (parent_group_authz_id, child_group_authz_id)
             VALUES (?1, ?2)",
            params![parent_group_authz_id, child_group_authz_id],
        )?;
        Ok(())
    }

    pub fn ensure_acyclic(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError> {
        if parent_group_authz_id == child_group_authz_id {
            return Err(StoreError::Invalid("a group cannot contain itself".into()));
        }

        let connection = self.connection();
        let creates_cycle: i64 = connection.query_row(
            "WITH RECURSIVE descendants(authz_id) AS (
                 SELECT child_group_authz_id
                 FROM authz_group_groups
                 WHERE parent_group_authz_id = ?1
                 UNION
                 SELECT relation.child_group_authz_id
                 FROM authz_group_groups relation
                 JOIN descendants ON relation.parent_group_authz_id = descendants.authz_id
             )
             SELECT EXISTS (SELECT 1 FROM descendants WHERE authz_id = ?2)",
            params![child_group_authz_id, parent_group_authz_id],
            |row| row.get(0),
        )?;

        if creates_cycle != 0 {
            return Err(StoreError::Invalid("group cycle detected".into()));
        }
        Ok(())
    }

    pub fn set_acl(
        &self,
        target_authz_id: &str,
        permission: Permission,
        actor_authz_id: Option<&str>,
        group_authz_id: Option<&str>,
    ) -> Result<(), StoreError> {
        let connection = self.connection();
        if let Some(actor_authz_id) = actor_authz_id {
            connection.execute(
                "INSERT OR IGNORE INTO authz_acl_actor
                 (target_authz_id, permission, actor_authz_id)
                 VALUES (?1, ?2, ?3)",
                params![target_authz_id, permission.as_str(), actor_authz_id],
            )?;
        }
        if let Some(group_authz_id) = group_authz_id {
            connection.execute(
                "INSERT OR IGNORE INTO authz_acl_group
                 (target_authz_id, permission, group_authz_id)
                 VALUES (?1, ?2, ?3)",
                params![target_authz_id, permission.as_str(), group_authz_id],
            )?;
        }
        Ok(())
    }

    /// Read the full ACL of a target. Every permission is present, even when
    /// its ACE is empty. Actor names are ordered clients first, then users,
    /// matching the upstream API's non-granular representation.
    pub fn get_acl(&self, target_authz_id: &str) -> Result<Acl, StoreError> {
        Ok(self
            .get_acl_entries(target_authz_id)?
            .into_iter()
            .map(|entry| {
                let mut actors = entry.clients;
                actors.extend(entry.users);
                (entry.permission, actors, entry.groups)
            })
            .collect())
    }

    /// Read the full ACL of a target with users and clients kept separate.
    pub fn get_acl_entries(&self, target_authz_id: &str) -> Result<Vec<AclEntry>, StoreError> {
        let connection = self.connection();
        let exists: i64 = connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM authz_entities WHERE authz_id = ?1)",
            [target_authz_id],
            |row| row.get(0),
        )?;
        if exists == 0 {
            return Err(StoreError::NotFound(format!(
                "ACL target {target_authz_id}"
            )));
        }

        let mut entries = Vec::with_capacity(Permission::ALL.len());
        for permission in Permission::ALL {
            let users = connection
                .prepare(
                    "SELECT u.username
                     FROM authz_acl_actor ace
                     JOIN users u ON u.authz_id = ace.actor_authz_id
                     WHERE ace.target_authz_id = ?1 AND ace.permission = ?2
                     ORDER BY u.username",
                )?
                .query_map(params![target_authz_id, permission.as_str()], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let clients = connection
                .prepare(
                    "SELECT c.name
                     FROM authz_acl_actor ace
                     JOIN clients c ON c.authz_id = ace.actor_authz_id
                     WHERE ace.target_authz_id = ?1 AND ace.permission = ?2
                     ORDER BY c.name",
                )?
                .query_map(params![target_authz_id, permission.as_str()], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let groups = connection
                .prepare(
                    "SELECT g.name
                     FROM authz_acl_group ace
                     JOIN groups g ON g.authz_id = ace.group_authz_id
                     WHERE ace.target_authz_id = ?1 AND ace.permission = ?2
                     ORDER BY g.name",
                )?
                .query_map(params![target_authz_id, permission.as_str()], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            entries.push(AclEntry {
                permission,
                users,
                clients,
                groups,
            });
        }
        Ok(entries)
    }

    /// Replace the ACE for a single permission. Actor and group names are
    /// resolved in the target's organization; unknown names are rejected
    /// without changing the existing ACE.
    pub fn set_acl_for_permission(
        &self,
        target_authz_id: &str,
        permission: Permission,
        actors: &[String],
        groups: &[String],
    ) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let org_id: Option<String> = transaction
            .query_row(
                "SELECT org_id FROM authz_entities WHERE authz_id = ?1",
                [target_authz_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ACL target {target_authz_id}")))?;

        let mut actor_ids = Vec::with_capacity(actors.len());
        for name in actors {
            actor_ids.push(resolve_actor_authz_id(
                &transaction,
                org_id.as_deref(),
                name,
            )?);
        }

        let mut group_ids = Vec::with_capacity(groups.len());
        for name in groups {
            let Some(org_id) = org_id.as_deref() else {
                return Err(StoreError::Invalid(format!("group {name} does not exist")));
            };
            let group_id = transaction
                .query_row(
                    "SELECT authz_id FROM groups WHERE org_id = ?1 AND name = ?2",
                    params![org_id, name],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::Invalid(format!("group {name} does not exist in this organization"))
                })?;
            group_ids.push(group_id);
        }

        transaction.execute(
            "DELETE FROM authz_acl_actor
             WHERE target_authz_id = ?1 AND permission = ?2",
            params![target_authz_id, permission.as_str()],
        )?;
        transaction.execute(
            "DELETE FROM authz_acl_group
             WHERE target_authz_id = ?1 AND permission = ?2",
            params![target_authz_id, permission.as_str()],
        )?;
        for actor_id in actor_ids {
            transaction.execute(
                "INSERT OR IGNORE INTO authz_acl_actor
                 (target_authz_id, permission, actor_authz_id)
                 VALUES (?1, ?2, ?3)",
                params![target_authz_id, permission.as_str(), actor_id],
            )?;
        }
        for group_id in group_ids {
            transaction.execute(
                "INSERT OR IGNORE INTO authz_acl_group
                 (target_authz_id, permission, group_authz_id)
                 VALUES (?1, ?2, ?3)",
                params![target_authz_id, permission.as_str(), group_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn check_permission(
        &self,
        actor_authz_id: &str,
        target_authz_id: &str,
        permission: Permission,
    ) -> Result<bool, StoreError> {
        let connection = self.connection();
        let allowed: i64 = connection.query_row(
            "WITH RECURSIVE actor_groups(group_authz_id) AS (
                 SELECT group_authz_id
                 FROM authz_group_actors
                 WHERE actor_authz_id = ?1
                 UNION
                 SELECT relation.parent_group_authz_id
                 FROM authz_group_groups relation
                 JOIN actor_groups ON relation.child_group_authz_id = actor_groups.group_authz_id
             )
             SELECT EXISTS (
                 SELECT 1
                 FROM authz_acl_actor
                 WHERE target_authz_id = ?2
                   AND actor_authz_id = ?1
                   AND permission = ?3
                 UNION ALL
                 SELECT 1
                 FROM authz_acl_group
                 WHERE target_authz_id = ?2
                   AND permission = ?3
                   AND group_authz_id IN (SELECT group_authz_id FROM actor_groups)
             )",
            params![actor_authz_id, target_authz_id, permission.as_str()],
            |row| row.get(0),
        )?;
        Ok(allowed != 0)
    }

    /// Resolve an ACL URL object type and name to its authz id.
    ///
    /// Supported types mirror the `/_acl` route table: clients, groups,
    /// containers, data (data bags), nodes, roles, environments, cookbooks,
    /// policies, policy_groups, and cookbook_artifacts.
    pub fn find_acl_target(
        &self,
        org_id: &str,
        object_type: &str,
        name: &str,
    ) -> Result<Option<String>, StoreError> {
        let table = match object_type {
            "clients" => "clients",
            "groups" => "groups",
            "containers" => "containers",
            "data" => "data_bags",
            "nodes" => "nodes",
            "roles" => "roles",
            "environments" => "environments",
            "cookbooks" => "cookbooks",
            "policies" => "policies",
            "policy_groups" => "policy_groups",
            "cookbook_artifacts" => "cookbook_artifacts",
            _ => return Ok(None),
        };
        let connection = self.connection();
        let sql = format!("SELECT authz_id FROM {table} WHERE org_id = ?1 AND name = ?2");
        Ok(connection
            .query_row(&sql, params![org_id, name], |row| row.get::<_, String>(0))
            .optional()?)
    }
}

fn resolve_actor_authz_id(
    transaction: &Transaction<'_>,
    org_id: Option<&str>,
    name: &str,
) -> Result<String, StoreError> {
    let user_id: Option<String> = transaction
        .query_row(
            "SELECT authz_id FROM users WHERE username = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()?;

    let client_id = match org_id {
        Some(org_id) => transaction
            .query_row(
                "SELECT authz_id FROM clients WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| row.get::<_, String>(0),
            )
            .optional()?,
        None => None,
    };

    match (user_id, client_id) {
        (Some(_), Some(_)) => Err(StoreError::Invalid(format!(
            "actor {name} exists as both a client and a user in this organization"
        ))),
        (Some(user_id), None) => Ok(user_id),
        (None, Some(client_id)) => Ok(client_id),
        (None, None) => Err(StoreError::Invalid(match org_id {
            Some(_) => format!("actor {name} does not exist in this organization"),
            None => format!("actor {name} does not exist"),
        })),
    }
}
