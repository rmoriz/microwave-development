use async_trait::async_trait;
use chrono::{DateTime, Utc};
use microwave_store::{
    ActorKey, ActorType, Client, IdentityRepository, Org, Principal, StoreError, User, UserPatch,
    UserProfile,
};
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::store::{db, new_id, now, unique, PostgresStore};

fn actor_type_str(actor_type: ActorType) -> &'static str {
    match actor_type {
        ActorType::User => "user",
        ActorType::Client => "client",
    }
}

pub(crate) fn user_from_row(row: &PgRow) -> Result<User, sqlx::Error> {
    Ok(User {
        id: row.try_get(0)?,
        authz_id: row.try_get(1)?,
        username: row.try_get(2)?,
        email: row.try_get(3)?,
        password_hash: row.try_get(4)?,
        password_salt: row.try_get(5)?,
        hash_type: row.try_get(6)?,
        admin: row.try_get(7)?,
        recovery_authentication_enabled: row.try_get(8)?,
        external_authentication_uid: row.try_get(9)?,
        display_name: row.try_get(10)?,
        first_name: row.try_get(11)?,
        middle_name: row.try_get(12)?,
        last_name: row.try_get(13)?,
        created_at: row.try_get(14)?,
        updated_at: row.try_get(15)?,
    })
}

fn client_from_row(row: &PgRow) -> Result<Client, sqlx::Error> {
    Ok(Client {
        id: row.try_get(0)?,
        org_id: row.try_get(1)?,
        authz_id: row.try_get(2)?,
        name: row.try_get(3)?,
        validator: row.try_get(4)?,
        admin: row.try_get(5)?,
        created_at: row.try_get(6)?,
        updated_at: row.try_get(7)?,
    })
}

