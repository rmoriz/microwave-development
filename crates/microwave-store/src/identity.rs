use crate::sqlite::{SqliteStore, StoreError};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension, Row};
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
    pub external_authentication_uid: Option<String>,
    pub display_name: Option<String>,
    pub first_name: Option<String>,
    pub middle_name: Option<String>,
    pub last_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Profile fields stored alongside a user. Missing values are omitted from
/// the serialized user document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserProfile {
    pub external_authentication_uid: Option<String>,
    pub display_name: Option<String>,
    pub first_name: Option<String>,
    pub middle_name: Option<String>,
    pub last_name: Option<String>,
}

/// A partial user update. For profile fields the outer `Option` says whether
/// the request supplied the field and the inner `Option` is its new value
/// (`None` clears it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserPatch {
    pub new_username: Option<String>,
    pub email: Option<String>,
    pub password: Option<(String, String, String)>,
    pub admin: Option<bool>,
    pub recovery_authentication_enabled: Option<bool>,
    pub external_authentication_uid: Option<Option<String>>,
    pub display_name: Option<Option<String>>,
    pub first_name: Option<Option<String>>,
    pub middle_name: Option<Option<String>>,
    pub last_name: Option<Option<String>>,
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
    pub expired: bool,
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

    pub fn update_org(&self, name: &str, full_name: &str) -> Result<Org, StoreError> {
        let now = now();
        let connection = self.connection();
        let changed = connection.execute(
            "UPDATE orgs SET full_name = ?2, updated_at = ?3 WHERE name = ?1",
            params![name, full_name, now],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("organization {name}")));
        }
        drop(connection);
        self.find_org(name)?
            .ok_or_else(|| StoreError::NotFound(format!("organization {name}")))
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

    /// Create a user with the legacy signature. The profile defaults to the
    /// username so callers that only know a name still produce a complete
    /// user document.
    pub fn create_user(
        &self,
        username: &str,
        email: &str,
        hash_type: &str,
        password_hash: &str,
        password_salt: &str,
        admin: bool,
    ) -> Result<User, StoreError> {
        self.create_user_with_profile(
            username,
            Some(email),
            hash_type,
            password_hash,
            password_salt,
            admin,
            &UserProfile {
                display_name: Some(username.into()),
                first_name: Some(username.into()),
                last_name: Some(username.into()),
                ..Default::default()
            },
        )
    }

    /// Create a user and persist the optional profile fields.
    #[allow(clippy::too_many_arguments)]
    pub fn create_user_with_profile(
        &self,
        username: &str,
        email: Option<&str>,
        hash_type: &str,
        password_hash: &str,
        password_salt: &str,
        admin: bool,
        profile: &UserProfile,
    ) -> Result<User, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let now = now();
        let email = email.map(str::to_lowercase);
        let mut connection = self.connection();
        let transaction = connection.transaction()?;

        if let Some(email) = email.as_deref() {
            let taken: i64 = transaction.query_row(
                "SELECT EXISTS (SELECT 1 FROM users WHERE lower(email) = lower(?1))",
                [email],
                |row| row.get(0),
            )?;
            if taken != 0 {
                return Err(StoreError::Conflict(
                    "Username or email address already in use.".into(),
                ));
            }
        }

        transaction
            .execute(
                "INSERT INTO users
                 (id, authz_id, username, email, password_hash, password_salt, hash_type,
                  admin, recovery_authentication_enabled, external_authentication_uid,
                  display_name, first_name, middle_name, last_name, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
                params![
                    id,
                    authz_id,
                    username,
                    email,
                    password_hash,
                    password_salt,
                    hash_type,
                    admin as i64,
                    profile.external_authentication_uid,
                    profile.display_name,
                    profile.first_name,
                    profile.middle_name,
                    profile.last_name,
                    now
                ],
            )
            .map_err(|error| {
                conflict(
                    error,
                    "Username or email address already in use.".to_string(),
                )
            })?;
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
            email,
            password_hash: password_hash.into(),
            password_salt: password_salt.into(),
            hash_type: hash_type.into(),
            admin,
            recovery_authentication_enabled: false,
            external_authentication_uid: profile.external_authentication_uid.clone(),
            display_name: profile.display_name.clone(),
            first_name: profile.first_name.clone(),
            middle_name: profile.middle_name.clone(),
            last_name: profile.last_name.clone(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn find_user(&self, username: &str) -> Result<Option<User>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, authz_id, username, email, password_hash, password_salt,
                        hash_type, admin, recovery_authentication_enabled,
                        external_authentication_uid, display_name, first_name, middle_name,
                        last_name, created_at, updated_at
                 FROM users WHERE username = ?1",
                [username],
                user_from_row,
            )
            .optional()?)
    }

    pub fn find_user_by_id(&self, id: &str) -> Result<Option<User>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, authz_id, username, email, password_hash, password_salt,
                        hash_type, admin, recovery_authentication_enabled,
                        external_authentication_uid, display_name, first_name, middle_name,
                        last_name, created_at, updated_at
                 FROM users WHERE id = ?1",
                [id],
                user_from_row,
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

    /// Create a client and give it the same ACL template as the org's
    /// `clients` container.
    ///
    /// When `creator_authz_id` is provided that actor receives every
    /// permission on the new client. When it is omitted the new client itself
    /// receives those permissions instead, which mirrors how a validator
    /// creates a client with superuser privileges.
    pub fn create_client_with_acl(
        &self,
        org_id: &str,
        name: &str,
        validator: bool,
        admin: bool,
        creator_authz_id: Option<&str>,
    ) -> Result<Client, StoreError> {
        let id = new_id();
        let now = now();
        let mut connection = self.connection();
        let transaction = connection.transaction()?;

        let authz_id = crate::objects::create_acl_object(&transaction, org_id, "clients")?;
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
        let granted = creator_authz_id.unwrap_or(&authz_id);
        crate::objects::grant_actor_all(&transaction, &authz_id, granted)?;
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

    pub fn update_client(
        &self,
        org_id: &str,
        name: &str,
        new_name: &str,
        validator: Option<bool>,
    ) -> Result<Client, StoreError> {
        let now = now();
        let connection = self.connection();
        let changed = connection
            .execute(
                "UPDATE clients
                 SET name = ?3,
                     validator = COALESCE(?4, validator),
                     updated_at = ?5
                 WHERE org_id = ?1 AND name = ?2",
                params![org_id, name, new_name, validator.map(|v| v as i64), now],
            )
            .map_err(|error| conflict(error, format!("client {new_name} already exists")))?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("client {name}")));
        }
        drop(connection);
        self.find_client(org_id, new_name)?
            .ok_or_else(|| StoreError::NotFound(format!("client {new_name}")))
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

    pub fn list_clients(&self, org_id: &str) -> Result<Vec<Client>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, org_id, authz_id, name, validator, admin, created_at, updated_at
             FROM clients WHERE org_id = ?1 ORDER BY name",
        )?;
        let rows = statement.query_map([org_id], |row| {
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
        })?;
        let mut clients = Vec::new();
        for row in rows {
            clients.push(row?);
        }
        Ok(clients)
    }

    pub fn delete_client(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let client = transaction
            .query_row(
                "SELECT id, authz_id FROM clients WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("client {name}")))?;

        transaction.execute(
            "DELETE FROM actor_keys WHERE actor_id = ?1 AND actor_type = 'client'",
            [&client.0],
        )?;
        transaction.execute(
            "DELETE FROM authz_entities WHERE authz_id = ?1",
            [&client.1],
        )?;
        transaction.execute("DELETE FROM clients WHERE id = ?1", [&client.0])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn add_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        public_key: &str,
    ) -> Result<(), StoreError> {
        self.create_actor_key(actor_id, actor_type, key_name, public_key, "infinity")
            .map(|_| ())
    }

    /// Replace the actor's default key, creating it if it does not exist yet.
    pub fn update_or_create_default_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        public_key: &str,
    ) -> Result<ActorKey, StoreError> {
        if self
            .find_actor_key(actor_id, actor_type, "default")?
            .is_some()
        {
            self.update_actor_key(
                actor_id,
                actor_type,
                "default",
                None,
                Some(public_key),
                None,
            )
        } else {
            self.create_actor_key(actor_id, actor_type, "default", public_key, "infinity")
        }
    }

    /// Create a key for an actor. `expires_at` is either `"infinity"` or an
    /// RFC 3339 timestamp.
    pub fn create_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        public_key: &str,
        expires_at: &str,
    ) -> Result<ActorKey, StoreError> {
        if key_name.is_empty() {
            return Err(StoreError::Invalid("key name must not be empty".into()));
        }
        if public_key.is_empty() {
            return Err(StoreError::Invalid("public key must not be empty".into()));
        }

        let now = now();
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO actor_keys
                 (actor_id, actor_type, key_name, public_key, key_version, expires_at,
                  last_updated_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, '', ?6, ?6)",
                params![
                    actor_id,
                    actor_type.as_str(),
                    key_name,
                    public_key,
                    expires_at,
                    now
                ],
            )
            .map_err(|error| conflict(error, format!("key {key_name} already exists")))?;
        drop(connection);
        self.find_actor_key(actor_id, actor_type, key_name)?
            .ok_or_else(|| StoreError::NotFound(format!("key {key_name}")))
    }

    pub fn find_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
    ) -> Result<Option<ActorKey>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT actor_id, actor_type, key_name, public_key, expires_at
                 FROM actor_keys
                 WHERE actor_id = ?1 AND actor_type = ?2 AND key_name = ?3",
                params![actor_id, actor_type.as_str(), key_name],
                actor_key_from_row,
            )
            .optional()?)
    }

    /// Update a key. Omitted fields are preserved. Renaming the `default` key
    /// is rejected because it is the canonical key for the actor.
    pub fn update_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        new_key_name: Option<&str>,
        public_key: Option<&str>,
        expires_at: Option<&str>,
    ) -> Result<ActorKey, StoreError> {
        let current = self
            .find_actor_key(actor_id, actor_type, key_name)?
            .ok_or_else(|| StoreError::NotFound(format!("key {key_name}")))?;

        if let Some(new_key_name) = new_key_name {
            if new_key_name.is_empty() {
                return Err(StoreError::Invalid("key name must not be empty".into()));
            }
            if key_name == "default" && new_key_name != "default" {
                return Err(StoreError::Invalid(
                    "the default key cannot be renamed".into(),
                ));
            }
        }
        if public_key.is_some_and(str::is_empty) {
            return Err(StoreError::Invalid("public key must not be empty".into()));
        }

        let now = now();
        let connection = self.connection();
        connection
            .execute(
                "UPDATE actor_keys
                 SET key_name = COALESCE(?3, key_name),
                     public_key = COALESCE(?4, public_key),
                     key_version = CASE WHEN ?4 IS NULL THEN key_version ELSE 1 END,
                     expires_at = COALESCE(?5, expires_at),
                     updated_at = ?6
                 WHERE actor_id = ?1 AND actor_type = ?2 AND key_name = ?7",
                params![
                    actor_id,
                    actor_type.as_str(),
                    new_key_name,
                    public_key,
                    expires_at,
                    now,
                    key_name
                ],
            )
            .map_err(|error| conflict(error, format!("key {key_name} already exists")))?;
        drop(connection);

        let updated_name = new_key_name.unwrap_or(&current.key_name);
        self.find_actor_key(actor_id, actor_type, updated_name)?
            .ok_or_else(|| StoreError::NotFound(format!("key {updated_name}")))
    }

    pub fn delete_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection();
        let changed = connection.execute(
            "DELETE FROM actor_keys
             WHERE actor_id = ?1 AND actor_type = ?2 AND key_name = ?3",
            params![actor_id, actor_type.as_str(), key_name],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("key {key_name}")));
        }
        Ok(())
    }

    pub fn find_actor_keys(
        &self,
        actor_id: &str,
        actor_type: ActorType,
    ) -> Result<Vec<ActorKey>, StoreError> {
        self.list_actor_keys(actor_id, actor_type)
    }

    /// List all keys for an actor, including expired keys and their expiry state.
    pub fn list_actor_keys(
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
        let rows =
            statement.query_map(params![actor_id, actor_type.as_str()], actor_key_from_row)?;

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
        self.list_users_filtered(None, None)
    }

    /// List users, optionally filtered by case-insensitive email or exact
    /// external authentication uid.
    pub fn list_users_filtered(
        &self,
        email: Option<&str>,
        external_authentication_uid: Option<&str>,
    ) -> Result<Vec<User>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, authz_id, username, email, password_hash, password_salt,
                    hash_type, admin, recovery_authentication_enabled,
                    external_authentication_uid, display_name, first_name, middle_name,
                    last_name, created_at, updated_at
             FROM users
             WHERE (?1 IS NULL OR lower(email) = lower(?1))
               AND (?2 IS NULL OR external_authentication_uid = ?2)
             ORDER BY username",
        )?;
        let rows =
            statement.query_map(params![email, external_authentication_uid], user_from_row)?;

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
        self.update_user_patch(
            username,
            &UserPatch {
                email: email.map(ToString::to_string),
                password: password.map(|(hash, salt, hash_type)| {
                    (hash.to_string(), salt.to_string(), hash_type.to_string())
                }),
                admin,
                ..Default::default()
            },
        )
    }

    /// Apply a validated partial update, including optional renames. Email
    /// uniqueness is compared case-insensitively, matching upstream's
    /// functional index on `lower(email)`.
    pub fn update_user_patch(&self, username: &str, patch: &UserPatch) -> Result<User, StoreError> {
        let current = self
            .find_user(username)?
            .ok_or_else(|| StoreError::NotFound(format!("user {username}")))?;
        let new_username = patch
            .new_username
            .as_deref()
            .unwrap_or(current.username.as_str());
        let new_email = patch
            .email
            .as_ref()
            .map(|email| email.to_lowercase())
            .or(current.email.clone());

        if new_username != current.username && self.find_user(new_username)?.is_some() {
            return Err(StoreError::Conflict(
                "Username or email address already in use.".into(),
            ));
        }
        if let Some(email) = new_email.as_deref() {
            let taken: i64 = self.connection().query_row(
                "SELECT EXISTS (
                     SELECT 1 FROM users WHERE lower(email) = lower(?1) AND id <> ?2
                 )",
                params![email, current.id],
                |row| row.get(0),
            )?;
            if taken != 0 {
                return Err(StoreError::Conflict(
                    "Username or email address already in use.".into(),
                ));
            }
        }

        let now = now();
        let profile =
            |value: &Option<Option<String>>| (value.is_some() as i64, value.clone().flatten());
        let (ext_set, ext_value) = profile(&patch.external_authentication_uid);
        let (display_set, display_value) = profile(&patch.display_name);
        let (first_set, first_value) = profile(&patch.first_name);
        let (middle_set, middle_value) = profile(&patch.middle_name);
        let (last_set, last_value) = profile(&patch.last_name);
        let changed = self.connection().execute(
            "UPDATE users
             SET username = ?2,
                 email = ?3,
                 password_hash = COALESCE(?4, password_hash),
                 password_salt = COALESCE(?5, password_salt),
                 hash_type = COALESCE(?6, hash_type),
                 admin = COALESCE(?7, admin),
                 recovery_authentication_enabled = COALESCE(?8, recovery_authentication_enabled),
                 external_authentication_uid =
                     CASE WHEN ?9 THEN ?10 ELSE external_authentication_uid END,
                 display_name = CASE WHEN ?11 THEN ?12 ELSE display_name END,
                 first_name = CASE WHEN ?13 THEN ?14 ELSE first_name END,
                 middle_name = CASE WHEN ?15 THEN ?16 ELSE middle_name END,
                 last_name = CASE WHEN ?17 THEN ?18 ELSE last_name END,
                 updated_at = ?19
             WHERE id = ?1",
            params![
                current.id,
                new_username,
                new_email,
                patch.password.as_ref().map(|value| value.0.as_str()),
                patch.password.as_ref().map(|value| value.1.as_str()),
                patch.password.as_ref().map(|value| value.2.as_str()),
                patch.admin.map(|value| value as i64),
                patch
                    .recovery_authentication_enabled
                    .map(|value| value as i64),
                ext_set,
                ext_value,
                display_set,
                display_value,
                first_set,
                first_value,
                middle_set,
                middle_value,
                last_set,
                last_value,
                now
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("user {username}")));
        }
        self.find_user(new_username)?
            .ok_or_else(|| StoreError::NotFound(format!("user {new_username}")))
    }

    /// True when both users belong to at least one common organization.
    pub fn users_share_org(
        &self,
        left_user_id: &str,
        right_user_id: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connection();
        let shared: i64 = connection.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM org_user_associations left_assoc
                 JOIN org_user_associations right_assoc
                   ON left_assoc.org_id = right_assoc.org_id
                 WHERE left_assoc.user_id = ?1 AND right_assoc.user_id = ?2
             )",
            params![left_user_id, right_user_id],
            |row| row.get(0),
        )?;
        Ok(shared != 0)
    }

    /// Enable or disable password-based system recovery for a user.
    pub fn set_recovery_authentication_enabled(
        &self,
        username: &str,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let now = now();
        let connection = self.connection();
        let changed = connection.execute(
            "UPDATE users
             SET recovery_authentication_enabled = ?2, updated_at = ?3
             WHERE username = ?1",
            params![username, enabled as i64, now],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("user {username}")));
        }
        Ok(())
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

