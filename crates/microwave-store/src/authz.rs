use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
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

/// The members of an organization group, in the three buckets the upstream
/// API exposes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupMembers {
    pub users: Vec<String>,
    pub clients: Vec<String>,
    pub groups: Vec<String>,
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
        transaction
            .execute(
                "INSERT INTO containers (id, org_id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, org_id, authz_id, name, now],
            )
            .map_err(|error| {
                crate::identity::conflict(error, format!("container {name} already exists"))
            })?;
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
        transaction
            .execute(
                "INSERT INTO groups (id, org_id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, org_id, authz_id, name, now],
            )
            .map_err(|error| {
                crate::identity::conflict(error, format!("group {name} already exists"))
            })?;
        transaction.commit()?;
        Ok(Group {
            id,
            authz_id,
            name: name.into(),
        })
    }

    /// Create a group that inherits the org's `groups` container ACL and then
    /// grants the creating actor all permissions on it.
    pub fn create_group_for_org_with_acl(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<Group, StoreError> {
        let id = Uuid::new_v4().simple().to_string();
        let authz_id = Uuid::new_v4().simple().to_string();
        let now = crate::identity::now();
        let container_authz_id: String = {
            let connection = self.connection();
            connection
                .query_row(
                    "SELECT authz_id FROM containers WHERE org_id = ?1 AND name = 'groups'",
                    [org_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound("container groups".into()))?
        };
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES (?1, 'group', ?2, ?3)",
            params![authz_id, org_id, now],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
             SELECT ?1, permission, actor_authz_id FROM authz_acl_actor
             WHERE target_authz_id = ?2",
            params![authz_id, container_authz_id],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO authz_acl_group (target_authz_id, permission, group_authz_id)
             SELECT ?1, permission, group_authz_id FROM authz_acl_group
             WHERE target_authz_id = ?2",
            params![authz_id, container_authz_id],
        )?;
        for permission in Permission::ALL {
            transaction.execute(
                "INSERT OR IGNORE INTO authz_acl_actor
                 (target_authz_id, permission, actor_authz_id)
                 VALUES (?1, ?2, ?3)",
                params![authz_id, permission.as_str(), creator_authz_id],
            )?;
        }
        transaction
            .execute(
                "INSERT INTO groups (id, org_id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, org_id, authz_id, name, now],
            )
            .map_err(|error| {
                crate::identity::conflict(error, format!("group {name} already exists"))
            })?;
        transaction.commit()?;
        Ok(Group {
            id,
            authz_id,
            name: name.into(),
        })
    }

    /// Find or create a group that is not scoped to an organization.
    ///
    /// Upstream uses these for `server-admins` and the per-organization
    /// `<org>_read_access_group`; ACL responses render them with a `::`
    /// prefix.
    pub fn ensure_global_group(&self, name: &str) -> Result<Group, StoreError> {
        if let Some(group) = self.find_global_group(name)? {
            return Ok(group);
        }

        let id = Uuid::new_v4().simple().to_string();
        let authz_id = Uuid::new_v4().simple().to_string();
        let now = crate::identity::now();
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES (?1, 'group', NULL, ?2)",
            params![authz_id, now],
        )?;
        // Concurrent first-user/first-org startup can race here. One writer
        // wins the unique name; the losers discard their authz entity and
        // return the winner instead of surfacing a spurious conflict.
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO global_groups (id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, authz_id, name, now],
        )?;
        if inserted == 0 {
            transaction.rollback()?;
            drop(connection);
            return self.find_global_group(name)?.ok_or_else(|| {
                StoreError::Conflict(format!("group {name} was created concurrently"))
            });
        }
        for permission in Permission::ALL {
            transaction.execute(
                "INSERT INTO authz_acl_group (target_authz_id, permission, group_authz_id)
                 VALUES (?1, ?2, ?3)",
                params![authz_id, permission.as_str(), authz_id],
            )?;
        }
        transaction.commit()?;
        Ok(Group {
            id,
            authz_id,
            name: name.into(),
        })
    }
}