fn actor_key_from_row(row: &PgRow) -> Result<ActorKey, sqlx::Error> {
    let actor_type = match row.try_get::<String, _>(1)?.as_str() {
        "user" => ActorType::User,
        _ => ActorType::Client,
    };
    let expires_at: String = row.try_get(4)?;
    Ok(ActorKey {
        actor_id: row.try_get(0)?,
        actor_type,
        key_name: row.try_get(2)?,
        public_key: row.try_get(3)?,
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

const USER_COLUMNS: &str = "id, authz_id, username, email, password_hash, password_salt, \
     hash_type, admin, recovery_authentication_enabled, external_authentication_uid, \
     display_name, first_name, middle_name, last_name, created_at, updated_at";

#[async_trait]
#[allow(clippy::too_many_arguments)]
impl IdentityRepository for PostgresStore {
    async fn create_org(&self, name: &str, full_name: &str) -> Result<Org, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;

        sqlx::query(
            "INSERT INTO orgs
             (id, authz_id, name, full_name, assigned_at, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, '', $5, $5)",
        )
        .bind(&id)
        .bind(&authz_id)
        .bind(name)
        .bind(full_name)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, format!("organization {name} already exists")))?;

        sqlx::query(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES ($1, 'actor', $2, $3)",
        )
        .bind(&authz_id)
        .bind(&id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        transaction.commit().await.map_err(db)?;
        Ok(Org {
            id,
            authz_id,
            name: name.into(),
            full_name: full_name.into(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
    }

    async fn update_org(&self, name: &str, full_name: &str) -> Result<Org, StoreError> {
        let timestamp = now();
        let result = sqlx::query("UPDATE orgs SET full_name = $2, updated_at = $3 WHERE name = $1")
            .bind(name)
            .bind(full_name)
            .bind(&timestamp)
            .execute(self.pool())
            .await
            .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("organization {name}")));
        }
        self.find_org(name)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("organization {name}")))
    }

    async fn find_org(&self, name: &str) -> Result<Option<Org>, StoreError> {
        let row = sqlx::query(
            "SELECT id, authz_id, name, full_name, created_at, updated_at
             FROM orgs WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| crate::store::org_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn list_orgs(&self) -> Result<Vec<Org>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, authz_id, name, full_name, created_at, updated_at
             FROM orgs ORDER BY name",
        )
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(crate::store::org_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn find_org_by_id(&self, id: &str) -> Result<Option<Org>, StoreError> {
        let row = sqlx::query(
            "SELECT id, authz_id, name, full_name, created_at, updated_at
             FROM orgs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| crate::store::org_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn create_user(
        &self,
        username: &str,
        email: &str,
        hash_type: &str,
        password_hash: &str,
        password_salt: &str,
        admin: bool,
    ) -> Result<User, StoreError> {
        let profile = UserProfile {
            display_name: Some(username.into()),
            first_name: Some(username.into()),
            last_name: Some(username.into()),
            ..Default::default()
        };
        self.create_user_with_profile(
            username,
            Some(email),
            hash_type,
            password_hash,
            password_salt,
            admin,
            &profile,
        )
        .await
    }

    async fn create_user_with_profile(
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
        let timestamp = now();
        let email = email.map(str::to_lowercase);
        let mut transaction = self.pool().begin().await.map_err(db)?;

        if let Some(email) = email.as_deref() {
            let taken: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM users WHERE lower(email) = lower($1))",
            )
            .bind(email)
            .fetch_one(&mut *transaction)
            .await
            .map_err(db)?;
            if taken {
                return Err(StoreError::Conflict(
                    "Username or email address already in use.".into(),
                ));
            }
        }

        sqlx::query(
            "INSERT INTO users
             (id, authz_id, username, email, password_hash, password_salt, hash_type,
              admin, recovery_authentication_enabled, external_authentication_uid,
              display_name, first_name, middle_name, last_name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, FALSE, $9, $10, $11, $12, $13, $14, $14)",
        )
        .bind(&id)
        .bind(&authz_id)
        .bind(username)
        .bind(&email)
        .bind(password_hash)
        .bind(password_salt)
        .bind(hash_type)
        .bind(admin)
        .bind(&profile.external_authentication_uid)
        .bind(&profile.display_name)
        .bind(&profile.first_name)
        .bind(&profile.middle_name)
        .bind(&profile.last_name)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            unique(
                error,
                "Username or email address already in use.".to_string(),
            )
        })?;

        sqlx::query(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES ($1, 'actor', NULL, $2)",
        )
        .bind(&authz_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        transaction.commit().await.map_err(db)?;
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
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
    }

    async fn find_user(&self, username: &str) -> Result<Option<User>, StoreError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {USER_COLUMNS} FROM users WHERE username = $1"
        )))
        .bind(username)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| user_from_row(&row)).transpose().map_err(db)
    }

    async fn find_user_by_id(&self, id: &str) -> Result<Option<User>, StoreError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {USER_COLUMNS} FROM users WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| user_from_row(&row)).transpose().map_err(db)
    }

    async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        self.list_users_filtered(None, None).await
    }

    async fn list_users_filtered(
        &self,
        email: Option<&str>,
        external_authentication_uid: Option<&str>,
    ) -> Result<Vec<User>, StoreError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {USER_COLUMNS}
             FROM users
             WHERE ($1::text IS NULL OR lower(email) = lower($1))
               AND ($2::text IS NULL OR external_authentication_uid = $2)
             ORDER BY username"
        )))
        .bind(email)
        .bind(external_authentication_uid)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(user_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn update_user(
        &self,
        username: &str,
        email: Option<&str>,
        password: Option<(&str, &str, &str)>,
        admin: Option<bool>,
    ) -> Result<User, StoreError> {
        let patch = UserPatch {
            email: email.map(ToString::to_string),
            password: password.map(|(hash, salt, hash_type)| {
                (hash.to_string(), salt.to_string(), hash_type.to_string())
            }),
            admin,
            ..Default::default()
        };
        self.update_user_patch(username, &patch).await
    }

    async fn update_user_patch(
        &self,
        username: &str,
        patch: &UserPatch,
    ) -> Result<User, StoreError> {
        let current = self
            .find_user(username)
            .await?
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

        if new_username != current.username && self.find_user(new_username).await?.is_some() {
            return Err(StoreError::Conflict(
                "Username or email address already in use.".into(),
            ));
        }
        if let Some(email) = new_email.as_deref() {
            let taken: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM users WHERE lower(email) = lower($1) AND id <> $2
                 )",
            )
            .bind(email)
            .bind(&current.id)
            .fetch_one(self.pool())
            .await
            .map_err(db)?;
            if taken {
                return Err(StoreError::Conflict(
                    "Username or email address already in use.".into(),
                ));
            }
        }

        let timestamp = now();
        let profile = |value: &Option<Option<String>>| (value.is_some(), value.clone().flatten());
        let (ext_set, ext_value) = profile(&patch.external_authentication_uid);
        let (display_set, display_value) = profile(&patch.display_name);
        let (first_set, first_value) = profile(&patch.first_name);
        let (middle_set, middle_value) = profile(&patch.middle_name);
        let (last_set, last_value) = profile(&patch.last_name);
        let result = sqlx::query(
            "UPDATE users
             SET username = $2,
                 email = $3,
                 password_hash = COALESCE($4, password_hash),
                 password_salt = COALESCE($5, password_salt),
                 hash_type = COALESCE($6, hash_type),
                 admin = COALESCE($7, admin),
                 recovery_authentication_enabled = COALESCE($8, recovery_authentication_enabled),
                 external_authentication_uid =
                     CASE WHEN $9 THEN $10 ELSE external_authentication_uid END,
                 display_name = CASE WHEN $11 THEN $12 ELSE display_name END,
                 first_name = CASE WHEN $13 THEN $14 ELSE first_name END,
                 middle_name = CASE WHEN $15 THEN $16 ELSE middle_name END,
                 last_name = CASE WHEN $17 THEN $18 ELSE last_name END,
                 updated_at = $19
             WHERE id = $1",
        )
        .bind(&current.id)
        .bind(new_username)
        .bind(&new_email)
        .bind(patch.password.as_ref().map(|value| value.0.as_str()))
        .bind(patch.password.as_ref().map(|value| value.1.as_str()))
        .bind(patch.password.as_ref().map(|value| value.2.as_str()))
        .bind(patch.admin)
        .bind(patch.recovery_authentication_enabled)
        .bind(ext_set)
        .bind(ext_value)
        .bind(display_set)
        .bind(display_value)
        .bind(first_set)
        .bind(first_value)
        .bind(middle_set)
        .bind(middle_value)
        .bind(last_set)
        .bind(last_value)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(|error| {
            unique(
                error,
                "Username or email address already in use.".to_string(),
            )
        })?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("user {username}")));
        }
        self.find_user(new_username)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("user {new_username}")))
    }

    async fn users_share_org(
        &self,
        left_user_id: &str,
        right_user_id: &str,
    ) -> Result<bool, StoreError> {
        let shared: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                 FROM org_user_associations left_assoc
                 JOIN org_user_associations right_assoc
                   ON left_assoc.org_id = right_assoc.org_id
                 WHERE left_assoc.user_id = $1 AND right_assoc.user_id = $2
             )",
        )
        .bind(left_user_id)
        .bind(right_user_id)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        Ok(shared)
    }

    async fn set_recovery_authentication_enabled(
        &self,
        username: &str,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let timestamp = now();
        let result = sqlx::query(
            "UPDATE users
             SET recovery_authentication_enabled = $2, updated_at = $3
             WHERE username = $1",
        )
        .bind(username)
        .bind(enabled)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("user {username}")));
        }
        Ok(())
    }

    async fn delete_user(&self, username: &str) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let row = sqlx::query("SELECT id, authz_id FROM users WHERE username = $1")
            .bind(username)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(db)?
            .ok_or_else(|| StoreError::NotFound(format!("user {username}")))?;
        let id: String = row.try_get(0).map_err(db)?;
        let authz_id: String = row.try_get(1).map_err(db)?;

        sqlx::query("DELETE FROM actor_keys WHERE actor_id = $1 AND actor_type = 'user'")
            .bind(&id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
            .bind(&authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(&id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn create_client(
        &self,
        org_id: &str,
        name: &str,
        validator: bool,
        admin: bool,
    ) -> Result<Client, StoreError> {
        let id = new_id();
        let authz_id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;

        sqlx::query(
            "INSERT INTO clients
             (id, org_id, authz_id, name, validator, admin, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
        )
        .bind(&id)
        .bind(org_id)
        .bind(&authz_id)
        .bind(name)
        .bind(validator)
        .bind(admin)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, format!("client {name} already exists")))?;

        sqlx::query(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES ($1, 'actor', $2, $3)",
        )
        .bind(&authz_id)
        .bind(org_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        transaction.commit().await.map_err(db)?;
        Ok(Client {
            id,
            org_id: org_id.into(),
            authz_id,
            name: name.into(),
            validator,
            admin,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
    }

    async fn create_client_with_acl(
        &self,
        org_id: &str,
        name: &str,
        validator: bool,
        admin: bool,
        creator_authz_id: Option<&str>,
    ) -> Result<Client, StoreError> {
        let id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;

        let authz_id =
            crate::objects::create_acl_object(&mut transaction, org_id, "clients").await?;
        sqlx::query(
            "INSERT INTO clients
             (id, org_id, authz_id, name, validator, admin, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
        )
        .bind(&id)
        .bind(org_id)
        .bind(&authz_id)
        .bind(name)
        .bind(validator)
        .bind(admin)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unique(error, format!("client {name} already exists")))?;
        let granted = creator_authz_id.unwrap_or(&authz_id);
        crate::objects::grant_actor_all(&mut transaction, &authz_id, granted).await?;
        transaction.commit().await.map_err(db)?;

        Ok(Client {
            id,
            org_id: org_id.into(),
            authz_id,
            name: name.into(),
            validator,
            admin,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
    }

    async fn update_client(
        &self,
        org_id: &str,
        name: &str,
        new_name: &str,
        validator: Option<bool>,
    ) -> Result<Client, StoreError> {
        let timestamp = now();
        let result = sqlx::query(
            "UPDATE clients
             SET name = $3,
                 validator = COALESCE($4, validator),
                 updated_at = $5
             WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .bind(new_name)
        .bind(validator)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(|error| unique(error, format!("client {new_name} already exists")))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("client {name}")));
        }
        self.find_client(org_id, new_name)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("client {new_name}")))
    }

    async fn find_client(&self, org_id: &str, name: &str) -> Result<Option<Client>, StoreError> {
        let row = sqlx::query(
            "SELECT id, org_id, authz_id, name, validator, admin, created_at, updated_at
             FROM clients WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| client_from_row(&row)).transpose().map_err(db)
    }

    async fn list_clients(&self, org_id: &str) -> Result<Vec<Client>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, org_id, authz_id, name, validator, admin, created_at, updated_at
             FROM clients WHERE org_id = $1 ORDER BY name",
        )
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(client_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn delete_client(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let row = sqlx::query("SELECT id, authz_id FROM clients WHERE org_id = $1 AND name = $2")
            .bind(org_id)
            .bind(name)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(db)?
            .ok_or_else(|| StoreError::NotFound(format!("client {name}")))?;
        let id: String = row.try_get(0).map_err(db)?;
        let authz_id: String = row.try_get(1).map_err(db)?;

        sqlx::query("DELETE FROM actor_keys WHERE actor_id = $1 AND actor_type = 'client'")
            .bind(&id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
            .bind(&authz_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM clients WHERE id = $1")
            .bind(&id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn add_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        public_key: &str,
    ) -> Result<(), StoreError> {
        self.create_actor_key(actor_id, actor_type, key_name, public_key, "infinity")
            .await
            .map(|_| ())
    }

    async fn update_or_create_default_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        public_key: &str,
    ) -> Result<ActorKey, StoreError> {
        if self
            .find_actor_key(actor_id, actor_type, "default")
            .await?
            .is_some()
        {
            // Replacing the default key resets its lifetime; the upstream API
            // has no way to set an expiry through client/user PUTs.
            self.update_actor_key(
                actor_id,
                actor_type,
                "default",
                None,
                Some(public_key),
                Some("infinity"),
            )
            .await
        } else {
            self.create_actor_key(actor_id, actor_type, "default", public_key, "infinity")
                .await
        }
    }

    async fn create_actor_key(
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
        let timestamp = now();
        sqlx::query(
            "INSERT INTO actor_keys
             (actor_id, actor_type, key_name, public_key, key_version, expires_at,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, 1, $5, '', $6, $6)",
        )
        .bind(actor_id)
        .bind(actor_type_str(actor_type))
        .bind(key_name)
        .bind(public_key)
        .bind(expires_at)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(|error| unique(error, format!("key {key_name} already exists")))?;

        self.find_actor_key(actor_id, actor_type, key_name)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("key {key_name}")))
    }

    async fn find_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
    ) -> Result<Option<ActorKey>, StoreError> {
        let row = sqlx::query(
            "SELECT actor_id, actor_type, key_name, public_key, expires_at
             FROM actor_keys
             WHERE actor_id = $1 AND actor_type = $2 AND key_name = $3",
        )
        .bind(actor_id)
        .bind(actor_type_str(actor_type))
        .bind(key_name)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| actor_key_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn update_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        new_key_name: Option<&str>,
        public_key: Option<&str>,
        expires_at: Option<&str>,
    ) -> Result<ActorKey, StoreError> {
        let current = self
            .find_actor_key(actor_id, actor_type, key_name)
            .await?
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

        let timestamp = now();
        sqlx::query(
            "UPDATE actor_keys
             SET key_name = COALESCE($3, key_name),
                 public_key = COALESCE($4, public_key),
                 key_version = CASE WHEN $4::text IS NULL THEN key_version ELSE 1 END,
                 expires_at = COALESCE($5, expires_at),
                 updated_at = $6
             WHERE actor_id = $1 AND actor_type = $2 AND key_name = $7",
        )
        .bind(actor_id)
        .bind(actor_type_str(actor_type))
        .bind(new_key_name)
        .bind(public_key)
        .bind(expires_at)
        .bind(&timestamp)
        .bind(key_name)
        .execute(self.pool())
        .await
        .map_err(|error| unique(error, format!("key {key_name} already exists")))?;

        let updated_name = new_key_name.unwrap_or(&current.key_name);
        self.find_actor_key(actor_id, actor_type, updated_name)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("key {updated_name}")))
    }

    async fn delete_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "DELETE FROM actor_keys
             WHERE actor_id = $1 AND actor_type = $2 AND key_name = $3",
        )
        .bind(actor_id)
        .bind(actor_type_str(actor_type))
        .bind(key_name)
        .execute(self.pool())
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("key {key_name}")));
        }
        Ok(())
    }

    async fn find_actor_keys(
        &self,
        actor_id: &str,
        actor_type: ActorType,
    ) -> Result<Vec<ActorKey>, StoreError> {
        self.list_actor_keys(actor_id, actor_type).await
    }

    async fn list_actor_keys(
        &self,
        actor_id: &str,
        actor_type: ActorType,
    ) -> Result<Vec<ActorKey>, StoreError> {
        let rows = sqlx::query(
            "SELECT actor_id, actor_type, key_name, public_key, expires_at
             FROM actor_keys
             WHERE actor_id = $1 AND actor_type = $2
             ORDER BY key_name",
        )
        .bind(actor_id)
        .bind(actor_type_str(actor_type))
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(actor_key_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn find_principals(
        &self,
        org_id: Option<&str>,
        name: &str,
    ) -> Result<Vec<Principal>, StoreError> {
        let mut principals = Vec::new();

        if let Some(org_id) = org_id {
            if let Some(client) = self.find_client(org_id, name).await? {
                principals.push(Principal {
                    actor_id: client.id,
                    authz_id: client.authz_id,
                    name: client.name,
                    actor_type: ActorType::Client,
                    org_id: Some(org_id.to_string()),
                });
            }
        }

        if let Some(user) = self.find_user(name).await? {
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

    async fn find_principal_keys(
        &self,
        principal: &Principal,
    ) -> Result<Vec<ActorKey>, StoreError> {
        self.find_actor_keys(&principal.actor_id, principal.actor_type)
            .await
    }
}
