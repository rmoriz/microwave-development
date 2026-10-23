use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::params;
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

    pub(crate) fn as_str(self) -> &'static str {
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
}
