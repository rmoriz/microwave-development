use crate::cookbooks::cleanup_unreferenced_checksums;
use crate::identity::{conflict, now};
use crate::objects::create_object_authz;
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCookbookArtifact {
    pub name: String,
    pub authz_id: String,
    pub identifiers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCookbookArtifactVersion {
    pub id: i64,
    pub name: String,
    pub identifier: String,
    pub manifest: String,
    pub checksums: Vec<String>,
    pub authz_id: String,
    pub created_at: String,
    pub created_by: String,
}

impl SqliteStore {
    pub fn list_cookbook_artifacts(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredCookbookArtifact>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, name, authz_id FROM cookbook_artifacts
             WHERE org_id = ?1 ORDER BY name",
        )?;
        let rows = statement.query_map([org_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut artifacts = Vec::new();
        for row in rows {
            let (id, name, authz_id) = row?;
            let identifiers = artifact_identifiers(&connection, id)?;
            artifacts.push(StoredCookbookArtifact {
                name,
                authz_id,
                identifiers,
            });
        }
        Ok(artifacts)
    }

    pub fn find_cookbook_artifact(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredCookbookArtifact>, StoreError> {
        let connection = self.connection();
        let row = connection
            .query_row(
                "SELECT id, authz_id FROM cookbook_artifacts WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((id, authz_id)) = row else {
            return Ok(None);
        };
        let identifiers = artifact_identifiers(&connection, id)?;
        Ok(Some(StoredCookbookArtifact {
            name: name.into(),
            authz_id,
            identifiers,
        }))
    }

    pub fn find_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
    ) -> Result<Option<StoredCookbookArtifactVersion>, StoreError> {
        let connection = self.connection();
        query_artifact_version(&connection, org_id, name, identifier)
    }

    pub fn create_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
        manifest: &str,
        checksums: &[String],
        creator_authz_id: &str,
    ) -> Result<StoredCookbookArtifactVersion, StoreError> {
        let timestamp = now();
        let mut connection = self.connection();
        let tx = connection.transaction()?;

        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, authz_id FROM cookbook_artifacts
                 WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (artifact_id, authz_id) = match existing {
            Some((id, authz_id)) => (id, authz_id),
            None => {
                let authz_id =
                    create_object_authz(&tx, org_id, "cookbook_artifacts", creator_authz_id)?;
                tx.execute(
                    "INSERT INTO cookbook_artifacts (org_id, name, authz_id)
                     VALUES (?1, ?2, ?3)",
                    params![org_id, name, authz_id],
                )
                .map_err(|error| {
                    conflict(error, format!("cookbook artifact {name} already exists"))
                })?;
                (tx.last_insert_rowid(), authz_id)
            }
        };

        tx.execute(
            "INSERT INTO cookbook_artifact_versions
             (cookbook_artifact_id, org_id, identifier, metadata, serialized_object,
              created_at, created_by)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
            params![
                artifact_id,
                org_id,
                identifier,
                manifest,
                timestamp,
                creator_authz_id
            ],
        )
        .map_err(|error| {
            conflict(
                error,
                format!("cookbook artifact version {name} {identifier} already exists"),
            )
        })?;
        let version_id = tx.last_insert_rowid();

        for checksum in checksums {
            tx.execute(
                "INSERT INTO cookbook_artifact_version_checksums
                 (cookbook_artifact_version_id, org_id, checksum)
                 VALUES (?1, ?2, ?3)",
                params![version_id, org_id, checksum],
            )?;
        }

        tx.commit()?;
        Ok(StoredCookbookArtifactVersion {
            id: version_id,
            name: name.into(),
            identifier: identifier.into(),
            manifest: manifest.into(),
            checksums: checksums.to_vec(),
            authz_id,
            created_at: timestamp,
            created_by: creator_authz_id.into(),
        })
    }

    /// Delete a cookbook artifact version and any artifact name that has no
    /// remaining versions. Returns the deleted document and the checksums that
    /// should be removed from object storage.
    pub fn delete_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
    ) -> Result<(StoredCookbookArtifactVersion, Vec<String>), StoreError> {
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let row: Option<(i64, i64, String, String, String, String)> = tx
            .query_row(
                "SELECT cav.id, ca.id, ca.authz_id, cav.serialized_object,
                        cav.created_at, cav.created_by
                 FROM cookbook_artifact_versions cav
                 JOIN cookbook_artifacts ca ON ca.id = cav.cookbook_artifact_id
                 WHERE cav.org_id = ?1 AND ca.name = ?2 AND cav.identifier = ?3",
                params![org_id, name, identifier],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((version_id, artifact_id, authz_id, manifest, created_at, created_by)) = row
        else {
            return Err(StoreError::NotFound(format!(
                "cookbook artifact version {name} {identifier}"
            )));
        };
        let checksums = version_checksums(&tx, version_id)?;

        tx.execute(
            "DELETE FROM cookbook_artifact_versions WHERE id = ?1",
            [version_id],
        )?;
        let remaining: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cookbook_artifact_versions WHERE cookbook_artifact_id = ?1",
            [artifact_id],
            |row| row.get(0),
        )?;
        if remaining == 0 {
            tx.execute(
                "DELETE FROM cookbook_artifacts WHERE id = ?1",
                [artifact_id],
            )?;
            tx.execute(
                "DELETE FROM authz_entities WHERE authz_id = ?1",
                [authz_id.clone()],
            )?;
        }
        let deleted_checksums = cleanup_unreferenced_checksums(&tx, org_id, checksums.clone())?;
        tx.commit()?;

        Ok((
            StoredCookbookArtifactVersion {
                id: version_id,
                name: name.into(),
                identifier: identifier.into(),
                manifest,
                checksums,
                authz_id,
                created_at,
                created_by,
            },
            deleted_checksums,
        ))
    }
}

fn artifact_identifiers(
    connection: &rusqlite::Connection,
    artifact_id: i64,
) -> Result<Vec<String>, StoreError> {
    Ok(connection
        .prepare(
            "SELECT identifier FROM cookbook_artifact_versions
             WHERE cookbook_artifact_id = ?1 ORDER BY identifier",
        )?
        .query_map([artifact_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn query_artifact_version(
    connection: &rusqlite::Connection,
    org_id: &str,
    name: &str,
    identifier: &str,
) -> Result<Option<StoredCookbookArtifactVersion>, StoreError> {
    let row = connection
        .query_row(
            "SELECT cav.id, ca.name, cav.identifier, cav.serialized_object,
                    ca.authz_id, cav.created_at, cav.created_by
             FROM cookbook_artifact_versions cav
             JOIN cookbook_artifacts ca ON ca.id = cav.cookbook_artifact_id
             WHERE cav.org_id = ?1 AND ca.name = ?2 AND cav.identifier = ?3",
            params![org_id, name, identifier],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((id, name, identifier, manifest, authz_id, created_at, created_by)) = row else {
        return Ok(None);
    };
    let checksums = version_checksums(connection, id)?;
    Ok(Some(StoredCookbookArtifactVersion {
        id,
        name,
        identifier,
        manifest,
        checksums,
        authz_id,
        created_at,
        created_by,
    }))
}

fn version_checksums(
    tx: &rusqlite::Connection,
    version_id: i64,
) -> Result<Vec<String>, StoreError> {
    Ok(tx
        .prepare(
            "SELECT checksum FROM cookbook_artifact_version_checksums
             WHERE cookbook_artifact_version_id = ?1 ORDER BY checksum",
        )?
        .query_map([version_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}
