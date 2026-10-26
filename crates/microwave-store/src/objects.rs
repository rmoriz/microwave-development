use crate::authz::Permission;
use crate::compression::{decode_serialized, encode_serialized};
use crate::identity::{new_id, now};
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension, Transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredNode {
    pub id: String,
    pub authz_id: String,
    pub name: String,
    pub environment: String,
    pub policy_name: Option<String>,
    pub policy_group: Option<String>,
    pub serialized_object: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub id: String,
    pub authz_id: String,
    pub name: String,
    pub serialized_object: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDataBagItem {
    pub id: String,
    pub data_bag_name: String,
    pub item_name: String,
    pub serialized_object: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy)]
pub struct NewNode<'a> {
    pub name: &'a str,
    pub body: &'a str,
    pub environment: &'a str,
    pub policy_name: Option<&'a str>,
    pub policy_group: Option<&'a str>,
    pub creator_authz_id: &'a str,
}

impl SqliteStore {
    pub fn create_node(&self, org_id: &str, node: NewNode<'_>) -> Result<StoredNode, StoreError> {
        let NewNode {
            name,
            body,
            environment,
            policy_name,
            policy_group,
            creator_authz_id,
        } = node;
        let id = new_id();
        let now = now();
        let serialized_object = encode_serialized(body, self.compression())?;
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let authz_id = create_object_authz(&tx, org_id, "nodes", creator_authz_id)?;
        tx.execute(
            "INSERT INTO nodes
             (id, authz_id, org_id, name, environment, policy_name, policy_group,
              serialized_object, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                id,
                authz_id,
                org_id,
                name,
                environment,
                policy_name,
                policy_group,
                serialized_object,
                creator_authz_id,
                now
            ],
        )
        .map_err(|error| crate::identity::conflict(error, "Node already exists".to_string()))?;
        tx.commit()?;
        Ok(StoredNode {
            id,
            authz_id,
            name: name.into(),
            environment: environment.into(),
            policy_name: policy_name.map(ToString::to_string),
            policy_group: policy_group.map(ToString::to_string),
            serialized_object: body.into(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn find_node(&self, org_id: &str, name: &str) -> Result<Option<StoredNode>, StoreError> {
        let connection = self.connection();
        let node = connection
            .query_row(
                "SELECT id, authz_id, name, environment, policy_name, policy_group,
                        serialized_object, created_at, updated_at
                 FROM nodes WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                node_from_row,
            )
            .optional()?;
        decode_node_option(node)
    }

    pub fn list_nodes(&self, org_id: &str) -> Result<Vec<StoredNode>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, authz_id, name, environment, policy_name, policy_group,
                    serialized_object, created_at, updated_at
             FROM nodes WHERE org_id = ?1 ORDER BY name",
        )?;
        let rows = statement.query_map([org_id], node_from_row)?;
        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(decode_node(row?)?);
        }
        Ok(nodes)
    }

    /// Number of nodes across every organization, matching upstream
    /// `chef_db:count_nodes/1` used by the license endpoint.
    pub fn count_nodes(&self) -> Result<u64, StoreError> {
        let connection = self.connection();
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    pub fn list_nodes_in_environment(
        &self,
        org_id: &str,
        environment: &str,
    ) -> Result<Vec<StoredNode>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, authz_id, name, environment, policy_name, policy_group,
                    serialized_object, created_at, updated_at
             FROM nodes WHERE org_id = ?1 AND environment = ?2 ORDER BY name",
        )?;
        let rows = statement.query_map(params![org_id, environment], node_from_row)?;
        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(decode_node(row?)?);
        }
        Ok(nodes)
    }

    pub fn update_node(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        environment: &str,
        policy_name: Option<&str>,
        policy_group: Option<&str>,
    ) -> Result<StoredNode, StoreError> {
        let now = now();
        let serialized_object = encode_serialized(body, self.compression())?;
        let connection = self.connection();
        let changed = connection.execute(
            "UPDATE nodes
             SET serialized_object = ?3, environment = ?4, policy_name = ?5,
                 policy_group = ?6, updated_at = ?7
             WHERE org_id = ?1 AND name = ?2",
            params![
                org_id,
                name,
                serialized_object,
                environment,
                policy_name,
                policy_group,
                now
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("node {name}")));
        }
        drop(connection);
        self.find_node(org_id, name)?
            .ok_or_else(|| StoreError::NotFound(format!("node {name}")))
    }

    pub fn delete_node(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let authz_id: String = tx
            .query_row(
                "SELECT authz_id FROM nodes WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("node {name}")))?;
        tx.execute("DELETE FROM authz_entities WHERE authz_id = ?1", [authz_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_environment(
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
    }

    pub fn find_environment(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        self.find_named_object("environments", org_id, name)
    }

    pub fn list_environments(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError> {
        self.list_named_objects("environments", org_id)
    }

    pub fn update_environment(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError> {
        self.update_named_object("environments", org_id, name, body)
    }

    pub fn delete_environment(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_named_object("environments", org_id, name)
    }

    pub fn create_role(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        self.create_named_object(org_id, "roles", "roles", name, body, creator_authz_id)
    }

    pub fn find_role(&self, org_id: &str, name: &str) -> Result<Option<StoredObject>, StoreError> {
        self.find_named_object("roles", org_id, name)
    }

    pub fn list_roles(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError> {
        self.list_named_objects("roles", org_id)
    }

    pub fn update_role(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError> {
        self.update_named_object("roles", org_id, name, body)
    }

    pub fn delete_role(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_named_object("roles", org_id, name)
    }

    pub fn create_data_bag(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        self.create_named_object(org_id, "data", "data_bags", name, "{}", creator_authz_id)
    }

    pub fn find_data_bag(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        self.find_named_object("data_bags", org_id, name)
    }

    pub fn list_data_bags(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError> {
        self.list_named_objects("data_bags", org_id)
    }

    pub fn delete_data_bag(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_named_object("data_bags", org_id, name)
    }

    pub fn create_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
        body: &str,
    ) -> Result<StoredDataBagItem, StoreError> {
        let id = new_id();
        let now = now();
        let serialized_object = encode_serialized(body, self.compression())?;
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO data_bag_items
                 (id, org_id, data_bag_name, item_name, serialized_object,
                  last_updated_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, ?6)",
                params![id, org_id, data_bag_name, item_name, serialized_object, now],
            )
            .map_err(|error| {
                crate::identity::conflict(
                    error,
                    format!("data bag item {data_bag_name}/{item_name} already exists"),
                )
            })?;
        Ok(StoredDataBagItem {
            id,
            data_bag_name: data_bag_name.into(),
            item_name: item_name.into(),
            serialized_object: body.into(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn find_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
    ) -> Result<Option<StoredDataBagItem>, StoreError> {
        let connection = self.connection();
        let item = connection
            .query_row(
                "SELECT id, data_bag_name, item_name, serialized_object, created_at, updated_at
                 FROM data_bag_items
                 WHERE org_id = ?1 AND data_bag_name = ?2 AND item_name = ?3",
                params![org_id, data_bag_name, item_name],
                data_bag_item_from_row,
            )
            .optional()?;
        decode_data_bag_item_option(item)
    }

    pub fn list_data_bag_items(
        &self,
        org_id: &str,
        data_bag_name: &str,
    ) -> Result<Vec<StoredDataBagItem>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, data_bag_name, item_name, serialized_object, created_at, updated_at
             FROM data_bag_items
             WHERE org_id = ?1 AND data_bag_name = ?2 ORDER BY item_name",
        )?;
        let rows = statement.query_map(params![org_id, data_bag_name], data_bag_item_from_row)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(decode_data_bag_item(row?)?);
        }
        Ok(items)
    }

    pub fn update_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
        body: &str,
    ) -> Result<StoredDataBagItem, StoreError> {
        let now = now();
        let serialized_object = encode_serialized(body, self.compression())?;
        let connection = self.connection();
        let changed = connection.execute(
            "UPDATE data_bag_items
             SET serialized_object = ?4, updated_at = ?5
             WHERE org_id = ?1 AND data_bag_name = ?2 AND item_name = ?3",
            params![org_id, data_bag_name, item_name, serialized_object, now],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!(
                "data bag item {data_bag_name}/{item_name}"
            )));
        }
        drop(connection);
        self.find_data_bag_item(org_id, data_bag_name, item_name)?
            .ok_or_else(|| {
                StoreError::NotFound(format!("data bag item {data_bag_name}/{item_name}"))
            })
    }

    pub fn delete_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection();
        let changed = connection.execute(
            "DELETE FROM data_bag_items
             WHERE org_id = ?1 AND data_bag_name = ?2 AND item_name = ?3",
            params![org_id, data_bag_name, item_name],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!(
                "data bag item {data_bag_name}/{item_name}"
            )));
        }
        Ok(())
    }

    fn create_named_object(
        &self,
        org_id: &str,
        container_name: &str,
        table: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        let id = new_id();
        let now = now();
        let serialized_object = encode_serialized(body, self.compression())?;
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let authz_id = create_object_authz(&tx, org_id, container_name, creator_authz_id)?;
        let sql = format!(
            "INSERT INTO {table}
             (id, authz_id, org_id, name, serialized_object, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)"
        );
        tx.execute(
            &sql,
            params![
                id,
                authz_id,
                org_id,
                name,
                serialized_object,
                creator_authz_id,
                now
            ],
        )
        .map_err(|error| {
            crate::identity::conflict(error, format!("{table} {name} already exists"))
        })?;
        tx.commit()?;
        Ok(StoredObject {
            id,
            authz_id,
            name: name.into(),
            serialized_object: body.into(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    fn find_named_object(
        &self,
        table: &str,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        let connection = self.connection();
        let sql = format!(
            "SELECT id, authz_id, name, serialized_object, created_at, updated_at
             FROM {table} WHERE org_id = ?1 AND name = ?2"
        );
        let object = connection
            .query_row(&sql, params![org_id, name], stored_object_from_row)
            .optional()?;
        decode_stored_object_option(object)
    }

    fn list_named_objects(
        &self,
        table: &str,
        org_id: &str,
    ) -> Result<Vec<StoredObject>, StoreError> {
        let connection = self.connection();
        let sql = format!(
            "SELECT id, authz_id, name, serialized_object, created_at, updated_at
             FROM {table} WHERE org_id = ?1 ORDER BY name"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([org_id], stored_object_from_row)?;
        let mut objects = Vec::new();
        for row in rows {
            objects.push(decode_stored_object(row?)?);
        }
        Ok(objects)
    }

    /// Update a named object, honouring a rename requested by the normalized
    /// body.
    ///
    /// Environments (and, for this server, roles) may be renamed with `PUT`:
    /// the body's `name` becomes the resource name. The row keeps its `id` and
    /// `authz_id`, so the ACL and every foreign reference remain intact. A
    /// rename onto an existing name is rejected as a conflict before anything
    /// is written.
    fn update_named_object(
        &self,
        table: &str,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError> {
        let now = now();
        let serialized_object = encode_serialized(body, self.compression())?;
        let new_name = requested_name(body).unwrap_or_else(|| name.to_string());
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        if new_name != name {
            let sql = format!("SELECT 1 FROM {table} WHERE org_id = ?1 AND name = ?2");
            let existing = tx
                .query_row(&sql, params![org_id, new_name], |_| Ok(()))
                .optional()?;
            if existing.is_some() {
                return Err(StoreError::Conflict(format!(
                    "{table} {new_name} already exists"
                )));
            }
        }
        let sql = format!(
            "UPDATE {table} SET name = ?3, serialized_object = ?4, updated_at = ?5
             WHERE org_id = ?1 AND name = ?2"
        );
        let changed = tx
            .execute(
                &sql,
                params![org_id, name, new_name, serialized_object, now],
            )
            .map_err(|error| {
                crate::identity::conflict(error, format!("{table} {new_name} already exists"))
            })?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("{table} {name}")));
        }
        tx.commit()?;
        drop(connection);
        self.find_named_object(table, org_id, &new_name)?
            .ok_or_else(|| StoreError::NotFound(format!("{table} {new_name}")))
    }

    fn delete_named_object(&self, table: &str, org_id: &str, name: &str) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let sql = format!("SELECT authz_id FROM {table} WHERE org_id = ?1 AND name = ?2");
        let authz_id: String = tx
            .query_row(&sql, params![org_id, name], |row| row.get(0))
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("{table} {name}")))?;
        tx.execute("DELETE FROM authz_entities WHERE authz_id = ?1", [authz_id])?;
        tx.commit()?;
        Ok(())
    }
}

