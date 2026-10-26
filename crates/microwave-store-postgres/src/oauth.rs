use async_trait::async_trait;
use chrono::{Duration, SecondsFormat, Utc};
use microwave_store::{
    OAuthAccessGrant, OAuthAccessToken, OAuthApplication, OAuthRepository, StoreError,
};
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::store::{db, new_id, now, PostgresStore};

const APPLICATION_COLUMNS: &str = "id, name, uid, secret, redirect_uri, scopes, confidential,
     created_at, updated_at";

const GRANT_COLUMNS: &str = "id, resource_owner_id, application_id, token, expires_in,
     redirect_uri, scopes, created_at, revoked_at";

const TOKEN_COLUMNS: &str = "id, resource_owner_id, application_id, token, refresh_token,
     expires_in, scopes, created_at, revoked_at";

fn application_from_row(row: &PgRow) -> Result<OAuthApplication, sqlx::Error> {
    Ok(OAuthApplication {
        id: row.try_get(0)?,
        name: row.try_get(1)?,
        uid: row.try_get(2)?,
        secret: row.try_get(3)?,
        redirect_uri: row.try_get(4)?,
        scopes: row.try_get(5)?,
        confidential: row.try_get(6)?,
        created_at: row.try_get(7)?,
        updated_at: row.try_get(8)?,
    })
}

fn grant_from_row(row: &PgRow) -> Result<OAuthAccessGrant, sqlx::Error> {
    Ok(OAuthAccessGrant {
        id: row.try_get(0)?,
        resource_owner_id: row.try_get(1)?,
        application_id: row.try_get(2)?,
        token: row.try_get(3)?,
        expires_in: row.try_get(4)?,
        redirect_uri: row.try_get(5)?,
        scopes: row.try_get(6)?,
        created_at: row.try_get(7)?,
        revoked_at: row.try_get(8)?,
    })
}

fn token_from_row(row: &PgRow) -> Result<OAuthAccessToken, sqlx::Error> {
    Ok(OAuthAccessToken {
        id: row.try_get(0)?,
        resource_owner_id: row.try_get(1)?,
        application_id: row.try_get(2)?,
        token: row.try_get(3)?,
        refresh_token: row.try_get(4)?,
        expires_in: row.try_get(5)?,
        scopes: row.try_get(6)?,
        created_at: row.try_get(7)?,
        revoked_at: row.try_get(8)?,
    })
}

fn random_token() -> String {
    format!("{}{}", new_id(), new_id())
}