/// Predicate over `authz_entities` that matches an entity no organization,
/// user, client, group, container, or stored object references.
///
/// Every column that stores an `authz_entities` primary key must appear here.
/// The `authz_acl_*` and `authz_group_*` relation tables are intentionally
/// excluded: they describe grants, and a grant whose target or actor no longer
/// exists is exactly what cleanup removes.
const ORPHAN_AUTHZ_ENTITY_PREDICATE: &str = "\
    NOT EXISTS (SELECT 1 FROM orgs WHERE orgs.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM users WHERE users.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM clients WHERE clients.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM containers WHERE containers.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM groups WHERE groups.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM global_groups WHERE global_groups.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM nodes WHERE nodes.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM roles WHERE roles.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM environments WHERE environments.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM data_bags WHERE data_bags.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM cookbooks WHERE cookbooks.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM cookbook_artifacts WHERE cookbook_artifacts.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM policies WHERE policies.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (SELECT 1 FROM policy_groups WHERE policy_groups.authz_id = authz_entities.authz_id)\
    AND NOT EXISTS (\
        SELECT 1 FROM policy_group_revision_associations association \
        WHERE association.policy_group_authz_id = authz_entities.authz_id\
    )";

impl SqliteStore {
    /// Count authorization entities that no identity or object table
    /// references. See [`ORPHAN_AUTHZ_ENTITY_PREDICATE`] for the definition.
    pub fn count_orphan_authz_entities(&self) -> Result<u64, StoreError> {
        let connection = self.connection();
        let count: i64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM authz_entities WHERE {ORPHAN_AUTHZ_ENTITY_PREDICATE}"),
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Remove authorization entities that no identity or object table
    /// references, plus any ACL or group-membership rows still pointing at
    /// them.
    ///
    /// The relation deletes are explicit so cleanup stays correct even when a
    /// connection was opened with `PRAGMA foreign_keys = OFF`; the schema's
    /// cascades normally do the same work.
    pub fn delete_orphan_authz_entities(&self) -> Result<u64, StoreError> {
        let orphans =
            format!("SELECT authz_id FROM authz_entities WHERE {ORPHAN_AUTHZ_ENTITY_PREDICATE}");
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        for (table, columns) in [
            ("authz_acl_actor", ["target_authz_id", "actor_authz_id"]),
            ("authz_acl_group", ["target_authz_id", "group_authz_id"]),
            ("authz_group_actors", ["group_authz_id", "actor_authz_id"]),
            (
                "authz_group_groups",
                ["parent_group_authz_id", "child_group_authz_id"],
            ),
        ] {
            let predicates = columns
                .map(|column| format!("{column} IN ({orphans})"))
                .join(" OR ");
            transaction.execute(&format!("DELETE FROM {table} WHERE {predicates}"), [])?;
        }
        let removed = transaction.execute(
            &format!("DELETE FROM authz_entities WHERE {ORPHAN_AUTHZ_ENTITY_PREDICATE}"),
            [],
        )?;
        transaction.commit()?;
        Ok(removed as u64)
    }
}

fn delete_global_group_in_transaction(
    transaction: &Transaction<'_>,
    name: &str,
) -> Result<(), StoreError> {
    let authz_id: Option<String> = transaction
        .query_row(
            "SELECT authz_id FROM global_groups WHERE name = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    let Some(authz_id) = authz_id else {
        return Ok(());
    };

    for statement in [
        "DELETE FROM authz_acl_actor
         WHERE target_authz_id = ?1 OR actor_authz_id = ?1",
        "DELETE FROM authz_acl_group
         WHERE target_authz_id = ?1 OR group_authz_id = ?1",
        "DELETE FROM authz_group_actors
         WHERE group_authz_id = ?1 OR actor_authz_id = ?1",
        "DELETE FROM authz_group_groups
         WHERE parent_group_authz_id = ?1 OR child_group_authz_id = ?1",
    ] {
        transaction.execute(statement, [&authz_id])?;
    }
    transaction.execute(
        "DELETE FROM authz_entities WHERE authz_id = ?1",
        [&authz_id],
    )?;
    transaction.execute("DELETE FROM global_groups WHERE name = ?1", [name])?;
    Ok(())
}

impl SqliteStore {
    pub fn find_global_group(&self, name: &str) -> Result<Option<Group>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, authz_id, name FROM global_groups WHERE name = ?1",
                [name],
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

