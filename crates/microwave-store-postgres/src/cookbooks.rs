use async_trait::async_trait;
use microwave_store::{
    parse_version, CookbookRepository, Permission, StoreError, StoredCookbookVersion,
};
use sqlx::postgres::Postgres;
use sqlx::{Row, Transaction};

use crate::store::{db, new_id, now, unique, PostgresStore};

#[async_trait]
impl CookbookRepository for PostgresStore {
    async fn create_cookbook_version(
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
        let mut transaction = self.pool().begin().await.map_err(db)?;

        let existing: Option<(i64, String)> =
            sqlx::query_as("SELECT id, authz_id FROM cookbooks WHERE org_id = $1 AND name = $2")
                .bind(org_id)
                .bind(name)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(db)?;

        let (cookbook_id, authz_id) = match existing {
            Some((id, authz_id)) => (id, authz_id),
            None => {
                let authz_id =
                    create_cookbook_authz(&mut transaction, org_id, creator_authz_id).await?;
                let cookbook_id: i64 = sqlx::query_scalar(
                    "INSERT INTO cookbooks (org_id, name, authz_id)
                     VALUES ($1, $2, $3) RETURNING id",
                )
                .bind(org_id)
                .bind(name)
                .bind(&authz_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|error| unique(error, format!("cookbook {name} already exists")))?;
                (cookbook_id, authz_id)
            }
        };

        sqlx::query(
            "INSERT INTO cookbook_versions
             (id, cookbook_id, org_id, name, major, minor, patch, frozen,
              meta_attributes, meta_deps, meta_long_desc, metadata, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, FALSE, NULL, '{}', NULL, $8, $8, $9, $10, $10)",
        )
        .bind(&version_id)
        .bind(cookbook_id)
        .bind(org_id)
        .bind(name)
        .bind(major)
        .bind(minor)
        .bind(patch)
        .bind(manifest)
        .bind(creator_authz_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            unique(
                error,
                format!("cookbook version {name} {version} already exists"),
            )
        })?;

        for checksum in checksums {
            sqlx::query(
                "INSERT INTO cookbook_version_checksums
                 (cookbook_version_id, org_id, checksum)
                 VALUES ($1, $2, $3)",
            )
            .bind(&version_id)
            .bind(org_id)
            .bind(checksum)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }

        transaction.commit().await.map_err(db)?;
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

    async fn find_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<StoredCookbookVersion>, StoreError> {
        let (major, minor, patch) = parse_version(version)?;
        let row = sqlx::query(
            "SELECT cv.id, cv.name, cv.serialized_object, cv.created_at, cv.updated_at,
                    c.authz_id, cv.major, cv.minor, cv.patch
             FROM cookbook_versions cv
             JOIN cookbooks c ON c.id = cv.cookbook_id
             WHERE cv.org_id = $1 AND cv.name = $2
               AND cv.major = $3 AND cv.minor = $4 AND cv.patch = $5",
        )
        .bind(org_id)
        .bind(name)
        .bind(major)
        .bind(minor)
        .bind(patch)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id: String = row.try_get(0).map_err(db)?;
        let checksums = self.version_checksums(&id).await?;
        Ok(Some(StoredCookbookVersion {
            id,
            name: row.try_get(1).map_err(db)?,
            version: format_version(
                row.try_get(6).map_err(db)?,
                row.try_get(7).map_err(db)?,
                row.try_get(8).map_err(db)?,
            ),
            manifest: row.try_get(2).map_err(db)?,
            checksums,
            authz_id: row.try_get(5).map_err(db)?,
            created_at: row.try_get(3).map_err(db)?,
            updated_at: row.try_get(4).map_err(db)?,
        }))
    }

