use crate::sqlite::{SqliteStore, StoreError};
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorType {
    User,
    Client,
}

impl ActorType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Client => "client",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Org {
    pub id: String,
    pub authz_id: String,
    pub name: String,
    pub full_name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub authz_id: String,
    pub username: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub password_salt: String,
    pub hash_type: String,
    pub admin: bool,
    pub recovery_authentication_enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub org_id: String,
    pub authz_id: String,
    pub name: String,
    pub validator: bool,
    pub admin: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorKey {
    pub actor_id: String,
    pub actor_type: ActorType,
    pub key_name: String,
    pub public_key: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub actor_id: String,
    pub authz_id: String,
    pub name: String,
    pub actor_type: ActorType,
    pub org_id: Option<String>,
}

impl SqliteStore {
    pub fn create_org(&self, name: &str, full_name: &str) -> Result<Org, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let now = now();
        let mut connection = self.connection();
        let transaction = connection.transaction()?;

        transaction
            .execute(
                "INSERT INTO orgs
                 (id, authz_id, name, full_name, assigned_at, last_updated_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, '', ?5, ?5)",
                params![id, authz_id, name, full_name, now],
            )
            .map_err(|error| conflict(error, format!("organization {name} already exists")))?;
        transaction
            .execute(
                "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
                 VALUES (?1, 'actor', ?2, ?3)",
                params![authz_id, id, now],
            )
            .map_err(StoreError::Sqlite)?;
        transaction.commit()?;