    pub fn list_global_groups(&self) -> Result<Vec<Group>, StoreError> {
        let connection = self.connection();
        let mut statement =
            connection.prepare("SELECT id, authz_id, name FROM global_groups ORDER BY name")?;
        let groups = statement
            .query_map([], |row| {
                Ok(Group {
                    id: row.get(0)?,
                    authz_id: row.get(1)?,
                    name: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(groups)
    }

    /// Remove a global group and every row that references its authz entity.
    ///
    /// The explicit ACL/membership deletes keep this reliable even when a
    /// caller opens SQLite without `foreign_keys = ON`; the store itself
    /// normally enables cascades.
    pub fn delete_global_group(&self, name: &str) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        delete_global_group_in_transaction(&transaction, name)?;
        transaction.commit()?;
        Ok(())
    }

    /// Resolve an ACL group name in the syntax used by Chef Server:
    /// `name` (the target organization), `org::name` (another organization) or
    /// `::name` (a global group).
    pub fn find_scoped_group(
        &self,
        org_id: Option<&str>,
        name: &str,
    ) -> Result<Option<Group>, StoreError> {
        if let Some(global_name) = name.strip_prefix("::") {
            return self.find_global_group(global_name);
        }
        if let Some((org_name, group_name)) = name.split_once("::") {
            let org = self.find_org(org_name)?;
            return match org {
                Some(org) => self.find_group(&org.id, group_name),
                None => Ok(None),
            };
        }
        match org_id {
            Some(org_id) => self.find_group(org_id, name),
            None => Ok(None),
        }
    }

    pub fn grant_actor_all(
        &self,
        target_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        crate::objects::grant_actor_all(&transaction, target_authz_id, actor_authz_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list_groups(&self, org_id: &str) -> Result<Vec<Group>, StoreError> {
        let connection = self.connection();
        let mut statement = connection
            .prepare("SELECT id, authz_id, name FROM groups WHERE org_id = ?1 ORDER BY name")?;
        let rows = statement.query_map([org_id], |row| {
            Ok(Group {
                id: row.get(0)?,
                authz_id: row.get(1)?,
                name: row.get(2)?,
            })
        })?;
        let mut groups = Vec::new();
        for row in rows {
            groups.push(row?);
        }
        Ok(groups)
    }

    pub fn rename_group(
        &self,
        org_id: &str,
        name: &str,
        new_name: &str,
    ) -> Result<Group, StoreError> {
        let now = crate::identity::now();
        let connection = self.connection();
        let changed = connection
            .execute(
                "UPDATE groups SET name = ?3, updated_at = ?4 WHERE org_id = ?1 AND name = ?2",
                params![org_id, name, new_name, now],
            )
            .map_err(|error| {
                crate::identity::conflict(error, format!("group {new_name} already exists"))
            })?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("group {name}")));
        }
        drop(connection);
        self.find_group(org_id, new_name)?
            .ok_or_else(|| StoreError::NotFound(format!("group {new_name}")))
    }

    pub fn delete_group(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let group = self
            .find_group(org_id, name)?
            .ok_or_else(|| StoreError::NotFound(format!("group {name}")))?;
        let connection = self.connection();
        connection.execute(
            "DELETE FROM authz_entities WHERE authz_id = ?1",
            [&group.authz_id],
        )?;
        Ok(())
    }

    pub fn list_group_members(
        &self,
        org_id: &str,
        group_authz_id: &str,
    ) -> Result<GroupMembers, StoreError> {
        let connection = self.connection();
        let users = connection
            .prepare(
                "SELECT u.username
                 FROM authz_group_actors member
                 JOIN users u ON u.authz_id = member.actor_authz_id
                 WHERE member.group_authz_id = ?1
                 ORDER BY u.username",
            )?
            .query_map([group_authz_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let clients = connection
            .prepare(
                "SELECT c.name
                 FROM authz_group_actors member
                 JOIN clients c ON c.authz_id = member.actor_authz_id
                 WHERE member.group_authz_id = ?1 AND c.org_id = ?2
                 ORDER BY c.name",
            )?
            .query_map(params![group_authz_id, org_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut statement = connection.prepare(
            "SELECT g.name, g.org_id, o.name
             FROM authz_group_groups relation
             JOIN groups g ON g.authz_id = relation.child_group_authz_id
             JOIN orgs o ON o.id = g.org_id
             WHERE relation.parent_group_authz_id = ?1
             UNION ALL
             SELECT gg.name, NULL, NULL
             FROM authz_group_groups relation
             JOIN global_groups gg ON gg.authz_id = relation.child_group_authz_id
             WHERE relation.parent_group_authz_id = ?1",
        )?;
        let mut groups = statement
            .query_map([group_authz_id], |row| {
                let name: String = row.get(0)?;
                let group_org_id: Option<String> = row.get(1)?;
                let org_name: Option<String> = row.get(2)?;
                Ok(render_group_name(
                    Some(org_id),
                    &name,
                    group_org_id.as_deref(),
                    org_name.as_deref(),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        groups.sort();
        Ok(GroupMembers {
            users,
            clients,
            groups,
        })
    }

    /// Replace the members in each provided bucket. Buckets passed as `None`
    /// keep their existing membership, matching the upstream PATCH-like PUT
    /// semantics.
    pub fn replace_group_members(
        &self,
        org_id: &str,
        group_authz_id: &str,
        users: Option<&[String]>,
        clients: Option<&[String]>,
        groups: Option<&[String]>,
    ) -> Result<(), StoreError> {
        let current = self.list_group_members(org_id, group_authz_id)?;
        let users = users.unwrap_or(&current.users);
        let clients = clients.unwrap_or(&current.clients);
        let groups = groups.unwrap_or(&current.groups);

        // Verify group targets before taking the write lock. This also rejects
        // self membership and cycles.
        let connection = self.connection();
        let mut child_group_ids = Vec::with_capacity(groups.len());
        for name in groups {
            let child = find_scoped_group_in_connection(&connection, Some(org_id), name)?
                .ok_or_else(|| {
                    StoreError::Invalid(format!("group {name} does not exist in this organization"))
                })?;
            let child = child.authz_id;
            if child == group_authz_id {
                return Err(StoreError::Invalid("a group cannot contain itself".into()));
            }
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
                params![child, group_authz_id],
                |row| row.get(0),
            )?;
            if creates_cycle != 0 {
                return Err(StoreError::Invalid("group cycle detected".into()));
            }
            child_group_ids.push(child);
        }

        let mut actor_ids = Vec::with_capacity(users.len() + clients.len());
        for name in users {
            let id: Option<String> = connection
                .query_row(
                    "SELECT authz_id FROM users WHERE username = ?1",
                    [name],
                    |row| row.get(0),
                )
                .optional()?;
            match id {
                Some(id) => actor_ids.push(id),
                // Clients calling the groups API send the combined actor list
                // in `users` as well as the typed `clients` list. A name that
                // is only a client is added by the loop below.
                None if clients.iter().any(|client| client == name) => {}
                None => return Err(StoreError::Invalid(format!("user {name} does not exist"))),
            }
        }
        for name in clients {
            let id: Option<String> = connection
                .query_row(
                    "SELECT authz_id FROM clients WHERE org_id = ?1 AND name = ?2",
                    params![org_id, name],
                    |row| row.get(0),
                )
                .optional()?;
            actor_ids.push(
                id.ok_or_else(|| StoreError::Invalid(format!("client {name} does not exist")))?,
            );
        }
        drop(connection);

        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM authz_group_actors WHERE group_authz_id = ?1",
            [group_authz_id],
        )?;
        transaction.execute(
            "DELETE FROM authz_group_groups WHERE parent_group_authz_id = ?1",
            [group_authz_id],
        )?;
        for actor_id in actor_ids {
            transaction.execute(
                "INSERT OR IGNORE INTO authz_group_actors (group_authz_id, actor_authz_id)
                 VALUES (?1, ?2)",
                params![group_authz_id, actor_id],
            )?;
        }
        for child_id in child_group_ids {
            transaction.execute(
                "INSERT OR IGNORE INTO authz_group_groups
                 (parent_group_authz_id, child_group_authz_id)
                 VALUES (?1, ?2)",
                params![group_authz_id, child_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Create a container and grant the creator every permission on it.
    /// Containers do not inherit their parent container's ACL upstream.
    pub fn create_container_for_org_with_acl(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
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
        for permission in Permission::ALL {
            transaction.execute(
                "INSERT OR IGNORE INTO authz_acl_actor
                 (target_authz_id, permission, actor_authz_id)
                 VALUES (?1, ?2, ?3)",
                params![authz_id, permission.as_str(), creator_authz_id],
            )?;
        }
        transaction
            .execute(
                "INSERT INTO containers (id, org_id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, org_id, authz_id, name, now],
            )
            .map_err(|error| {
                crate::identity::conflict(error, format!("container {name} already exists"))
            })?;
        transaction.commit()?;
        Ok(Container {
            id,
            authz_id,
            name: name.into(),
        })
    }

    pub fn list_containers(&self, org_id: &str) -> Result<Vec<Container>, StoreError> {
        let connection = self.connection();
        let mut statement = connection
            .prepare("SELECT id, authz_id, name FROM containers WHERE org_id = ?1 ORDER BY name")?;
        let rows = statement.query_map([org_id], |row| {
            Ok(Container {
                id: row.get(0)?,
                authz_id: row.get(1)?,
                name: row.get(2)?,
            })
        })?;
        let mut containers = Vec::new();
        for row in rows {
            containers.push(row?);
        }
        Ok(containers)
    }

    pub fn delete_container(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let container = self
            .find_container(org_id, name)?
            .ok_or_else(|| StoreError::NotFound(format!("container {name}")))?;
        let connection = self.connection();
        connection.execute(
            "DELETE FROM authz_entities WHERE authz_id = ?1",
            [&container.authz_id],
        )?;
        Ok(())
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
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO org_user_associations
             (org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, '', ?3, ?3)",
            params![org_id, user_id, now],
        )?;
        let user_authz_id: String = transaction.query_row(
            "SELECT authz_id FROM users WHERE id = ?1",
            [user_id],
            |row| row.get(0),
        )?;
        add_org_read_access_ace(&transaction, org_id, &user_authz_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_org(&self, org_id: &str) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let name: Option<String> = transaction
            .query_row("SELECT name FROM orgs WHERE id = ?1", [org_id], |row| {
                row.get(0)
            })
            .optional()?;
        // `cookbook_versions` and `cookbook_artifact_versions` restrict their
        // parent deletes, so remove version/link rows before deleting the
        // organization that owns them. The remaining org-owned rows cascade.
        for statement in [
            "DELETE FROM cookbook_version_checksums WHERE org_id = ?1",
            "DELETE FROM cookbook_artifact_version_checksums WHERE org_id = ?1",
            "DELETE FROM cookbook_versions WHERE org_id = ?1",
            "DELETE FROM cookbooks WHERE org_id = ?1",
            "DELETE FROM cookbook_artifact_versions WHERE org_id = ?1",
            "DELETE FROM cookbook_artifacts WHERE org_id = ?1",
            "DELETE FROM sandboxes WHERE org_id = ?1",
            "DELETE FROM sandboxed_checksums WHERE org_id = ?1",
            "DELETE FROM sandbox_checksums WHERE org_id = ?1",
            "DELETE FROM actor_keys
             WHERE actor_type = 'client'
               AND actor_id IN (SELECT id FROM clients WHERE org_id = ?1)",
            "DELETE FROM checksums WHERE org_id = ?1",
            "DELETE FROM orgs WHERE id = ?1",
        ] {
            transaction.execute(statement, params![org_id])?;
        }
        if let Some(name) = name {
            // Keep the org row, its dependents and the global read-access
            // group in one transaction. A failure here rolls the org delete
            // back instead of leaving an unrecoverable half-deleted org.
            delete_global_group_in_transaction(&transaction, &format!("{name}_read_access_group"))?;
        }
        transaction.commit()?;
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

    pub fn remove_group_member(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection();
        connection.execute(
            "DELETE FROM authz_group_actors
             WHERE group_authz_id = ?1 AND actor_authz_id = ?2",
            params![group_authz_id, actor_authz_id],
        )?;
        Ok(())
    }

    pub fn add_group_child(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError> {
        // Take the write lock before checking reachability so two concurrent
        // calls cannot both observe an acyclic graph and then insert edges that
        // together form a cycle.
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_acyclic_in_connection(&transaction, parent_group_authz_id, child_group_authz_id)?;
        transaction.execute(
            "INSERT OR IGNORE INTO authz_group_groups
             (parent_group_authz_id, child_group_authz_id)
             VALUES (?1, ?2)",
            params![parent_group_authz_id, child_group_authz_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn ensure_acyclic(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_acyclic_in_connection(&transaction, parent_group_authz_id, child_group_authz_id)?;
        transaction.rollback()?;
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
    ///
    /// Group names are scoped relative to the target's organization: local
    /// groups are unqualified, groups from other organizations are
    /// `org::name`, and global groups are `::name`.
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
        let context_org_id: Option<String> = connection.query_row(
            "SELECT org_id FROM authz_entities WHERE authz_id = ?1",
            [target_authz_id],
            |row| row.get(0),
        )?;

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
            let mut statement = connection.prepare(
                "SELECT g.name, g.org_id, o.name
                 FROM authz_acl_group ace
                 JOIN groups g ON g.authz_id = ace.group_authz_id
                 JOIN orgs o ON o.id = g.org_id
                 WHERE ace.target_authz_id = ?1 AND ace.permission = ?2
                 UNION ALL
                 SELECT gg.name, NULL, NULL
                 FROM authz_acl_group ace
                 JOIN global_groups gg ON gg.authz_id = ace.group_authz_id
                 WHERE ace.target_authz_id = ?1 AND ace.permission = ?2",
            )?;
            let mut groups = statement
                .query_map(params![target_authz_id, permission.as_str()], |row| {
                    let name: String = row.get(0)?;
                    let group_org_id: Option<String> = row.get(1)?;
                    let org_name: Option<String> = row.get(2)?;
                    Ok(render_group_name(
                        context_org_id.as_deref(),
                        &name,
                        group_org_id.as_deref(),
                        org_name.as_deref(),
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            groups.sort();
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
        let org_id = acl_target_org_id(&transaction, target_authz_id)?;

        let mut actor_ids = Vec::with_capacity(actors.len());
        for name in actors {
            actor_ids.push(resolve_actor_authz_id(
                &transaction,
                org_id.as_deref(),
                name,
            )?);
        }

        let group_ids = resolve_group_ids(&transaction, org_id.as_deref(), groups)?;
        replace_acl_entries(
            &transaction,
            target_authz_id,
            permission,
            &actor_ids,
            &group_ids,
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Replace one permission's ACE from explicit `users`/`clients` lists.
    /// Unlike the legacy actor form this never treats a shared name as
    /// ambiguous, because the caller has named each actor's type.
    pub fn set_acl_for_permission_split(
        &self,
        target_authz_id: &str,
        permission: Permission,
        users: &[String],
        clients: &[String],
        groups: &[String],
    ) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let org_id = acl_target_org_id(&transaction, target_authz_id)?;

        let mut actor_ids = Vec::with_capacity(users.len() + clients.len());
        for name in users {
            actor_ids.push(resolve_user_authz_id(&transaction, name)?);
        }
        for name in clients {
            let Some(org_id) = org_id.as_deref() else {
                return Err(StoreError::Invalid(format!(
                    "client {name} is not allowed in this context"
                )));
            };
            actor_ids.push(resolve_client_authz_id(&transaction, org_id, name)?);
        }

        let group_ids = resolve_group_ids(&transaction, org_id.as_deref(), groups)?;
        replace_acl_entries(
            &transaction,
            target_authz_id,
            permission,
            &actor_ids,
            &group_ids,
        )?;
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

pub(crate) fn add_org_read_access_ace(
    transaction: &Transaction<'_>,
    org_id: &str,
    actor_authz_id: &str,
) -> Result<(), StoreError> {
    let Some(group_authz_id) = org_read_access_group_authz_id(transaction, org_id)? else {
        return Ok(());
    };
    transaction.execute(
        "INSERT OR IGNORE INTO authz_acl_group
         (target_authz_id, permission, group_authz_id)
         VALUES (?1, 'read', ?2)",
        params![actor_authz_id, group_authz_id],
    )?;
    Ok(())
}

pub(crate) fn remove_org_read_access_ace(
    transaction: &Transaction<'_>,
    org_id: &str,
    actor_authz_id: &str,
) -> Result<(), StoreError> {
    let Some(group_authz_id) = org_read_access_group_authz_id(transaction, org_id)? else {
        return Ok(());
    };
    transaction.execute(
        "DELETE FROM authz_acl_group
         WHERE target_authz_id = ?1 AND permission = 'read' AND group_authz_id = ?2",
        params![actor_authz_id, group_authz_id],
    )?;
    Ok(())
}

fn org_read_access_group_authz_id(
    transaction: &Transaction<'_>,
    org_id: &str,
) -> Result<Option<String>, StoreError> {
    let org_name: Option<String> = transaction
        .query_row("SELECT name FROM orgs WHERE id = ?1", [org_id], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(org_name) = org_name else {
        return Ok(None);
    };
    let group_name = format!("{org_name}_read_access_group");
    Ok(transaction
        .query_row(
            "SELECT authz_id FROM global_groups WHERE name = ?1",
            [group_name],
            |row| row.get(0),
        )
        .optional()?)
}

fn acl_target_org_id(
    transaction: &Transaction<'_>,
    target_authz_id: &str,
) -> Result<Option<String>, StoreError> {
    let org_id: Option<Option<String>> = transaction
        .query_row(
            "SELECT org_id FROM authz_entities WHERE authz_id = ?1",
            [target_authz_id],
            |row| row.get(0),
        )
        .optional()?;
    org_id.ok_or_else(|| StoreError::NotFound(format!("ACL target {target_authz_id}")))
}

fn resolve_group_ids(
    transaction: &Transaction<'_>,
    org_id: Option<&str>,
    groups: &[String],
) -> Result<Vec<String>, StoreError> {
    let mut group_ids = Vec::with_capacity(groups.len());
    for name in groups {
        let group =
            find_scoped_group_in_transaction(transaction, org_id, name)?.ok_or_else(|| {
                StoreError::Invalid(format!("group {name} does not exist in this organization"))
            })?;
        group_ids.push(group.authz_id);
    }
    Ok(group_ids)
}

fn replace_acl_entries(
    transaction: &Transaction<'_>,
    target_authz_id: &str,
    permission: Permission,
    actor_ids: &[String],
    group_ids: &[String],
) -> Result<(), StoreError> {
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
    Ok(())
}

fn ensure_acyclic_in_connection(
    connection: &rusqlite::Connection,
    parent_group_authz_id: &str,
    child_group_authz_id: &str,
) -> Result<(), StoreError> {
    if parent_group_authz_id == child_group_authz_id {
        return Err(StoreError::Invalid("a group cannot contain itself".into()));
    }

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

fn render_group_name(
    context_org_id: Option<&str>,
    name: &str,
    group_org_id: Option<&str>,
    org_name: Option<&str>,
) -> String {
    match (group_org_id, org_name) {
        (None, _) => format!("::{name}"),
        (Some(group_org_id), Some(org_name)) => {
            if context_org_id == Some(group_org_id) {
                name.to_string()
            } else {
                format!("{org_name}::{name}")
            }
        }
        (Some(_), None) => name.to_string(),
    }
}

fn find_scoped_group_in_connection(
    connection: &rusqlite::Connection,
    org_id: Option<&str>,
    name: &str,
) -> Result<Option<Group>, StoreError> {
    if let Some(global_name) = name.strip_prefix("::") {
        return query_global_group(connection, global_name);
    }
    if let Some((org_name, group_name)) = name.split_once("::") {
        let org_id: Option<String> = connection
            .query_row("SELECT id FROM orgs WHERE name = ?1", [org_name], |row| {
                row.get(0)
            })
            .optional()?;
        return match org_id {
            Some(org_id) => query_org_group(connection, &org_id, group_name),
            None => Ok(None),
        };
    }
    match org_id {
        Some(org_id) => query_org_group(connection, org_id, name),
        None => Ok(None),
    }
}

fn find_scoped_group_in_transaction(
    transaction: &Transaction<'_>,
    org_id: Option<&str>,
    name: &str,
) -> Result<Option<Group>, StoreError> {
    if let Some(global_name) = name.strip_prefix("::") {
        return query_global_group(transaction, global_name);
    }
    if let Some((org_name, group_name)) = name.split_once("::") {
        let org_id: Option<String> = transaction
            .query_row("SELECT id FROM orgs WHERE name = ?1", [org_name], |row| {
                row.get(0)
            })
            .optional()?;
        return match org_id {
            Some(org_id) => query_org_group(transaction, &org_id, group_name),
            None => Ok(None),
        };
    }
    match org_id {
        Some(org_id) => query_org_group(transaction, org_id, name),
        None => Ok(None),
    }
}

fn query_org_group(
    connection: &rusqlite::Connection,
    org_id: &str,
    name: &str,
) -> Result<Option<Group>, StoreError> {
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

fn query_global_group(
    connection: &rusqlite::Connection,
    name: &str,
) -> Result<Option<Group>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT id, authz_id, name FROM global_groups WHERE name = ?1",
            [name],
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

fn resolve_user_authz_id(transaction: &Transaction<'_>, name: &str) -> Result<String, StoreError> {
    transaction
        .query_row(
            "SELECT authz_id FROM users WHERE username = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::Invalid(format!("user {name} does not exist")))
}

fn resolve_client_authz_id(
    transaction: &Transaction<'_>,
    org_id: &str,
    name: &str,
) -> Result<String, StoreError> {
    transaction
        .query_row(
            "SELECT authz_id FROM clients WHERE org_id = ?1 AND name = ?2",
            params![org_id, name],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::Invalid(format!("client {name} does not exist in this organization"))
        })
}

fn resolve_actor_authz_id(
    transaction: &Transaction<'_>,
    org_id: Option<&str>,
    name: &str,
) -> Result<String, StoreError> {
    let user = transaction
        .query_row(
            "SELECT id, authz_id FROM users WHERE username = ?1",
            [name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;

    let client = match org_id {
        Some(org_id) => transaction
            .query_row(
                "SELECT authz_id FROM clients WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| row.get::<_, String>(0),
            )
            .optional()?,
        None => None,
    };

    match (user, client) {
        (Some((user_id, _)), Some(client_authz_id)) => {
            let Some(org_id) = org_id else {
                return Err(StoreError::Invalid(format!(
                    "actor {name} exists as both a client and a user"
                )));
            };
            let associated: i64 = transaction.query_row(
                "SELECT EXISTS (
                     SELECT 1 FROM org_user_associations
                     WHERE org_id = ?1 AND user_id = ?2
                 )",
                params![org_id, user_id],
                |row| row.get(0),
            )?;
            if associated != 0 {
                Err(StoreError::Invalid(format!(
                    "actor {name} exists as both a client and a user in this organization"
                )))
            } else {
                Ok(client_authz_id)
            }
        }
        (Some((_, user_authz_id)), None) => Ok(user_authz_id),
        (None, Some(client_authz_id)) => Ok(client_authz_id),
        (None, None) => Err(StoreError::Invalid(match org_id {
            Some(_) => format!("actor {name} does not exist in this organization"),
            None => format!("actor {name} does not exist"),
        })),
    }
}
