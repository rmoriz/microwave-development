use crate::identity::now;
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension, Row};

/// An OAuth application registered with the local `/id` compatibility surface.
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

impl SqliteStore {
    #[allow(clippy::too_many_arguments)]
    pub fn create_oauth_application(
        &self,
        name: &str,
        uid: &str,
        secret: &str,
        redirect_uri: &str,
        scopes: &str,
        confidential: bool,
    ) -> Result<OAuthApplication, StoreError> {
        let timestamp = now();
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO oauth_applications
                 (name, uid, secret, redirect_uri, scopes, confidential, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    name,
                    uid,
                    secret,
                    redirect_uri,
                    scopes,
                    confidential as i64,
                    timestamp
                ],
            )
            .map_err(|error| {
                crate::identity::conflict(error, format!("OAuth application {name} already exists"))
            })?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.find_oauth_application(id)?
            .ok_or_else(|| StoreError::NotFound(format!("OAuth application {id}")))
    }

    pub fn find_oauth_application(&self, id: i64) -> Result<Option<OAuthApplication>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT id, name, uid, secret, redirect_uri, scopes, confidential,
                        created_at, updated_at
                 FROM oauth_applications WHERE id = ?1",
                [id],
                oauth_application_from_row,
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
                "SELECT id, name, uid, secret, redirect_uri, scopes, confidential,
                        created_at, updated_at
                 FROM oauth_applications WHERE uid = ?1",
                [uid],
                oauth_application_from_row,
            )
            .optional()?)
    }

    pub fn list_oauth_applications(&self) -> Result<Vec<OAuthApplication>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, name, uid, secret, redirect_uri, scopes, confidential,
                    created_at, updated_at
             FROM oauth_applications ORDER BY id",
        )?;
        let rows = statement.query_map([], oauth_application_from_row)?;
        let mut applications = Vec::new();
        for row in rows {
            applications.push(row?);
        }
        Ok(applications)
    }
}

fn oauth_application_from_row(row: &Row<'_>) -> rusqlite::Result<OAuthApplication> {
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
