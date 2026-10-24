use async_trait::async_trait;
use microwave_store::{
    NewNode, ObjectRepository, Permission, StoreError, StoredDataBagItem, StoredNode, StoredObject,
};
use sqlx::postgres::{PgRow, Postgres};
use sqlx::{Row, Transaction};

use crate::store::{db, new_id, now, unique, PostgresStore};

impl PostgresStore {
    async fn create_named_object(
        &self,
        org_id: &str,
        container_name: &str,
        table: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        let id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let authz_id =
            create_object_authz(&mut transaction, org_id, container_name, creator_authz_id).await?;
        let sql = format!(
            "INSERT INTO {table}
             (id, authz_id, org_id, name, serialized_object, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(&id)
            .bind(&authz_id)
            .bind(org_id)
            .bind(name)
            .bind(body)
            .bind(creator_authz_id)
            .bind(&timestamp)
            .execute(&mut *transaction)
            .await
            .map_err(|error| unique(error, format!("{table} {name} already exists")))?;
        transaction.commit().await.map_err(db)?;
        Ok(StoredObject {
            id,
            authz_id,
            name: name.into(),
            serialized_object: body.into(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
    }

    async fn find_named_object(
        &self,
        table: &str,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        let sql = format!(
            "SELECT id, authz_id, name, serialized_object, created_at, updated_at
             FROM {table} WHERE org_id = $1 AND name = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(org_id)
            .bind(name)
            .fetch_optional(self.pool())
            .await
            .map_err(db)?;
        row.map(|row| stored_object_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn list_named_objects(
        &self,
        table: &str,
        org_id: &str,
    ) -> Result<Vec<StoredObject>, StoreError> {
        let sql = format!(
            "SELECT id, authz_id, name, serialized_object, created_at, updated_at
             FROM {table} WHERE org_id = $1 ORDER BY name"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(org_id)
            .fetch_all(self.pool())
            .await
            .map_err(db)?;
        rows.iter()
            .map(stored_object_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn update_named_object(
        &self,
        table: &str,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError> {
        let timestamp = now();
        let sql = format!(
            "UPDATE {table} SET serialized_object = $3, updated_at = $4
             WHERE org_id = $1 AND name = $2"
        );
        let result = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(org_id)
            .bind(name)
            .bind(body)
            .bind(&timestamp)
            .execute(self.pool())
            .await
            .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("{table} {name}")));
        }
        self.find_named_object(table, org_id, name)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("{table} {name}")))
    }

    async fn delete_named_object(
        &self,
        table: &str,
        org_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let sql = format!("SELECT authz_id FROM {table} WHERE org_id = $1 AND name = $2");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(org_id)
            .bind(name)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(db)?
            .ok_or_else(|| StoreError::NotFound(format!("{table} {name}")))?;
        let authz_id: String = row.try_get(0).map_err(db)?;
        sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
            .bind(&authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }
}

#[async_trait]
impl ObjectRepository for PostgresStore {
    async fn create_node(&self, org_id: &str, node: NewNode<'_>) -> Result<StoredNode, StoreError> {
        let NewNode {
            name,
            body,
            environment,
            policy_name,
            policy_group,
            creator_authz_id,
        } = node;
        let id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let authz_id =
            create_object_authz(&mut transaction, org_id, "nodes", creator_authz_id).await?;
        sqlx::query(
            "INSERT INTO nodes
             (id, authz_id, org_id, name, environment, policy_name, policy_group,
              serialized_object, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
        )
        .bind(&id)
        .bind(&authz_id)
        .bind(org_id)
        .bind(name)
        .bind(environment)
        .bind(policy_name)
        .bind(policy_group)
        .bind(body)
        .bind(creator_authz_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, "Node already exists".to_string()))?;
        transaction.commit().await.map_err(db)?;
        Ok(StoredNode {
            id,
            authz_id,
            name: name.into(),
            environment: environment.into(),
            policy_name: policy_name.map(ToString::to_string),
            policy_group: policy_group.map(ToString::to_string),
            serialized_object: body.into(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
    }

    async fn find_node(&self, org_id: &str, name: &str) -> Result<Option<StoredNode>, StoreError> {
        let row = sqlx::query(
            "SELECT id, authz_id, name, environment, policy_name, policy_group,
                    serialized_object, created_at, updated_at
             FROM nodes WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| node_from_row(&row)).transpose().map_err(db)
    }

    async fn list_nodes(&self, org_id: &str) -> Result<Vec<StoredNode>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, authz_id, name, environment, policy_name, policy_group,
                    serialized_object, created_at, updated_at
             FROM nodes WHERE org_id = $1 ORDER BY name",
        )
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(node_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn list_nodes_in_environment(
        &self,
        org_id: &str,
        environment: &str,
    ) -> Result<Vec<StoredNode>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, authz_id, name, environment, policy_name, policy_group,
                    serialized_object, created_at, updated_at
             FROM nodes WHERE org_id = $1 AND environment = $2 ORDER BY name",
        )
        .bind(org_id)
        .bind(environment)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(node_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn update_node(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        environment: &str,
        policy_name: Option<&str>,
        policy_group: Option<&str>,
    ) -> Result<StoredNode, StoreError> {
        let timestamp = now();
        let result = sqlx::query(
            "UPDATE nodes
             SET serialized_object = $3, environment = $4, policy_name = $5,
                 policy_group = $6, updated_at = $7
             WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .bind(body)
        .bind(environment)
        .bind(policy_name)
        .bind(policy_group)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("node {name}")));
        }
        self.find_node(org_id, name)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("node {name}")))
    }

    async fn delete_node(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let row = sqlx::query("SELECT authz_id FROM nodes WHERE org_id = $1 AND name = $2")
            .bind(org_id)
            .bind(name)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(db)?
            .ok_or_else(|| StoreError::NotFound(format!("node {name}")))?;
        let authz_id: String = row.try_get(0).map_err(db)?;
        sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
            .bind(&authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn create_environment(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        self.create_named_object(
            org_id,
            "environments",
            "environments",
            name,
            body,
            creator_authz_id,
        )
        .await
    }

    async fn find_environment(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        self.find_named_object("environments", org_id, name).await
    }

    async fn list_environments(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError> {
        self.list_named_objects("environments", org_id).await
    }

    async fn update_environment(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError> {
        self.update_named_object("environments", org_id, name, body)
            .await
    }

    async fn delete_environment(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_named_object("environments", org_id, name).await
    }

    async fn create_role(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        self.create_named_object(org_id, "roles", "roles", name, body, creator_authz_id)
            .await
    }

    async fn find_role(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        self.find_named_object("roles", org_id, name).await
    }

    async fn list_roles(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError> {
        self.list_named_objects("roles", org_id).await
    }

    async fn update_role(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError> {
        self.update_named_object("roles", org_id, name, body).await
    }

    async fn delete_role(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_named_object("roles", org_id, name).await
    }

    async fn create_data_bag(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        self.create_named_object(org_id, "data", "data_bags", name, "{}", creator_authz_id)
            .await
    }

    async fn find_data_bag(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        self.find_named_object("data_bags", org_id, name).await
    }

    async fn list_data_bags(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError> {
        self.list_named_objects("data_bags", org_id).await
    }

    async fn delete_data_bag(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_named_object("data_bags", org_id, name).await
    }

    async fn create_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
        body: &str,
    ) -> Result<StoredDataBagItem, StoreError> {
        let id = new_id();
        let timestamp = now();
        sqlx::query(
            "INSERT INTO data_bag_items
             (id, org_id, data_bag_name, item_name, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, '', $6, $6)",
        )
        .bind(&id)
        .bind(org_id)
        .bind(data_bag_name)
        .bind(item_name)
        .bind(body)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(|error| {
            unique(
                error,
                format!("data bag item {data_bag_name}/{item_name} already exists"),
            )
        })?;
        Ok(StoredDataBagItem {
            id,
            data_bag_name: data_bag_name.into(),
            item_name: item_name.into(),
            serialized_object: body.into(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
    }

    async fn find_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
    ) -> Result<Option<StoredDataBagItem>, StoreError> {
        let row = sqlx::query(
            "SELECT id, data_bag_name, item_name, serialized_object, created_at, updated_at
             FROM data_bag_items
             WHERE org_id = $1 AND data_bag_name = $2 AND item_name = $3",
        )
        .bind(org_id)
        .bind(data_bag_name)
        .bind(item_name)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| data_bag_item_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn list_data_bag_items(
        &self,
        org_id: &str,
        data_bag_name: &str,
    ) -> Result<Vec<StoredDataBagItem>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, data_bag_name, item_name, serialized_object, created_at, updated_at
             FROM data_bag_items
             WHERE org_id = $1 AND data_bag_name = $2 ORDER BY item_name",
        )
        .bind(org_id)
        .bind(data_bag_name)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(data_bag_item_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn update_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
        body: &str,
    ) -> Result<StoredDataBagItem, StoreError> {
        let timestamp = now();
        let result = sqlx::query(
            "UPDATE data_bag_items
             SET serialized_object = $4, updated_at = $5
             WHERE org_id = $1 AND data_bag_name = $2 AND item_name = $3",
        )
        .bind(org_id)
        .bind(data_bag_name)
        .bind(item_name)
        .bind(body)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!(
                "data bag item {data_bag_name}/{item_name}"
            )));
        }
        self.find_data_bag_item(org_id, data_bag_name, item_name)
            .await?
            .ok_or_else(|| {
                StoreError::NotFound(format!("data bag item {data_bag_name}/{item_name}"))
            })
    }

    async fn delete_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "DELETE FROM data_bag_items
             WHERE org_id = $1 AND data_bag_name = $2 AND item_name = $3",
        )
        .bind(org_id)
        .bind(data_bag_name)
        .bind(item_name)
        .execute(self.pool())
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!(
                "data bag item {data_bag_name}/{item_name}"
            )));
        }
        Ok(())
    }
}

pub(crate) async fn create_object_authz(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    container_name: &str,
    creator_authz_id: &str,
) -> Result<String, StoreError> {
    let authz_id = create_acl_object(transaction, org_id, container_name).await?;
    grant_actor_all(transaction, &authz_id, creator_authz_id).await?;
    Ok(authz_id)
}

/// Create an authz object that inherits the ACL of `container_name` without
/// granting any additional actor.
pub(crate) async fn create_acl_object(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    container_name: &str,
) -> Result<String, StoreError> {
    let row = sqlx::query("SELECT authz_id FROM containers WHERE org_id = $1 AND name = $2")
        .bind(org_id)
        .bind(container_name)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(db)?
        .ok_or_else(|| StoreError::NotFound(format!("container {container_name}")))?;
    let container_authz_id: String = row.try_get(0).map_err(db)?;

    let authz_id = new_id();
    sqlx::query(
        "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
         VALUES ($1, 'object', $2, $3)",
    )
    .bind(&authz_id)
    .bind(org_id)
    .bind(now())
    .execute(&mut **transaction)
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
    .execute(&mut **transaction)
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
    .execute(&mut **transaction)
    .await
    .map_err(db)?;

    Ok(authz_id)
}

pub(crate) async fn grant_actor_all(
    transaction: &mut Transaction<'_, Postgres>,
    target_authz_id: &str,
    actor_authz_id: &str,
) -> Result<(), StoreError> {
    for permission in Permission::ALL {
        sqlx::query(
            "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
             VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(target_authz_id)
        .bind(permission.as_str())
        .bind(actor_authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

fn node_from_row(row: &PgRow) -> Result<StoredNode, sqlx::Error> {
    Ok(StoredNode {
        id: row.try_get(0)?,
        authz_id: row.try_get(1)?,
        name: row.try_get(2)?,
        environment: row.try_get(3)?,
        policy_name: row.try_get(4)?,
        policy_group: row.try_get(5)?,
        serialized_object: row.try_get(6)?,
        created_at: row.try_get(7)?,
        updated_at: row.try_get(8)?,
    })
}

fn stored_object_from_row(row: &PgRow) -> Result<StoredObject, sqlx::Error> {
    Ok(StoredObject {
        id: row.try_get(0)?,
        authz_id: row.try_get(1)?,
        name: row.try_get(2)?,
        serialized_object: row.try_get(3)?,
        created_at: row.try_get(4)?,
        updated_at: row.try_get(5)?,
    })
}

fn data_bag_item_from_row(row: &PgRow) -> Result<StoredDataBagItem, sqlx::Error> {
    Ok(StoredDataBagItem {
        id: row.try_get(0)?,
        data_bag_name: row.try_get(1)?,
        item_name: row.try_get(2)?,
        serialized_object: row.try_get(3)?,
        created_at: row.try_get(4)?,
        updated_at: row.try_get(5)?,
    })
}
