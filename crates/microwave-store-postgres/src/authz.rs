use async_trait::async_trait;
use microwave_store::{
    Acl, AclEntry, AuthzRepository, Container, Group, GroupMembers, Permission, StoreError,
};
use sqlx::postgres::Postgres;
use sqlx::{Row, Transaction};

use crate::store::{db, new_id, now, unique, PostgresStore};

impl PostgresStore {
    async fn create_authz_entity(&self, entity_type: &str) -> Result<String, StoreError> {
        let authz_id = new_id();
        let timestamp = now();
        sqlx::query(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES ($1, $2, NULL, $3)",
        )
        .bind(&authz_id)
        .bind(entity_type)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(authz_id)
    }
}

#[async_trait]
#[allow(clippy::too_many_arguments)]
impl AuthzRepository for PostgresStore {
    async fn create_actor(&self) -> Result<String, StoreError> {
        self.create_authz_entity("actor").await
    }

    async fn create_object(&self) -> Result<String, StoreError> {
        self.create_authz_entity("object").await
    }

    async fn create_group(&self, _name: &str) -> Result<String, StoreError> {
        self.create_authz_entity("group").await
    }

    async fn create_container(&self) -> Result<String, StoreError> {
        self.create_authz_entity("container").await
    }

    async fn create_container_for_org(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Container, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        sqlx::query(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES ($1, 'container', $2, $3)",
        )
        .bind(&authz_id)
        .bind(org_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        sqlx::query(
            "INSERT INTO containers (id, org_id, authz_id, name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $5)",
        )
        .bind(&id)
        .bind(org_id)
        .bind(&authz_id)
        .bind(name)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, format!("container {name} already exists")))?;
        transaction.commit().await.map_err(db)?;
        Ok(Container {
            id,
            authz_id,
            name: name.into(),
        })
    }

    async fn create_container_for_org_with_acl(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<Container, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        sqlx::query(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES ($1, 'container', $2, $3)",
        )
        .bind(&authz_id)
        .bind(org_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        for permission in Permission::ALL {
            sqlx::query(
                "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
                 VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(&authz_id)
            .bind(permission.as_str())
            .bind(creator_authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        sqlx::query(
            "INSERT INTO containers (id, org_id, authz_id, name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $5)",
        )
        .bind(&id)
        .bind(org_id)
        .bind(&authz_id)
        .bind(name)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, format!("container {name} already exists")))?;
        transaction.commit().await.map_err(db)?;
        Ok(Container {
            id,
            authz_id,
            name: name.into(),
        })
    }

    async fn create_group_for_org(&self, org_id: &str, name: &str) -> Result<Group, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        sqlx::query(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES ($1, 'group', $2, $3)",
        )
        .bind(&authz_id)
        .bind(org_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        sqlx::query(
            "INSERT INTO groups (id, org_id, authz_id, name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $5)",
        )
        .bind(&id)
        .bind(org_id)
        .bind(&authz_id)
        .bind(name)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, format!("group {name} already exists")))?;
        transaction.commit().await.map_err(db)?;
        Ok(Group {
            id,
            authz_id,
            name: name.into(),
        })
    }

    async fn create_group_for_org_with_acl(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<Group, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let container_authz_id: String = sqlx::query_scalar(
            "SELECT authz_id FROM containers WHERE org_id = $1 AND name = 'groups'",
        )
        .bind(org_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or_else(|| StoreError::NotFound("container groups".into()))?;

        sqlx::query(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES ($1, 'group', $2, $3)",
        )
        .bind(&authz_id)
        .bind(org_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        sqlx::query(
            "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
             SELECT $1, permission, actor_authz_id FROM authz_acl_actor
             WHERE target_authz_id = $2
             ON CONFLICT DO NOTHING",
        )
        .bind(&authz_id)
        .bind(&container_authz_id)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        sqlx::query(
            "INSERT INTO authz_acl_group (target_authz_id, permission, group_authz_id)
             SELECT $1, permission, group_authz_id FROM authz_acl_group
             WHERE target_authz_id = $2
             ON CONFLICT DO NOTHING",
        )
        .bind(&authz_id)
        .bind(&container_authz_id)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        for permission in Permission::ALL {
            sqlx::query(
                "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
                 VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(&authz_id)
            .bind(permission.as_str())
            .bind(creator_authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        sqlx::query(
            "INSERT INTO groups (id, org_id, authz_id, name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $5)",
        )
        .bind(&id)
        .bind(org_id)
        .bind(&authz_id)
        .bind(name)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, format!("group {name} already exists")))?;
        transaction.commit().await.map_err(db)?;
        Ok(Group {
            id,
            authz_id,
            name: name.into(),
        })
    }

    async fn find_group(&self, org_id: &str, name: &str) -> Result<Option<Group>, StoreError> {
        let row =
            sqlx::query("SELECT id, authz_id, name FROM groups WHERE org_id = $1 AND name = $2")
                .bind(org_id)
                .bind(name)
                .fetch_optional(self.pool())
                .await
                .map_err(db)?;
        row.map(|row| {
            Ok(Group {
                id: row.try_get(0)?,
                authz_id: row.try_get(1)?,
                name: row.try_get(2)?,
            })
        })
        .transpose()
        .map_err(db)
    }

    async fn find_container(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<Container>, StoreError> {
        let row = sqlx::query(
            "SELECT id, authz_id, name FROM containers WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| {
            Ok(Container {
                id: row.try_get(0)?,
                authz_id: row.try_get(1)?,
                name: row.try_get(2)?,
            })
        })
        .transpose()
        .map_err(db)
    }

    async fn list_groups(&self, org_id: &str) -> Result<Vec<Group>, StoreError> {
        let rows =
            sqlx::query("SELECT id, authz_id, name FROM groups WHERE org_id = $1 ORDER BY name")
                .bind(org_id)
                .fetch_all(self.pool())
                .await
                .map_err(db)?;
        rows.iter()
            .map(|row| {
                Ok(Group {
                    id: row.try_get(0)?,
                    authz_id: row.try_get(1)?,
                    name: row.try_get(2)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn list_containers(&self, org_id: &str) -> Result<Vec<Container>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, authz_id, name FROM containers WHERE org_id = $1 ORDER BY name",
        )
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(|row| {
                Ok(Container {
                    id: row.try_get(0)?,
                    authz_id: row.try_get(1)?,
                    name: row.try_get(2)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn rename_group(
        &self,
        org_id: &str,
        name: &str,
        new_name: &str,
    ) -> Result<Group, StoreError> {
        let timestamp = now();
        let result = sqlx::query(
            "UPDATE groups SET name = $3, updated_at = $4 WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .bind(new_name)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(|error| unique(error, format!("group {new_name} already exists")))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("group {name}")));
        }
        self.find_group(org_id, new_name)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("group {new_name}")))
    }

    async fn delete_group(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let group = self
            .find_group(org_id, name)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("group {name}")))?;
        sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
            .bind(&group.authz_id)
            .execute(self.pool())
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn delete_container(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let container = self
            .find_container(org_id, name)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("container {name}")))?;
        sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
            .bind(&container.authz_id)
            .execute(self.pool())
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn list_group_members(
        &self,
        org_id: &str,
        group_authz_id: &str,
    ) -> Result<GroupMembers, StoreError> {
        let users: Vec<String> = sqlx::query_scalar(
            "SELECT u.username
             FROM authz_group_actors member
             JOIN users u ON u.authz_id = member.actor_authz_id
             WHERE member.group_authz_id = $1
             ORDER BY u.username",
        )
        .bind(group_authz_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        let clients: Vec<String> = sqlx::query_scalar(
            "SELECT c.name
             FROM authz_group_actors member
             JOIN clients c ON c.authz_id = member.actor_authz_id
             WHERE member.group_authz_id = $1 AND c.org_id = $2
             ORDER BY c.name",
        )
        .bind(group_authz_id)
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        let groups: Vec<String> = sqlx::query_scalar(
            "SELECT g.name
             FROM authz_group_groups relation
             JOIN groups g ON g.authz_id = relation.child_group_authz_id
             WHERE relation.parent_group_authz_id = $1 AND g.org_id = $2
             ORDER BY g.name",
        )
        .bind(group_authz_id)
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        Ok(GroupMembers {
            users,
            clients,
            groups,
        })
    }

    async fn replace_group_members(
        &self,
        org_id: &str,
        group_authz_id: &str,
        users: Option<&[String]>,
        clients: Option<&[String]>,
        groups: Option<&[String]>,
    ) -> Result<(), StoreError> {
        let current = self.list_group_members(org_id, group_authz_id).await?;
        let users = users.unwrap_or(&current.users);
        let clients = clients.unwrap_or(&current.clients);
        let groups = groups.unwrap_or(&current.groups);

        let mut child_group_ids = Vec::with_capacity(groups.len());
        for name in groups {
            let child: Option<String> =
                sqlx::query_scalar("SELECT authz_id FROM groups WHERE org_id = $1 AND name = $2")
                    .bind(org_id)
                    .bind(name)
                    .fetch_optional(self.pool())
                    .await
                    .map_err(db)?;
            let child = child.ok_or_else(|| {
                StoreError::Invalid(format!("group {name} does not exist in this organization"))
            })?;
            if child == group_authz_id {
                return Err(StoreError::Invalid("a group cannot contain itself".into()));
            }
            let creates_cycle: bool = sqlx::query_scalar(
                "WITH RECURSIVE descendants(authz_id) AS (
                     SELECT child_group_authz_id
                     FROM authz_group_groups
                     WHERE parent_group_authz_id = $1
                     UNION
                     SELECT relation.child_group_authz_id
                     FROM authz_group_groups relation
                     JOIN descendants ON relation.parent_group_authz_id = descendants.authz_id
                 )
                 SELECT EXISTS (SELECT 1 FROM descendants WHERE authz_id = $2)",
            )
            .bind(&child)
            .bind(group_authz_id)
            .fetch_one(self.pool())
            .await
            .map_err(db)?;
            if creates_cycle {
                return Err(StoreError::Invalid("group cycle detected".into()));
            }
            child_group_ids.push(child);
        }

        let mut actor_ids = Vec::with_capacity(users.len() + clients.len());
        for name in users {
            let id: Option<String> =
                sqlx::query_scalar("SELECT authz_id FROM users WHERE username = $1")
                    .bind(name)
                    .fetch_optional(self.pool())
                    .await
                    .map_err(db)?;
            actor_ids.push(
                id.ok_or_else(|| StoreError::Invalid(format!("user {name} does not exist")))?,
            );
        }
        for name in clients {
            let id: Option<String> =
                sqlx::query_scalar("SELECT authz_id FROM clients WHERE org_id = $1 AND name = $2")
                    .bind(org_id)
                    .bind(name)
                    .fetch_optional(self.pool())
                    .await
                    .map_err(db)?;
            actor_ids.push(
                id.ok_or_else(|| StoreError::Invalid(format!("client {name} does not exist")))?,
            );
        }

        let mut transaction = self.pool().begin().await.map_err(db)?;
        sqlx::query("DELETE FROM authz_group_actors WHERE group_authz_id = $1")
            .bind(group_authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM authz_group_groups WHERE parent_group_authz_id = $1")
            .bind(group_authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        for actor_id in actor_ids {
            sqlx::query(
                "INSERT INTO authz_group_actors (group_authz_id, actor_authz_id)
                 VALUES ($1, $2)
                 ON CONFLICT DO NOTHING",
            )
            .bind(group_authz_id)
            .bind(&actor_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        for child_id in child_group_ids {
            sqlx::query(
                "INSERT INTO authz_group_groups (parent_group_authz_id, child_group_authz_id)
                 VALUES ($1, $2)
                 ON CONFLICT DO NOTHING",
            )
            .bind(group_authz_id)
            .bind(&child_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn add_group_member(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO authz_group_actors (group_authz_id, actor_authz_id)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(group_authz_id)
        .bind(actor_authz_id)
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn remove_group_member(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "DELETE FROM authz_group_actors
             WHERE group_authz_id = $1 AND actor_authz_id = $2",
        )
        .bind(group_authz_id)
        .bind(actor_authz_id)
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn add_group_child(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError> {
        self.ensure_acyclic(parent_group_authz_id, child_group_authz_id)
            .await?;
        sqlx::query(
            "INSERT INTO authz_group_groups (parent_group_authz_id, child_group_authz_id)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(parent_group_authz_id)
        .bind(child_group_authz_id)
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn ensure_acyclic(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError> {
        if parent_group_authz_id == child_group_authz_id {
            return Err(StoreError::Invalid("a group cannot contain itself".into()));
        }
        let creates_cycle: bool = sqlx::query_scalar(
            "WITH RECURSIVE descendants(authz_id) AS (
                 SELECT child_group_authz_id
                 FROM authz_group_groups
                 WHERE parent_group_authz_id = $1
                 UNION
                 SELECT relation.child_group_authz_id
                 FROM authz_group_groups relation
                 JOIN descendants ON relation.parent_group_authz_id = descendants.authz_id
             )
             SELECT EXISTS (SELECT 1 FROM descendants WHERE authz_id = $2)",
        )
        .bind(child_group_authz_id)
        .bind(parent_group_authz_id)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        if creates_cycle {
            return Err(StoreError::Invalid("group cycle detected".into()));
        }
        Ok(())
    }

    async fn set_acl(
        &self,
        target_authz_id: &str,
        permission: Permission,
        actor_authz_id: Option<&str>,
        group_authz_id: Option<&str>,
    ) -> Result<(), StoreError> {
        if let Some(actor_authz_id) = actor_authz_id {
            sqlx::query(
                "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
                 VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(target_authz_id)
            .bind(permission.as_str())
            .bind(actor_authz_id)
            .execute(self.pool())
            .await
            .map_err(db)?;
        }
        if let Some(group_authz_id) = group_authz_id {
            sqlx::query(
                "INSERT INTO authz_acl_group (target_authz_id, permission, group_authz_id)
                 VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(target_authz_id)
            .bind(permission.as_str())
            .bind(group_authz_id)
            .execute(self.pool())
            .await
            .map_err(db)?;
        }
        Ok(())
    }

    async fn get_acl(&self, target_authz_id: &str) -> Result<Acl, StoreError> {
        Ok(self
            .get_acl_entries(target_authz_id)
            .await?
            .into_iter()
            .map(|entry| {
                let mut actors = entry.clients;
                actors.extend(entry.users);
                (entry.permission, actors, entry.groups)
            })
            .collect())
    }

    async fn get_acl_entries(&self, target_authz_id: &str) -> Result<Vec<AclEntry>, StoreError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM authz_entities WHERE authz_id = $1)")
                .bind(target_authz_id)
                .fetch_one(self.pool())
                .await
                .map_err(db)?;
        if !exists {
            return Err(StoreError::NotFound(format!(
                "ACL target {target_authz_id}"
            )));
        }

        let mut entries = Vec::with_capacity(Permission::ALL.len());
        for permission in Permission::ALL {
            let users: Vec<String> = sqlx::query_scalar(
                "SELECT u.username
                 FROM authz_acl_actor ace
                 JOIN users u ON u.authz_id = ace.actor_authz_id
                 WHERE ace.target_authz_id = $1 AND ace.permission = $2
                 ORDER BY u.username",
            )
            .bind(target_authz_id)
            .bind(permission.as_str())
            .fetch_all(self.pool())
            .await
            .map_err(db)?;
            let clients: Vec<String> = sqlx::query_scalar(
                "SELECT c.name
                 FROM authz_acl_actor ace
                 JOIN clients c ON c.authz_id = ace.actor_authz_id
                 WHERE ace.target_authz_id = $1 AND ace.permission = $2
                 ORDER BY c.name",
            )
            .bind(target_authz_id)
            .bind(permission.as_str())
            .fetch_all(self.pool())
            .await
            .map_err(db)?;
            let groups: Vec<String> = sqlx::query_scalar(
                "SELECT g.name
                 FROM authz_acl_group ace
                 JOIN groups g ON g.authz_id = ace.group_authz_id
                 WHERE ace.target_authz_id = $1 AND ace.permission = $2
                 ORDER BY g.name",
            )
            .bind(target_authz_id)
            .bind(permission.as_str())
            .fetch_all(self.pool())
            .await
            .map_err(db)?;
            entries.push(AclEntry {
                permission,
                users,
                clients,
                groups,
            });
        }
        Ok(entries)
    }

    async fn set_acl_for_permission(
        &self,
        target_authz_id: &str,
        permission: Permission,
        actors: &[String],
        groups: &[String],
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let org_id: Option<String> =
            sqlx::query_scalar("SELECT org_id FROM authz_entities WHERE authz_id = $1")
                .bind(target_authz_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(db)?
                .ok_or_else(|| StoreError::NotFound(format!("ACL target {target_authz_id}")))?;

        let mut actor_ids = Vec::with_capacity(actors.len());
        for name in actors {
            actor_ids
                .push(resolve_actor_authz_id(&mut transaction, org_id.as_deref(), name).await?);
        }

        let mut group_ids = Vec::with_capacity(groups.len());
        for name in groups {
            let Some(org_id) = org_id.as_deref() else {
                return Err(StoreError::Invalid(format!("group {name} does not exist")));
            };
            let group_id: Option<String> =
                sqlx::query_scalar("SELECT authz_id FROM groups WHERE org_id = $1 AND name = $2")
                    .bind(org_id)
                    .bind(name)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(db)?;
            group_ids.push(group_id.ok_or_else(|| {
                StoreError::Invalid(format!("group {name} does not exist in this organization"))
            })?);
        }

        sqlx::query("DELETE FROM authz_acl_actor WHERE target_authz_id = $1 AND permission = $2")
            .bind(target_authz_id)
            .bind(permission.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM authz_acl_group WHERE target_authz_id = $1 AND permission = $2")
            .bind(target_authz_id)
            .bind(permission.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        for actor_id in actor_ids {
            sqlx::query(
                "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
                 VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(target_authz_id)
            .bind(permission.as_str())
            .bind(&actor_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        for group_id in group_ids {
            sqlx::query(
                "INSERT INTO authz_acl_group (target_authz_id, permission, group_authz_id)
                 VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(target_authz_id)
            .bind(permission.as_str())
            .bind(&group_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn check_permission(
        &self,
        actor_authz_id: &str,
        target_authz_id: &str,
        permission: Permission,
    ) -> Result<bool, StoreError> {
        let allowed: bool = sqlx::query_scalar(
            "WITH RECURSIVE actor_groups(group_authz_id) AS (
                 SELECT group_authz_id
                 FROM authz_group_actors
                 WHERE actor_authz_id = $1
                 UNION
                 SELECT relation.parent_group_authz_id
                 FROM authz_group_groups relation
                 JOIN actor_groups ON relation.child_group_authz_id = actor_groups.group_authz_id
             )
             SELECT EXISTS (
                 SELECT 1
                 FROM authz_acl_actor
                 WHERE target_authz_id = $2
                   AND actor_authz_id = $1
                   AND permission = $3
                 UNION ALL
                 SELECT 1
                 FROM authz_acl_group
                 WHERE target_authz_id = $2
                   AND permission = $3
                   AND group_authz_id IN (SELECT group_authz_id FROM actor_groups)
             )",
        )
        .bind(actor_authz_id)
        .bind(target_authz_id)
        .bind(permission.as_str())
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        Ok(allowed)
    }

    async fn find_acl_target(
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
        let sql = format!("SELECT authz_id FROM {table} WHERE org_id = $1 AND name = $2");
        sqlx::query_scalar(&sql)
            .bind(org_id)
            .bind(name)
            .fetch_optional(self.pool())
            .await
            .map_err(db)
    }

    async fn associate_user_org(&self, org_id: &str, user_id: &str) -> Result<(), StoreError> {
        let timestamp = now();
        sqlx::query(
            "INSERT INTO org_user_associations
             (org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, '', $3, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(org_id)
        .bind(user_id)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn delete_org(&self, org_id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM orgs WHERE id = $1")
            .bind(org_id)
            .execute(self.pool())
            .await
            .map_err(db)?;
        Ok(())
    }
}

async fn resolve_actor_authz_id(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: Option<&str>,
    name: &str,
) -> Result<String, StoreError> {
    let user_id: Option<String> =
        sqlx::query_scalar("SELECT authz_id FROM users WHERE username = $1")
            .bind(name)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(db)?;

    let client_id = match org_id {
        Some(org_id) => {
            sqlx::query_scalar("SELECT authz_id FROM clients WHERE org_id = $1 AND name = $2")
                .bind(org_id)
                .bind(name)
                .fetch_optional(&mut **transaction)
                .await
                .map_err(db)?
        }
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