    async fn list_cookbook_versions(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredCookbookVersion>, StoreError> {
        let rows = sqlx::query(
            "SELECT cv.id, cv.name, cv.serialized_object, cv.created_at, cv.updated_at,
                    c.authz_id, cv.major, cv.minor, cv.patch
             FROM cookbook_versions cv
             JOIN cookbooks c ON c.id = cv.cookbook_id
             WHERE cv.org_id = $1
             ORDER BY cv.name, cv.major, cv.minor, cv.patch",
        )
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;

        let mut versions = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get(0).map_err(db)?;
            let checksums = self.version_checksums(&id).await?;
            versions.push(StoredCookbookVersion {
                id,
                name: row.try_get(1).map_err(db)?,
                version: format_version(
                    row.try_get(6).map_err(db)?,
                    row.try_get(7).map_err(db)?,
                    row.try_get(8).map_err(db)?,
                ),
                manifest: row.try_get(2).map_err(db)?,
                checksums,
                authz_id: row.try_get(5).map_err(db)?,
                created_at: row.try_get(3).map_err(db)?,
                updated_at: row.try_get(4).map_err(db)?,
            });
        }
        Ok(versions)
    }

    async fn delete_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<(), StoreError> {
        let (major, minor, patch) = parse_version(version)?;
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT cv.id, cv.cookbook_id, c.authz_id
             FROM cookbook_versions cv
             JOIN cookbooks c ON c.id = cv.cookbook_id
             WHERE cv.org_id = $1 AND cv.name = $2
               AND cv.major = $3 AND cv.minor = $4 AND cv.patch = $5",
        )
        .bind(org_id)
        .bind(name)
        .bind(major)
        .bind(minor)
        .bind(patch)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or_else(|| StoreError::NotFound(format!("cookbook version {name} {version}")))?;
        let version_id: String = row.try_get(0).map_err(db)?;
        let cookbook_id: i64 = row.try_get(1).map_err(db)?;
        let authz_id: String = row.try_get(2).map_err(db)?;

        sqlx::query("DELETE FROM cookbook_versions WHERE id = $1")
            .bind(&version_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM cookbook_versions WHERE cookbook_id = $1")
                .bind(cookbook_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(db)?;
        if remaining == 0 {
            sqlx::query("DELETE FROM cookbooks WHERE id = $1")
                .bind(cookbook_id)
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
            sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
                .bind(&authz_id)
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
        }
        transaction.commit().await.map_err(db)?;
        Ok(())
    }
}

impl PostgresStore {
    async fn version_checksums(&self, version_id: &str) -> Result<Vec<String>, StoreError> {
        sqlx::query_scalar(
            "SELECT checksum FROM cookbook_version_checksums
             WHERE cookbook_version_id = $1 ORDER BY checksum",
        )
        .bind(version_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)
    }
}

async fn create_cookbook_authz(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    creator_authz_id: &str,
) -> Result<String, StoreError> {
    let container_authz_id: String = sqlx::query_scalar(
        "SELECT authz_id FROM containers WHERE org_id = $1 AND name = 'cookbooks'",
    )
    .bind(org_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?
    .ok_or_else(|| StoreError::NotFound("container cookbooks".into()))?;
    let authz_id = new_id();
    sqlx::query(
        "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
         VALUES ($1, 'object', $2, $3)",
    )
    .bind(&authz_id)
    .bind(org_id)
    .bind(now())
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    sqlx::query(
        "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
         SELECT $1, permission, actor_authz_id FROM authz_acl_actor WHERE target_authz_id = $2
         ON CONFLICT DO NOTHING",
    )
    .bind(&authz_id)
    .bind(&container_authz_id)
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    sqlx::query(
        "INSERT INTO authz_acl_group (target_authz_id, permission, group_authz_id)
         SELECT $1, permission, group_authz_id FROM authz_acl_group WHERE target_authz_id = $2
         ON CONFLICT DO NOTHING",
    )
    .bind(&authz_id)
    .bind(&container_authz_id)
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    for permission in Permission::ALL {
        sqlx::query(
            "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
             VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(&authz_id)
        .bind(permission.as_str())
        .bind(creator_authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(authz_id)
}

fn format_version(major: i64, minor: i64, patch: i64) -> String {
    format!("{major}.{minor}.{patch}")
}