fn session_cutoff() -> String {
    (Utc::now() - Duration::seconds(microwave_store::ID_SESSION_TTL_SECONDS))
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn password_reset_cutoff(max_age_seconds: i64) -> String {
    (Utc::now() - Duration::seconds(max_age_seconds.max(0)))
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn session_key(token: &str) -> String {
    format!("id_session:{token}")
}

fn password_reset_key(token: &str) -> String {
    format!("id_password_reset:{token}")
}

#[async_trait]
impl OAuthRepository for PostgresStore {
    async fn create_oauth_application(
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
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO oauth_applications
             (name, uid, secret, redirect_uri, scopes, confidential, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
             RETURNING id",
        )
        .bind(name.trim())
        .bind(&uid)
        .bind(&secret)
        .bind(redirect_uri.trim())
        .bind(scopes)
        .bind(confidential)
        .bind(&timestamp)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        self.find_oauth_application(id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("oauth application {id}")))
    }

    async fn find_oauth_application(
        &self,
        id: i64,
    ) -> Result<Option<OAuthApplication>, StoreError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {APPLICATION_COLUMNS} FROM oauth_applications WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| application_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn find_oauth_application_by_uid(
        &self,
        uid: &str,
    ) -> Result<Option<OAuthApplication>, StoreError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {APPLICATION_COLUMNS} FROM oauth_applications WHERE uid = $1"
        )))
        .bind(uid)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| application_from_row(&row))
            .transpose()
            .map_err(db)
    }

    async fn list_oauth_applications(&self) -> Result<Vec<OAuthApplication>, StoreError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {APPLICATION_COLUMNS} FROM oauth_applications ORDER BY name"
        )))
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(application_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn update_oauth_application(
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

        let changed = sqlx::query(
            "UPDATE oauth_applications
             SET name = COALESCE($2, name),
                 redirect_uri = COALESCE($3, redirect_uri),
                 updated_at = $4
             WHERE id = $1",
        )
        .bind(id)
        .bind(name.map(str::trim))
        .bind(redirect_uri.map(str::trim))
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db)?;
        if changed.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("oauth application {id}")));
        }
        self.find_oauth_application(id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("oauth application {id}")))
    }

    async fn delete_oauth_application(&self, id: i64) -> Result<(), StoreError> {
        let changed = sqlx::query("DELETE FROM oauth_applications WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(db)?;
        if changed.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("oauth application {id}")));
        }
        Ok(())
    }

    async fn create_oauth_access_grant(
        &self,
        resource_owner_id: &str,
        application_id: i64,
        redirect_uri: &str,
        scopes: &str,
        expires_in: i64,
    ) -> Result<OAuthAccessGrant, StoreError> {
        let token = random_token();
        let timestamp = now();
        sqlx::query(
            "INSERT INTO oauth_access_grants
             (resource_owner_id, application_id, token, expires_in, redirect_uri, scopes, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(resource_owner_id)
        .bind(application_id)
        .bind(&token)
        .bind(expires_in)
        .bind(redirect_uri)
        .bind(scopes)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(db)?;
        self.find_oauth_access_grant(&token)
            .await?
            .ok_or_else(|| StoreError::NotFound("oauth access grant".into()))
    }

    async fn find_oauth_access_grant(
        &self,
        token: &str,
    ) -> Result<Option<OAuthAccessGrant>, StoreError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {GRANT_COLUMNS} FROM oauth_access_grants WHERE token = $1"
        )))
        .bind(token)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| grant_from_row(&row)).transpose().map_err(db)
    }

    async fn revoke_oauth_access_grant(&self, token: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE oauth_access_grants SET revoked_at = COALESCE(revoked_at, $2)
             WHERE token = $1",
        )
        .bind(token)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn create_oauth_access_token(
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
        sqlx::query(
            "INSERT INTO oauth_access_tokens
             (resource_owner_id, application_id, token, refresh_token, expires_in, scopes, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(resource_owner_id)
        .bind(application_id)
        .bind(&token)
        .bind(refresh_token.as_deref())
        .bind(expires_in)
        .bind(scopes)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(db)?;
        self.find_oauth_access_token(&token)
            .await?
            .ok_or_else(|| StoreError::NotFound("oauth access token".into()))
    }

    async fn find_oauth_access_token(
        &self,
        token: &str,
    ) -> Result<Option<OAuthAccessToken>, StoreError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {TOKEN_COLUMNS} FROM oauth_access_tokens WHERE token = $1"
        )))
        .bind(token)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| token_from_row(&row)).transpose().map_err(db)
    }

    async fn find_oauth_access_token_by_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<Option<OAuthAccessToken>, StoreError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {TOKEN_COLUMNS} FROM oauth_access_tokens WHERE refresh_token = $1"
        )))
        .bind(refresh_token)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.map(|row| token_from_row(&row)).transpose().map_err(db)
    }

    async fn list_oauth_access_tokens_for_owner(
        &self,
        resource_owner_id: &str,
    ) -> Result<Vec<OAuthAccessToken>, StoreError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {TOKEN_COLUMNS} FROM oauth_access_tokens
             WHERE resource_owner_id = $1 AND revoked_at IS NULL
             ORDER BY created_at DESC, id DESC"
        )))
        .bind(resource_owner_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter()
            .map(token_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    async fn revoke_oauth_tokens_and_grants_for(
        &self,
        application_id: i64,
        resource_owner_id: &str,
    ) -> Result<(), StoreError> {
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        sqlx::query(
            "UPDATE oauth_access_tokens SET revoked_at = COALESCE(revoked_at, $3)
             WHERE application_id = $1 AND resource_owner_id = $2",
        )
        .bind(application_id)
        .bind(resource_owner_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        sqlx::query(
            "UPDATE oauth_access_grants SET revoked_at = COALESCE(revoked_at, $3)
             WHERE application_id = $1 AND resource_owner_id = $2",
        )
        .bind(application_id)
        .bind(resource_owner_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn revoke_oauth_access_token(&self, token: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE oauth_access_tokens SET revoked_at = COALESCE(revoked_at, $2)
             WHERE token = $1",
        )
        .bind(token)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn revoke_oauth_access_token_by_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE oauth_access_tokens SET revoked_at = COALESCE(revoked_at, $2)
             WHERE refresh_token = $1",
        )
        .bind(refresh_token)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn create_id_session(&self, username: &str) -> Result<String, StoreError> {
        let token = random_token();
        sqlx::query("DELETE FROM kvpair WHERE key LIKE 'id_session:%' AND updated_at < $1")
            .bind(session_cutoff())
            .execute(self.pool())
            .await
            .map_err(db)?;
        sqlx::query("INSERT INTO kvpair (key, value, updated_at) VALUES ($1, $2, $3)")
            .bind(session_key(&token))
            .bind(username)
            .bind(now())
            .execute(self.pool())
            .await
            .map_err(db)?;
        Ok(token)
    }

    async fn find_id_session(&self, token: &str) -> Result<Option<String>, StoreError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT value, updated_at FROM kvpair WHERE key = $1")
                .bind(session_key(token))
                .fetch_optional(self.pool())
                .await
                .map_err(db)?;
        let Some((username, updated_at)) = row else {
            return Ok(None);
        };
        if updated_at < session_cutoff() {
            sqlx::query("DELETE FROM kvpair WHERE key = $1")
                .bind(session_key(token))
                .execute(self.pool())
                .await
                .map_err(db)?;
            return Ok(None);
        }
        Ok(Some(username))
    }

    async fn delete_id_session(&self, token: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM kvpair WHERE key = $1")
            .bind(session_key(token))
            .execute(self.pool())
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn create_password_reset_token(&self, username: &str) -> Result<String, StoreError> {
        let token = random_token();
        sqlx::query("INSERT INTO kvpair (key, value, updated_at) VALUES ($1, $2, $3)")
            .bind(password_reset_key(&token))
            .bind(username)
            .bind(now())
            .execute(self.pool())
            .await
            .map_err(db)?;
        Ok(token)
    }

    async fn find_password_reset_token(
        &self,
        token: &str,
        max_age_seconds: i64,
    ) -> Result<Option<String>, StoreError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT value, updated_at FROM kvpair WHERE key = $1")
                .bind(password_reset_key(token))
                .fetch_optional(self.pool())
                .await
                .map_err(db)?;
        let Some((username, updated_at)) = row else {
            return Ok(None);
        };
        if updated_at <= password_reset_cutoff(max_age_seconds) {
            sqlx::query("DELETE FROM kvpair WHERE key = $1")
                .bind(password_reset_key(token))
                .execute(self.pool())
                .await
                .map_err(db)?;
            return Ok(None);
        }
        Ok(Some(username))
    }

    async fn consume_password_reset_token(
        &self,
        token: &str,
        max_age_seconds: i64,
    ) -> Result<Option<String>, StoreError> {
        let row: Option<(String, String)> =
            sqlx::query_as("DELETE FROM kvpair WHERE key = $1 RETURNING value, updated_at")
                .bind(password_reset_key(token))
                .fetch_optional(self.pool())
                .await
                .map_err(db)?;
        let Some((username, updated_at)) = row else {
            return Ok(None);
        };
        if updated_at <= password_reset_cutoff(max_age_seconds) {
            return Ok(None);
        }
        Ok(Some(username))
    }
}
