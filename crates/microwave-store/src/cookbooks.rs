use crate::compression::{decode_serialized, encode_serialized};
use crate::identity::{conflict, new_id, now};
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension, Transaction};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCookbookVersion {
    pub id: String,
    pub name: String,
    pub version: String,
    pub manifest: String,
    pub checksums: Vec<String>,
    pub frozen: bool,
    pub authz_id: String,
    pub created_at: String,
    pub updated_at: String,
}

impl SqliteStore {
    #[allow(clippy::too_many_arguments)]
    pub fn create_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
        manifest: &str,
        checksums: &[String],
        frozen: bool,
        creator_authz_id: &str,
    ) -> Result<StoredCookbookVersion, StoreError> {
        let (major, minor, patch) = parse_version(version)?;
        let timestamp = now();
        let version_id = new_id();
        let serialized_object = encode_serialized(manifest, self.compression())?;
        let mut connection = self.connection();
        let tx = connection.transaction()?;

        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, authz_id FROM cookbooks WHERE org_id = ?1 AND name = ?2",
                params![org_id, name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (cookbook_id, authz_id) = match existing {
            Some((id, authz_id)) => (id, authz_id),
            None => {
                let authz_id = create_cookbook_authz(&tx, org_id, creator_authz_id)?;
                tx.execute(
                    "INSERT INTO cookbooks (org_id, name, authz_id) VALUES (?1, ?2, ?3)",
                    params![org_id, name, authz_id],
                )
                .map_err(|error| conflict(error, format!("cookbook {name} already exists")))?;
                (tx.last_insert_rowid(), authz_id)
            }
        };

        tx.execute(
            "INSERT INTO cookbook_versions
             (id, cookbook_id, org_id, name, major, minor, patch, frozen,
              meta_attributes, meta_deps, meta_long_desc, metadata, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, '{}', NULL, NULL, ?9, ?10, ?11, ?11)",
            params![
                version_id,
                cookbook_id,
                org_id,
                name,
                major,
                minor,
                patch,
                i64::from(frozen),
                serialized_object,
                creator_authz_id,
                timestamp
            ],
        )
        .map_err(|error| {
            conflict(
                error,
                format!("cookbook version {name} {version} already exists"),
            )
        })?;

        insert_version_checksums(&tx, org_id, &version_id, checksums)?;

        tx.commit()?;
        Ok(StoredCookbookVersion {
            id: version_id,
            name: name.into(),
            version: format_version(major, minor, patch),
            manifest: manifest.into(),
            checksums: checksums.to_vec(),
            frozen,
            authz_id,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
    }

    /// Replace an existing cookbook version's manifest and checksums.
    ///
    /// Returns the updated record plus every checksum that is no longer
    /// referenced by any cookbook or cookbook artifact and should therefore be
    /// removed from object storage.
    #[allow(clippy::too_many_arguments)]
    pub fn update_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
        manifest: &str,
        checksums: &[String],
        frozen: bool,
        updater_authz_id: &str,
    ) -> Result<(StoredCookbookVersion, Vec<String>), StoreError> {
        let (major, minor, patch) = parse_version(version)?;
        let timestamp = now();
        let serialized_object = encode_serialized(manifest, self.compression())?;
        let mut connection = self.connection();
        let tx = connection.transaction()?;

        let row: Option<(String, String, String)> = tx
            .query_row(
                "SELECT cv.id, cv.created_at, c.authz_id
                 FROM cookbook_versions cv
                 JOIN cookbooks c ON c.id = cv.cookbook_id
                 WHERE cv.org_id = ?1 AND cv.name = ?2
                   AND cv.major = ?3 AND cv.minor = ?4 AND cv.patch = ?5",
                params![org_id, name, major, minor, patch],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((version_id, created_at, authz_id)) = row else {
            return Err(StoreError::NotFound(format!(
                "cookbook version {name} {version}"
            )));
        };

        let old_checksums = version_checksums(&tx, &version_id)?;
        tx.execute(
            "UPDATE cookbook_versions
             SET serialized_object = ?2, frozen = ?3, updated_at = ?4,
                 last_updated_by = ?5
             WHERE id = ?1",
            params![
                version_id,
                serialized_object,
                i64::from(frozen),
                timestamp,
                updater_authz_id
            ],
        )?;
        tx.execute(
            "DELETE FROM cookbook_version_checksums WHERE cookbook_version_id = ?1",
            [&version_id],
        )?;
        insert_version_checksums(&tx, org_id, &version_id, checksums)?;

        let new_checksums: BTreeSet<&String> = checksums.iter().collect();
        let removed_candidates = old_checksums
            .into_iter()
            .filter(|checksum| !new_checksums.contains(checksum));
        let deleted_checksums = cleanup_unreferenced_checksums(&tx, org_id, removed_candidates)?;

        tx.commit()?;
        Ok((
            StoredCookbookVersion {
                id: version_id,
                name: name.into(),
                version: format_version(major, minor, patch),
                manifest: manifest.into(),
                checksums: checksums.to_vec(),
                frozen,
                authz_id,
                created_at,
                updated_at: timestamp,
            },
            deleted_checksums,
        ))
    }

    pub fn find_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<StoredCookbookVersion>, StoreError> {
        let (major, minor, patch) = parse_version(version)?;
        let connection = self.connection();
        let row = connection
            .query_row(
                "SELECT cv.id, cv.name, cv.serialized_object, cv.created_at, cv.updated_at,
                        c.authz_id, cv.major, cv.minor, cv.patch, cv.frozen
                 FROM cookbook_versions cv
                 JOIN cookbooks c ON c.id = cv.cookbook_id
                 WHERE cv.org_id = ?1 AND cv.name = ?2
                   AND cv.major = ?3 AND cv.minor = ?4 AND cv.patch = ?5",
                params![org_id, name, major, minor, patch],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                    ))
                },
            )
            .optional()?;

        let Some((
            id,
            name,
            manifest,
            created_at,
            updated_at,
            authz_id,
            major,
            minor,
            patch,
            frozen,
        )) = row
        else {
            return Ok(None);
        };
        let manifest = decode_serialized(&manifest)?;
        let checksums = version_checksums(&connection, &id)?;
        Ok(Some(StoredCookbookVersion {
            id,
            name,
            version: format_version(major, minor, patch),
            manifest,
            checksums,
            frozen: frozen != 0,
            authz_id,
            created_at,
            updated_at,
        }))
    }

    pub fn list_cookbook_versions(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredCookbookVersion>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT cv.id, cv.name, cv.serialized_object, cv.created_at, cv.updated_at,
                    c.authz_id, cv.major, cv.minor, cv.patch, cv.frozen
             FROM cookbook_versions cv
             JOIN cookbooks c ON c.id = cv.cookbook_id
             WHERE cv.org_id = ?1
             ORDER BY cv.name, cv.major, cv.minor, cv.patch",
        )?;
        let rows = statement.query_map([org_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })?;

        let mut versions = Vec::new();
        for row in rows {
            let (id, name, manifest, created_at, updated_at, authz_id, major, minor, patch, frozen) =
                row?;
            let manifest = decode_serialized(&manifest)?;
            let checksums = version_checksums(&connection, &id)?;
            versions.push(StoredCookbookVersion {
                id,
                name,
                version: format_version(major, minor, patch),
                manifest,
                checksums,
                frozen: frozen != 0,
                authz_id,
                created_at,
                updated_at,
            });
        }
        Ok(versions)
    }

    /// Delete a cookbook version.
    ///
    /// Returns the deleted document along with the checksums that are no longer
    /// referenced and should be removed from object storage.
    pub fn delete_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<(StoredCookbookVersion, Vec<String>), StoreError> {
        let (major, minor, patch) = parse_version(version)?;
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let row: Option<(String, i64, String, String, String, i64)> = tx
            .query_row(
                "SELECT cv.id, cv.cookbook_id, c.authz_id, cv.serialized_object,
                        cv.created_at, cv.frozen
                 FROM cookbook_versions cv
                 JOIN cookbooks c ON c.id = cv.cookbook_id
                 WHERE cv.org_id = ?1 AND cv.name = ?2
                   AND cv.major = ?3 AND cv.minor = ?4 AND cv.patch = ?5",
                params![org_id, name, major, minor, patch],
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
        let Some((version_id, cookbook_id, authz_id, manifest, created_at, frozen)) = row else {
            return Err(StoreError::NotFound(format!(
                "cookbook version {name} {version}"
            )));
        };
        let manifest = decode_serialized(&manifest)?;
        let checksums = version_checksums(&tx, &version_id)?;

        tx.execute("DELETE FROM cookbook_versions WHERE id = ?1", [&version_id])?;
        let remaining: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cookbook_versions WHERE cookbook_id = ?1",
            [cookbook_id],
            |row| row.get(0),
        )?;
        if remaining == 0 {
            tx.execute("DELETE FROM cookbooks WHERE id = ?1", [cookbook_id])?;
            tx.execute(
                "DELETE FROM authz_entities WHERE authz_id = ?1",
                [authz_id.clone()],
            )?;
        }
        let deleted_checksums = cleanup_unreferenced_checksums(&tx, org_id, checksums.clone())?;
        tx.commit()?;

        Ok((
            StoredCookbookVersion {
                id: version_id,
                name: name.into(),
                version: format_version(major, minor, patch),
                manifest,
                checksums,
                frozen: frozen != 0,
                authz_id,
                created_at: created_at.clone(),
                updated_at: created_at,
            },
            deleted_checksums,
        ))
    }
}