pub(crate) fn create_object_authz(
    tx: &Transaction<'_>,
    org_id: &str,
    container_name: &str,
    creator_authz_id: &str,
) -> Result<String, StoreError> {
    let authz_id = create_acl_object(tx, org_id, container_name)?;
    grant_actor_all(tx, &authz_id, creator_authz_id)?;
    Ok(authz_id)
}

/// Create an authz object that inherits the ACL of `container_name` without
/// granting any additional actor.
pub(crate) fn create_acl_object(
    tx: &Transaction<'_>,
    org_id: &str,
    container_name: &str,
) -> Result<String, StoreError> {
    let container_authz_id: String = tx
        .query_row(
            "SELECT authz_id FROM containers WHERE org_id = ?1 AND name = ?2",
            params![org_id, container_name],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("container {container_name}")))?;

    let authz_id = new_id();
    tx.execute(
        "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
         VALUES (?1, 'object', ?2, ?3)",
        params![authz_id, org_id, now()],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
         SELECT ?1, permission, actor_authz_id FROM authz_acl_actor
         WHERE target_authz_id = ?2",
        params![authz_id, container_authz_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO authz_acl_group (target_authz_id, permission, group_authz_id)
         SELECT ?1, permission, group_authz_id FROM authz_acl_group
         WHERE target_authz_id = ?2",
        params![authz_id, container_authz_id],
    )?;

    Ok(authz_id)
}