        Ok(Org {
            id,
            authz_id,
            name: name.into(),
            full_name: full_name.into(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn find_org(&self, name: &str) -> Result<Option<Org>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, authz_id, name, full_name, created_at, updated_at
                 FROM orgs WHERE name = ?1",
                [name],
                |row| {
                    Ok(Org {
                        id: row.get(0)?,
                        authz_id: row.get(1)?,
                        name: row.get(2)?,
                        full_name: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn list_orgs(&self) -> Result<Vec<Org>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, authz_id, name, full_name, created_at, updated_at
             FROM orgs ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Org {
                id: row.get(0)?,
                authz_id: row.get(1)?,
                name: row.get(2)?,
                full_name: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        let mut orgs = Vec::new();
        for row in rows {
            orgs.push(row?);
        }
        Ok(orgs)
    }

    pub fn create_user(
        &self,
        username: &str,
        email: &str,
        hash_type: &str,
        password_hash: &str,
        password_salt: &str,
        admin: bool,
    ) -> Result<User, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let now = now();
        let mut connection = self.connection();
        let transaction = connection.transaction()?;

        transaction
            .execute(
                "INSERT INTO users
                 (id, authz_id, username, email, password_hash, password_salt, hash_type,
                  admin, recovery_authentication_enabled, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?9)",
                params![
                    id,
                    authz_id,
                    username,
                    email,
                    password_hash,
                    password_salt,
                    hash_type,
                    admin as i64,
                    now
                ],
            )
            .map_err(|error| conflict(error, format!("user {username} already exists")))?;
        transaction
            .execute(
                "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
                 VALUES (?1, 'actor', NULL, ?2)",
                params![authz_id, now],
            )
            .map_err(StoreError::Sqlite)?;
        transaction.commit()?;

        Ok(User {
            id,
            authz_id,
            username: username.into(),
            email: Some(email.into()),
            password_hash: password_hash.into(),
            password_salt: password_salt.into(),
            hash_type: hash_type.into(),
            admin,
            recovery_authentication_enabled: false,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn find_user(&self, username: &str) -> Result<Option<User>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, authz_id, username, email, password_hash, password_salt,
                        hash_type, admin, recovery_authentication_enabled, created_at, updated_at
                 FROM users WHERE username = ?1",
                [username],
                |row| {
                    Ok(User {
                        id: row.get(0)?,
                        authz_id: row.get(1)?,
                        username: row.get(2)?,
                        email: row.get(3)?,
                        password_hash: row.get(4)?,
                        password_salt: row.get(5)?,
                        hash_type: row.get(6)?,
                        admin: row.get::<_, i64>(7)? != 0,
                        recovery_authentication_enabled: row.get::<_, i64>(8)? != 0,
                        created_at: row.get(9)?,
                        updated_at: row.get(10)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn create_client(
        &self,
        org_id: &str,
        name: &str,
        validator: bool,
        admin: bool,
    ) -> Result<Client, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let now = now();
        let mut connection = self.connection();
        let transaction = connection.transaction()?;

        transaction
            .execute(
                "INSERT INTO clients
                 (id, org_id, authz_id, name, validator, admin, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id,
                    org_id,
                    authz_id,
                    name,
                    validator as i64,
                    admin as i64,
                    now
                ],
            )
            .map_err(|error| conflict(error, format!("client {name} already exists")))?;
        transaction
            .execute(
                "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
                 VALUES (?1, 'actor', ?2, ?3)",
                params![authz_id, org_id, now],
            )
            .map_err(StoreError::Sqlite)?;
        transaction.commit()?;

        Ok(Client {
            id,
            org_id: org_id.into(),
            authz_id,
            name: name.into(),
            validator,
            admin,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn find_client(&self, org_id: &str, name: &str) -> Result<Option<Client>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, org_id, authz_id, name, validator, admin, created_at, updated_at
                 FROM clients WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| {
                    Ok(Client {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        authz_id: row.get(2)?,
                        name: row.get(3)?,
                        validator: row.get::<_, i64>(4)? != 0,
                        admin: row.get::<_, i64>(5)? != 0,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn add_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        public_key: &str,
    ) -> Result<(), StoreError> {
        let now = now();
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO actor_keys
                 (actor_id, actor_type, key_name, public_key, key_version, expires_at,
                  last_updated_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, 'infinity', '', ?5, ?5)",
                params![actor_id, actor_type.as_str(), key_name, public_key, now],
            )
            .map_err(|error| conflict(error, format!("key {key_name} already exists")))?;
        Ok(())
    }

    pub fn find_actor_keys(
        &self,
        actor_id: &str,
        actor_type: ActorType,
    ) -> Result<Vec<ActorKey>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT actor_id, actor_type, key_name, public_key, expires_at
             FROM actor_keys
             WHERE actor_id = ?1 AND actor_type = ?2
             ORDER BY key_name",
        )?;
        let rows = statement.query_map(params![actor_id, actor_type.as_str()], |row| {
            Ok(ActorKey {
                actor_id: row.get(0)?,
                actor_type: match row.get::<_, String>(1)?.as_str() {
                    "user" => ActorType::User,
                    _ => ActorType::Client,
                },
                key_name: row.get(2)?,
                public_key: row.get(3)?,
                expires_at: row.get(4)?,
            })
        })?;

        let mut keys = Vec::new();
        for row in rows {
            keys.push(row?);
        }
        Ok(keys)
    }

    pub fn find_principals(
        &self,
        org_id: Option<&str>,
        name: &str,
    ) -> Result<Vec<Principal>, StoreError> {
        let mut principals = Vec::new();

        if let Some(org_id) = org_id {
            if let Some(client) = self.find_client(org_id, name)? {
                principals.push(Principal {
                    actor_id: client.id,
                    authz_id: client.authz_id,
                    name: client.name,
                    actor_type: ActorType::Client,
                    org_id: Some(org_id.to_string()),
                });
            }
        }

        if let Some(user) = self.find_user(name)? {
            principals.push(Principal {
                actor_id: user.id,
                authz_id: user.authz_id,
                name: user.username,
                actor_type: ActorType::User,
                org_id: org_id.map(ToString::to_string),
            });
        }

        Ok(principals)
    }

    pub fn find_principal_keys(&self, principal: &Principal) -> Result<Vec<ActorKey>, StoreError> {
        self.find_actor_keys(&principal.actor_id, principal.actor_type)
    }

    pub fn list_users(&self) -> Result<Vec<User>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, authz_id, username, email, password_hash, password_salt,
                    hash_type, admin, recovery_authentication_enabled, created_at, updated_at
             FROM users ORDER BY username",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(User {
                id: row.get(0)?,
                authz_id: row.get(1)?,
                username: row.get(2)?,
                email: row.get(3)?,
                password_hash: row.get(4)?,
                password_salt: row.get(5)?,
                hash_type: row.get(6)?,
                admin: row.get::<_, i64>(7)? != 0,
                recovery_authentication_enabled: row.get::<_, i64>(8)? != 0,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })?;

        let mut users = Vec::new();
        for row in rows {
            users.push(row?);
        }
        Ok(users)
    }

    pub fn update_user(
        &self,
        username: &str,
        email: Option<&str>,
        password: Option<(&str, &str, &str)>,
        admin: Option<bool>,
    ) -> Result<User, StoreError> {
        let now = now();
        let connection = self.connection();
        let changed = connection.execute(
            "UPDATE users
             SET email = COALESCE(?2, email),
                 password_hash = COALESCE(?3, password_hash),
                 password_salt = COALESCE(?4, password_salt),
                 hash_type = COALESCE(?5, hash_type),
                 admin = COALESCE(?6, admin),
                 updated_at = ?7
             WHERE username = ?1",
            params![
                username,
                email,
                password.map(|value| value.0),
                password.map(|value| value.1),
                password.map(|value| value.2),
                admin.map(|value| value as i64),
                now
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("user {username}")));
        }
        self.find_user(username)?
            .ok_or_else(|| StoreError::NotFound(format!("user {username}")))
    }

    pub fn delete_user(&self, username: &str) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let user = transaction
            .query_row(
                "SELECT id, authz_id FROM users WHERE username = ?1",
                [username],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("user {username}")))?;

        transaction.execute(
            "DELETE FROM actor_keys WHERE actor_id = ?1 AND actor_type = 'user'",
            [&user.0],
        )?;
        transaction.execute("DELETE FROM authz_entities WHERE authz_id = ?1", [&user.1])?;
        transaction.execute("DELETE FROM users WHERE id = ?1", [&user.0])?;
        transaction.commit()?;
        Ok(())
    }
}

fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}

pub(crate) fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn conflict(error: rusqlite::Error, message: String) -> StoreError {
    if let rusqlite::Error::SqliteFailure(inner, _) = &error {
        if inner.code == rusqlite::ErrorCode::ConstraintViolation {
            return StoreError::Conflict(message);
        }
    }
    StoreError::Sqlite(error)
}