/// Decode the canonical user column order used by every user query.
pub(crate) fn user_from_row(row: &Row<'_>) -> rusqlite::Result<User> {
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
        external_authentication_uid: row.get(9)?,
        display_name: row.get(10)?,
        first_name: row.get(11)?,
        middle_name: row.get(12)?,
        last_name: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn actor_key_from_row(row: &Row<'_>) -> rusqlite::Result<ActorKey> {
    let actor_type = match row.get::<_, String>(1)?.as_str() {
        "user" => ActorType::User,
        _ => ActorType::Client,
    };
    let expires_at: String = row.get(4)?;
    Ok(ActorKey {
        actor_id: row.get(0)?,
        actor_type,
        key_name: row.get(2)?,
        public_key: row.get(3)?,
        expired: key_is_expired(&expires_at),
        expires_at,
    })
}

fn key_is_expired(expires_at: &str) -> bool {
    if expires_at == "infinity" {
        return false;
    }
    DateTime::parse_from_rfc3339(expires_at)
        .map(|expires_at| expires_at.with_timezone(&Utc) <= Utc::now())
        .unwrap_or(false)
}

pub(crate) fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}

pub(crate) fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn conflict(error: rusqlite::Error, message: String) -> StoreError {
    if let rusqlite::Error::SqliteFailure(inner, _) = &error {
        if inner.code == rusqlite::ErrorCode::ConstraintViolation {
            return StoreError::Conflict(message);
        }
    }
    StoreError::Sqlite(error)
}
