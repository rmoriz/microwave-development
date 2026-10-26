use crate::identity::{new_id, now};
use crate::sqlite::{SqliteStore, StoreError};
use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension, Row};

/// An OAuth application registered with the identity service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthApplication {
    pub id: i64,
    pub name: String,
    pub uid: String,
    pub secret: String,
    pub redirect_uri: String,
    pub scopes: String,
    pub confidential: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A pending authorization-code grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAccessGrant {
    pub id: i64,
    pub resource_owner_id: String,
    pub application_id: i64,
    pub token: String,
    pub expires_in: i64,
    pub redirect_uri: String,
    pub scopes: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

/// An OAuth bearer token and, optionally, its refresh token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAccessToken {
    pub id: i64,
    pub resource_owner_id: Option<String>,
    pub application_id: Option<i64>,
    pub token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub scopes: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

const APPLICATION_COLUMNS: &str = "id, name, uid, secret, redirect_uri, scopes, confidential,
     created_at, updated_at";

const GRANT_COLUMNS: &str = "id, resource_owner_id, application_id, token, expires_in,
     redirect_uri, scopes, created_at, revoked_at";

const TOKEN_COLUMNS: &str = "id, resource_owner_id, application_id, token, refresh_token,
     expires_in, scopes, created_at, revoked_at";

impl SqliteStore {
    pub fn create_oauth_application(
        &self,
        name: &str,
        redirect_uri: &str,
        scopes: &str,
        confidential: bool,
    ) -> Result<OAuthApplication, StoreError> {
        if name.trim().is_empty() {
            return Err(StoreError::Invalid("name must not be empty".into()));
        }
        if redirect_uri.trim().is_empty() {
            return Err(StoreError::Invalid("redirect_uri must not be empty".into()));
        }

        let uid = random_token();
        let secret = random_token();
        let timestamp = now();
        let connection = self.connection();
        connection.execute(
            "INSERT INTO oauth_applications
             (name, uid, secret, redirect_uri, scopes, confidential, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                name.trim(),
                uid,
                secret,
                redirect_uri.trim(),
                scopes,
                confidential as i64,
                timestamp
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.find_oauth_application(id)?
            .ok_or_else(|| StoreError::NotFound(format!("oauth application {id}")))
    }

    pub fn find_oauth_application(&self, id: i64) -> Result<Option<OAuthApplication>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                &format!("SELECT {APPLICATION_COLUMNS} FROM oauth_applications WHERE id = ?1"),
                [id],
                application_from_row,
            )
            .optional()?)
    }

    pub fn find_oauth_application_by_uid(
        &self,
        uid: &str,
    ) -> Result<Option<OAuthApplication>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                &format!("SELECT {APPLICATION_COLUMNS} FROM oauth_applications WHERE uid = ?1"),
                [uid],
                application_from_row,
            )
            .optional()?)
    }

    pub fn list_oauth_applications(&self) -> Result<Vec<OAuthApplication>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(&format!(
            "SELECT {APPLICATION_COLUMNS} FROM oauth_applications ORDER BY name"
        ))?;
        let rows = statement.query_map([], application_from_row)?;
        let mut applications = Vec::new();
        for row in rows {
            applications.push(row?);
        }
        Ok(applications)
    }

    pub fn update_oauth_application(
        &self,
        id: i64,
        name: Option<&str>,
        redirect_uri: Option<&str>,
    ) -> Result<OAuthApplication, StoreError> {
        if name.is_some_and(|value| value.trim().is_empty()) {
            return Err(StoreError::Invalid("name must not be empty".into()));
        }
        if redirect_uri.is_some_and(|value| value.trim().is_empty()) {
            return Err(StoreError::Invalid("redirect_uri must not be empty".into()));
        }

        let connection = self.connection();
        let changed = connection.execute(
            "UPDATE oauth_applications
             SET name = COALESCE(?2, name),
                 redirect_uri = COALESCE(?3, redirect_uri),
                 updated_at = ?4
             WHERE id = ?1",
            params![id, name.map(str::trim), redirect_uri.map(str::trim), now()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("oauth application {id}")));
        }
        drop(connection);
        self.find_oauth_application(id)?
            .ok_or_else(|| StoreError::NotFound(format!("oauth application {id}")))
    }

    pub fn delete_oauth_application(&self, id: i64) -> Result<(), StoreError> {
        let connection = self.connection();
        let changed = connection.execute("DELETE FROM oauth_applications WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("oauth application {id}")));
        }
        Ok(())
    }

    pub fn create_oauth_access_grant(
        &self,
        resource_owner_id: &str,
        application_id: i64,
        redirect_uri: &str,
        scopes: &str,
        expires_in: i64,
    ) -> Result<OAuthAccessGrant, StoreError> {
        let token = random_token();
        let timestamp = now();
        let connection = self.connection();
        connection.execute(
            "INSERT INTO oauth_access_grants
             (resource_owner_id, application_id, token, expires_in, redirect_uri, scopes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                resource_owner_id,
                application_id,
                token,
                expires_in,
                redirect_uri,
                scopes,
                timestamp
            ],
        )?;
        drop(connection);
        self.find_oauth_access_grant(&token)?
            .ok_or_else(|| StoreError::NotFound("oauth access grant".into()))
    }

    pub fn find_oauth_access_grant(
        &self,
        token: &str,
    ) -> Result<Option<OAuthAccessGrant>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                &format!("SELECT {GRANT_COLUMNS} FROM oauth_access_grants WHERE token = ?1"),
                [token],
                grant_from_row,
            )
            .optional()?)
    }

    pub fn revoke_oauth_access_grant(&self, token: &str) -> Result<(), StoreError> {
        let connection = self.connection();
        connection.execute(
            "UPDATE oauth_access_grants SET revoked_at = COALESCE(revoked_at, ?2)
             WHERE token = ?1",
            params![token, now()],
        )?;
        Ok(())
    }

    pub fn create_oauth_access_token(
        &self,
        resource_owner_id: &str,
        application_id: i64,
        scopes: &str,
        expires_in: Option<i64>,
        include_refresh_token: bool,
    ) -> Result<OAuthAccessToken, StoreError> {
        let token = random_token();
        let refresh_token = include_refresh_token.then(random_token);
        let timestamp = now();
        let connection = self.connection();
        connection.execute(
            "INSERT INTO oauth_access_tokens
             (resource_owner_id, application_id, token, refresh_token, expires_in, scopes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                resource_owner_id,
                application_id,
                token,
                refresh_token,
                expires_in,
                scopes,
                timestamp
            ],
        )?;
        drop(connection);
        self.find_oauth_access_token(&token)?
            .ok_or_else(|| StoreError::NotFound("oauth access token".into()))
    }

    pub fn find_oauth_access_token(
        &self,
        token: &str,
    ) -> Result<Option<OAuthAccessToken>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                &format!("SELECT {TOKEN_COLUMNS} FROM oauth_access_tokens WHERE token = ?1"),
                [token],
                token_from_row,
            )
            .optional()?)
    }

    pub fn find_oauth_access_token_by_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<Option<OAuthAccessToken>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                &format!(
                    "SELECT {TOKEN_COLUMNS} FROM oauth_access_tokens WHERE refresh_token = ?1"
                ),
                [refresh_token],
                token_from_row,
            )
            .optional()?)
    }

    /// Active (not revoked) access tokens for a resource owner, newest first.
    pub fn list_oauth_access_tokens_for_owner(
        &self,
        resource_owner_id: &str,
    ) -> Result<Vec<OAuthAccessToken>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(&format!(
            "SELECT {TOKEN_COLUMNS} FROM oauth_access_tokens
             WHERE resource_owner_id = ?1 AND revoked_at IS NULL
             ORDER BY created_at DESC, id DESC"
        ))?;
        let rows = statement.query_map([resource_owner_id], token_from_row)?;
        let mut tokens = Vec::new();
        for row in rows {
            tokens.push(row?);
        }
        Ok(tokens)
    }

    /// Revoke every access token and pending grant for an application owned by
    /// one resource owner.
    pub fn revoke_oauth_tokens_and_grants_for(
        &self,
        application_id: i64,
        resource_owner_id: &str,
    ) -> Result<(), StoreError> {
        let timestamp = now();
        let connection = self.connection();
        connection.execute(
            "UPDATE oauth_access_tokens SET revoked_at = COALESCE(revoked_at, ?3)
             WHERE application_id = ?1 AND resource_owner_id = ?2",
            params![application_id, resource_owner_id, timestamp],
        )?;
        connection.execute(
            "UPDATE oauth_access_grants SET revoked_at = COALESCE(revoked_at, ?3)
             WHERE application_id = ?1 AND resource_owner_id = ?2",
            params![application_id, resource_owner_id, timestamp],
        )?;
        Ok(())
    }

    pub fn revoke_oauth_access_token(&self, token: &str) -> Result<(), StoreError> {
        let connection = self.connection();
        connection.execute(
            "UPDATE oauth_access_tokens SET revoked_at = COALESCE(revoked_at, ?2)
             WHERE token = ?1",
            params![token, now()],
        )?;
        Ok(())
    }

    pub fn revoke_oauth_access_token_by_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection();
        connection.execute(
            "UPDATE oauth_access_tokens SET revoked_at = COALESCE(revoked_at, ?2)
             WHERE refresh_token = ?1",
            params![refresh_token, now()],
        )?;
        Ok(())
    }

    /// Create an opaque browser session token for a user.
    pub fn create_id_session(&self, username: &str) -> Result<String, StoreError> {
        let token = random_token();
        let connection = self.connection();
        connection.execute(
            "DELETE FROM kvpair WHERE key LIKE 'id_session:%' AND updated_at < ?1",
            [session_cutoff()],
        )?;
        connection.execute(
            "INSERT INTO kvpair (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![session_key(&token), username, now()],
        )?;
        Ok(token)
    }

    pub fn find_id_session(&self, token: &str) -> Result<Option<String>, StoreError> {
        let connection = self.connection();
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT value, updated_at FROM kvpair WHERE key = ?1",
                [session_key(token)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((username, updated_at)) = row else {
            return Ok(None);
        };
        if updated_at < session_cutoff() {
            connection.execute("DELETE FROM kvpair WHERE key = ?1", [session_key(token)])?;
            return Ok(None);
        }
        Ok(Some(username))
    }

    pub fn delete_id_session(&self, token: &str) -> Result<(), StoreError> {
        let connection = self.connection();
        connection.execute("DELETE FROM kvpair WHERE key = ?1", [session_key(token)])?;
        Ok(())
    }

    /// Create a single-use password reset token for a user.
    pub fn create_password_reset_token(&self, username: &str) -> Result<String, StoreError> {
        let token = random_token();
        let connection = self.connection();
        connection.execute(
            "INSERT INTO kvpair (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![password_reset_key(&token), username, now()],
        )?;
        Ok(token)
    }

    pub fn find_password_reset_token(&self, token: &str) -> Result<Option<String>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT value FROM kvpair WHERE key = ?1",
                [password_reset_key(token)],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Consume a password reset token, returning the username it belongs to.
    pub fn consume_password_reset_token(&self, token: &str) -> Result<Option<String>, StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let username = transaction
            .query_row(
                "SELECT value FROM kvpair WHERE key = ?1",
                [password_reset_key(token)],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if username.is_some() {
            transaction.execute(
                "DELETE FROM kvpair WHERE key = ?1",
                [password_reset_key(token)],
            )?;
        }
        transaction.commit()?;
        Ok(username)
    }
}

