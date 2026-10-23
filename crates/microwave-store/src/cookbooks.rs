use crate::identity::{conflict, new_id, now};
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension, Transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCookbookVersion {
    pub id: String,
    pub name: String,
    pub version: String,
    pub manifest: String,
    pub checksums: Vec<String>,
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
        creator_authz_id: &str,
    ) -> Result<StoredCookbookVersion, StoreError> {
        let (major, minor, patch) = parse_version(version)?;
        let timestamp = now();
        let version_id = new_id();
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
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, NULL, '{}', NULL, ?8, ?8, ?9, ?10, ?10)",
            params![
                version_id,
                cookbook_id,
                org_id,
                name,
                major,
                minor,
                patch,
                manifest,
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

        for checksum in checksums {
            tx.execute(
                "INSERT INTO cookbook_version_checksums
                 (cookbook_version_id, org_id, checksum)
                 VALUES (?1, ?2, ?3)",
                params![version_id, org_id, checksum],
            )?;
        }

        tx.commit()?;
        Ok(StoredCookbookVersion {
            id: version_id,
            name: name.into(),
            version: version.into(),
            manifest: manifest.into(),
            checksums: checksums.to_vec(),
            authz_id,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })
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
                        c.authz_id, cv.major, cv.minor, cv.patch
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
                    ))
                },
            )
            .optional()?;

        let Some((id, name, manifest, created_at, updated_at, authz_id, major, minor, patch)) = row
        else {
            return Ok(None);
        };
        let checksums = connection
            .prepare(
                "SELECT checksum FROM cookbook_version_checksums
                 WHERE cookbook_version_id = ?1 ORDER BY checksum",
            )?
            .query_map([&id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(StoredCookbookVersion {
            id,
            name,
            version: format_version(major, minor, patch),
            manifest,
            checksums,
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
                    c.authz_id, cv.major, cv.minor, cv.patch
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
            ))
        })?;

        let mut versions = Vec::new();
        for row in rows {
            let (id, name, manifest, created_at, updated_at, authz_id, major, minor, patch) = row?;
            let checksums = connection
                .prepare(
                    "SELECT checksum FROM cookbook_version_checksums
                     WHERE cookbook_version_id = ?1 ORDER BY checksum",
                )?
                .query_map([&id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            versions.push(StoredCookbookVersion {
                id,
                name,
                version: format_version(major, minor, patch),
                manifest,
                checksums,
                authz_id,
                created_at,
                updated_at,
            });
        }
        Ok(versions)
    }

    pub fn delete_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<(), StoreError> {
        let (major, minor, patch) = parse_version(version)?;
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        let row: Option<(String, i64, String)> = tx
            .query_row(
                "SELECT cv.id, cv.cookbook_id, c.authz_id
                 FROM cookbook_versions cv
                 JOIN cookbooks c ON c.id = cv.cookbook_id
                 WHERE cv.org_id = ?1 AND cv.name = ?2
                   AND cv.major = ?3 AND cv.minor = ?4 AND cv.patch = ?5",
                params![org_id, name, major, minor, patch],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((version_id, cookbook_id, authz_id)) = row else {
            return Err(StoreError::NotFound(format!(
                "cookbook version {name} {version}"
            )));
        };

        tx.execute("DELETE FROM cookbook_versions WHERE id = ?1", [&version_id])?;
        let remaining: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cookbook_versions WHERE cookbook_id = ?1",
            [cookbook_id],
            |row| row.get(0),
        )?;
        if remaining == 0 {
            tx.execute("DELETE FROM cookbooks WHERE id = ?1", [cookbook_id])?;
            tx.execute("DELETE FROM authz_entities WHERE authz_id = ?1", [authz_id])?;
        }
        tx.commit()?;
        Ok(())
    }
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
                part.parse()
                    .map_err(|_| StoreError::Invalid(format!("invalid version {version}")))
            })
            .unwrap_or(Ok(0))
    };
    Ok((parse(0)?, parse(1)?, parse(2)?))
}

fn format_version(major: i64, minor: i64, patch: i64) -> String {
    format!("{major}.{minor}.{patch}")
}