fn insert_version_checksums(
    tx: &Transaction<'_>,
    org_id: &str,
    version_id: &str,
    checksums: &[String],
) -> Result<(), StoreError> {
    for checksum in checksums {
        tx.execute(
            "INSERT INTO cookbook_version_checksums
             (cookbook_version_id, org_id, checksum)
             VALUES (?1, ?2, ?3)",
            params![version_id, org_id, checksum],
        )?;
    }
    Ok(())
}

fn version_checksums(
    connection: &rusqlite::Connection,
    version_id: &str,
) -> Result<Vec<String>, StoreError> {
    Ok(connection
        .prepare(
            "SELECT checksum FROM cookbook_version_checksums
             WHERE cookbook_version_id = ?1 ORDER BY checksum",
        )?
        .query_map([version_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

/// Delete checksum rows that are no longer referenced by either a cookbook
/// version or a cookbook artifact version, returning those that were removed.
pub(crate) fn cleanup_unreferenced_checksums(
    tx: &Transaction<'_>,
    org_id: &str,
    candidates: impl IntoIterator<Item = String>,
) -> Result<Vec<String>, StoreError> {
    let mut deleted = Vec::new();
    let mut seen = BTreeSet::new();
    for checksum in candidates {
        if !seen.insert(checksum.clone()) {
            continue;
        }
        let referenced: i64 = tx.query_row(
            "SELECT (
                 EXISTS(SELECT 1 FROM cookbook_version_checksums
                        WHERE org_id = ?1 AND checksum = ?2)
                 OR EXISTS(SELECT 1 FROM cookbook_artifact_version_checksums
                           WHERE org_id = ?1 AND checksum = ?2)
             )",
            params![org_id, checksum],
            |row| row.get(0),
        )?;
        if referenced == 0 {
            let changed = tx.execute(
                "DELETE FROM checksums WHERE org_id = ?1 AND checksum = ?2",
                params![org_id, checksum],
            )?;
            if changed > 0 {
                deleted.push(checksum.clone());
            }
        }
    }
    Ok(deleted)
}

fn create_cookbook_authz(
    tx: &Transaction<'_>,
    org_id: &str,
    creator_authz_id: &str,
) -> Result<String, StoreError> {
    let container_authz_id: String = tx
        .query_row(
            "SELECT authz_id FROM containers WHERE org_id = ?1 AND name = 'cookbooks'",
            [org_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound("container cookbooks".into()))?;
    let authz_id = new_id();
    tx.execute(
        "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
         VALUES (?1, 'object', ?2, ?3)",
        params![authz_id, org_id, now()],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
         SELECT ?1, permission, actor_authz_id FROM authz_acl_actor WHERE target_authz_id = ?2",
        params![authz_id, container_authz_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO authz_acl_group (target_authz_id, permission, group_authz_id)
         SELECT ?1, permission, group_authz_id FROM authz_acl_group WHERE target_authz_id = ?2",
        params![authz_id, container_authz_id],
    )?;
    for permission in crate::authz::Permission::ALL {
        tx.execute(
            "INSERT OR IGNORE INTO authz_acl_actor
             (target_authz_id, permission, actor_authz_id)
             VALUES (?1, ?2, ?3)",
            params![authz_id, permission.as_str(), creator_authz_id],
        )?;
    }
    Ok(authz_id)
}

pub fn parse_version(version: &str) -> Result<(i64, i64, i64), StoreError> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return Err(StoreError::Invalid(format!("invalid version {version}")));
    }
    let parse = |index: usize| -> Result<i64, StoreError> {
        parts
            .get(index)
            .map(|part| {
                if part.is_empty() {
                    return Err(StoreError::Invalid(format!("invalid version {version}")));
                }
                let value: i64 = part
                    .parse()
                    .map_err(|_| StoreError::Invalid(format!("invalid version {version}")))?;
                if value < 0 {
                    return Err(StoreError::Invalid(format!("invalid version {version}")));
                }
                Ok(value)
            })
            .unwrap_or(Ok(0))
    };
    Ok((parse(0)?, parse(1)?, parse(2)?))
}

fn format_version(major: i64, minor: i64, patch: i64) -> String {
    format!("{major}.{minor}.{patch}")
}