pub const ID_SESSION_TTL_SECONDS: i64 = 86_400;

fn session_cutoff() -> String {
    (Utc::now() - Duration::seconds(ID_SESSION_TTL_SECONDS))
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn session_key(token: &str) -> String {
    format!("id_session:{token}")
}

fn password_reset_key(token: &str) -> String {
    format!("id_password_reset:{token}")
}

fn random_token() -> String {
    format!("{}{}", new_id(), new_id())
}

fn application_from_row(row: &Row<'_>) -> rusqlite::Result<OAuthApplication> {
    Ok(OAuthApplication {
        id: row.get(0)?,
        name: row.get(1)?,
        uid: row.get(2)?,
        secret: row.get(3)?,
        redirect_uri: row.get(4)?,
        scopes: row.get(5)?,
        confidential: row.get::<_, i64>(6)? != 0,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn grant_from_row(row: &Row<'_>) -> rusqlite::Result<OAuthAccessGrant> {
    Ok(OAuthAccessGrant {
        id: row.get(0)?,
        resource_owner_id: row.get(1)?,
        application_id: row.get(2)?,
        token: row.get(3)?,
        expires_in: row.get(4)?,
        redirect_uri: row.get(5)?,
        scopes: row.get(6)?,
        created_at: row.get(7)?,
        revoked_at: row.get(8)?,
    })
}

fn token_from_row(row: &Row<'_>) -> rusqlite::Result<OAuthAccessToken> {
    Ok(OAuthAccessToken {
        id: row.get(0)?,
        resource_owner_id: row.get(1)?,
        application_id: row.get(2)?,
        token: row.get(3)?,
        refresh_token: row.get(4)?,
        expires_in: row.get(5)?,
        scopes: row.get(6)?,
        created_at: row.get(7)?,
        revoked_at: row.get(8)?,
    })
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn expired_id_sessions_are_removed() {
        let store = SqliteStore::open_in_memory().unwrap();
        let token = store.create_id_session("alice").unwrap();
        store
            .connection()
            .execute(
                "UPDATE kvpair SET updated_at = '2000-01-01T00:00:00Z' WHERE key = ?1",
                [session_key(&token)],
            )
            .unwrap();
        assert_eq!(store.find_id_session(&token).unwrap(), None);
        let remaining: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM kvpair WHERE key = ?1",
                [session_key(&token)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }
}