pub(crate) fn grant_actor_all(
    tx: &Transaction<'_>,
    target_authz_id: &str,
    actor_authz_id: &str,
) -> Result<(), StoreError> {
    for permission in Permission::ALL {
        tx.execute(
            "INSERT OR IGNORE INTO authz_acl_actor
             (target_authz_id, permission, actor_authz_id)
             VALUES (?1, ?2, ?3)",
            params![target_authz_id, permission.as_str(), actor_authz_id],
        )?;
    }
    Ok(())
}

fn node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredNode> {
    Ok(StoredNode {
        id: row.get(0)?,
        authz_id: row.get(1)?,
        name: row.get(2)?,
        environment: row.get(3)?,
        policy_name: row.get(4)?,
        policy_group: row.get(5)?,
        serialized_object: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

/// Read the `name` member of a normalized named-object body.
fn requested_name(body: &str) -> Option<String> {
    let document: serde_json::Value = serde_json::from_str(body).ok()?;
    document
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
}

fn stored_object_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredObject> {
    Ok(StoredObject {
        id: row.get(0)?,
        authz_id: row.get(1)?,
        name: row.get(2)?,
        serialized_object: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn data_bag_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredDataBagItem> {
    Ok(StoredDataBagItem {
        id: row.get(0)?,
        data_bag_name: row.get(1)?,
        item_name: row.get(2)?,
        serialized_object: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn decode_node(mut node: StoredNode) -> Result<StoredNode, StoreError> {
    node.serialized_object = decode_serialized(&node.serialized_object)?;
    Ok(node)
}

fn decode_node_option(node: Option<StoredNode>) -> Result<Option<StoredNode>, StoreError> {
    node.map(decode_node).transpose()
}

fn decode_stored_object(mut object: StoredObject) -> Result<StoredObject, StoreError> {
    object.serialized_object = decode_serialized(&object.serialized_object)?;
    Ok(object)
}

fn decode_stored_object_option(
    object: Option<StoredObject>,
) -> Result<Option<StoredObject>, StoreError> {
    object.map(decode_stored_object).transpose()
}

fn decode_data_bag_item(mut item: StoredDataBagItem) -> Result<StoredDataBagItem, StoreError> {
    item.serialized_object = decode_serialized(&item.serialized_object)?;
    Ok(item)
}

fn decode_data_bag_item_option(
    item: Option<StoredDataBagItem>,
) -> Result<Option<StoredDataBagItem>, StoreError> {
    item.map(decode_data_bag_item).transpose()
}
