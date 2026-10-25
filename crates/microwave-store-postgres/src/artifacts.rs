use async_trait::async_trait;
use microwave_store::{
    decode_serialized, encode_serialized, ArtifactRepository, Permission, StoreError,
    StoredCookbookArtifact, StoredCookbookArtifactVersion,
};
use sqlx::postgres::Postgres;
use sqlx::{Row, Transaction};

use crate::cookbooks::cleanup_unreferenced_checksums;
use crate::store::{db, new_id, now, unique, PostgresStore};

#[async_trait]
impl ArtifactRepository for PostgresStore {
    async fn list_cookbook_artifacts(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredCookbookArtifact>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, authz_id FROM cookbook_artifacts
             WHERE org_id = $1 ORDER BY name",
        )
        .bind(org_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;

        let mut artifacts = Vec::with_capacity(rows.len());
        for row in rows {
            let id: i64 = row.try_get(0).map_err(db)?;
            let name: String = row.try_get(1).map_err(db)?;
            let authz_id: String = row.try_get(2).map_err(db)?;
            let identifiers = self.artifact_identifiers(id).await?;
            artifacts.push(StoredCookbookArtifact {
                name,
                authz_id,
                identifiers,
            });
        }
        Ok(artifacts)
    }

    async fn find_cookbook_artifact(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredCookbookArtifact>, StoreError> {
        let row = sqlx::query(
            "SELECT id, authz_id FROM cookbook_artifacts WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id: i64 = row.try_get(0).map_err(db)?;
        let authz_id: String = row.try_get(1).map_err(db)?;
        Ok(Some(StoredCookbookArtifact {
            name: name.into(),
            authz_id,
            identifiers: self.artifact_identifiers(id).await?,
        }))
    }

    async fn find_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
    ) -> Result<Option<StoredCookbookArtifactVersion>, StoreError> {
        let row = sqlx::query(
            "SELECT cav.id, ca.name, cav.identifier, cav.serialized_object,
                    ca.authz_id, cav.created_at, cav.created_by
             FROM cookbook_artifact_versions cav
             JOIN cookbook_artifacts ca ON ca.id = cav.cookbook_artifact_id
             WHERE cav.org_id = $1 AND ca.name = $2 AND cav.identifier = $3",
        )
        .bind(org_id)
        .bind(name)
        .bind(identifier)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id: i64 = row.try_get(0).map_err(db)?;
        let checksums = self.artifact_version_checksums(id).await?;
        let manifest = decode_serialized(&row.try_get::<String, _>(3).map_err(db)?)?;
        Ok(Some(StoredCookbookArtifactVersion {
            id,
            name: row.try_get(1).map_err(db)?,
            identifier: row.try_get(2).map_err(db)?,
            manifest,
            checksums,
            authz_id: row.try_get(4).map_err(db)?,
            created_at: row.try_get(5).map_err(db)?,
            created_by: row.try_get(6).map_err(db)?,
        }))
    }

    async fn create_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
        manifest: &str,
        checksums: &[String],
        creator_authz_id: &str,
    ) -> Result<StoredCookbookArtifactVersion, StoreError> {
        let timestamp = now();
        let serialized_object = encode_serialized(manifest, self.compression())?;
        let mut transaction = self.pool().begin().await.map_err(db)?;

        let existing: Option<(i64, String)> = sqlx::query_as(
            "SELECT id, authz_id FROM cookbook_artifacts WHERE org_id = $1 AND name = $2",
        )
        .bind(org_id)
        .bind(name)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?;

        let (artifact_id, authz_id) = match existing {
            Some((id, authz_id)) => (id, authz_id),
            None => {
                let authz_id =
                    create_artifact_authz(&mut transaction, org_id, creator_authz_id).await?;
                let artifact_id: i64 = sqlx::query_scalar(
                    "INSERT INTO cookbook_artifacts (org_id, name, authz_id)
                     VALUES ($1, $2, $3) RETURNING id",
                )
                .bind(org_id)
                .bind(name)
                .bind(&authz_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|error| {
                    unique(error, format!("cookbook artifact {name} already exists"))
                })?;
                (artifact_id, authz_id)
            }
        };

        let version_id: i64 = sqlx::query_scalar(
            "INSERT INTO cookbook_artifact_versions
             (cookbook_artifact_id, org_id, identifier, metadata, serialized_object,
              created_at, created_by)
             VALUES ($1, $2, $3, NULL, $4, $5, $6)
             RETURNING id",
        )
        .bind(artifact_id)
        .bind(org_id)
        .bind(identifier)
        .bind(&serialized_object)
        .bind(&timestamp)
        .bind(creator_authz_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| {
            unique(
                error,
                format!("cookbook artifact version {name} {identifier} already exists"),
            )
        })?;

        for checksum in checksums {
            sqlx::query(
                "INSERT INTO cookbook_artifact_version_checksums
                 (cookbook_artifact_version_id, org_id, checksum)
                 VALUES ($1, $2, $3)",
            )
            .bind(version_id)
            .bind(org_id)
            .bind(checksum)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }

        transaction.commit().await.map_err(db)?;
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

    async fn delete_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
    ) -> Result<(StoredCookbookArtifactVersion, Vec<String>), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT cav.id, ca.id, ca.authz_id, cav.serialized_object,
                    cav.created_at, cav.created_by
             FROM cookbook_artifact_versions cav
             JOIN cookbook_artifacts ca ON ca.id = cav.cookbook_artifact_id
             WHERE cav.org_id = $1 AND ca.name = $2 AND cav.identifier = $3",
        )
        .bind(org_id)
        .bind(name)
        .bind(identifier)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or_else(|| {
            StoreError::NotFound(format!("cookbook artifact version {name} {identifier}"))
        })?;
        let version_id: i64 = row.try_get(0).map_err(db)?;
        let artifact_id: i64 = row.try_get(1).map_err(db)?;
        let authz_id: String = row.try_get(2).map_err(db)?;
        let manifest: String = row.try_get(3).map_err(db)?;
        let manifest = decode_serialized(&manifest)?;
        let created_at: String = row.try_get(4).map_err(db)?;
        let created_by: String = row.try_get(5).map_err(db)?;

        let checksums: Vec<String> = sqlx::query_scalar(
            "SELECT checksum FROM cookbook_artifact_version_checksums
             WHERE cookbook_artifact_version_id = $1 ORDER BY checksum",
        )
        .bind(version_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(db)?;

        sqlx::query("DELETE FROM cookbook_artifact_versions WHERE id = $1")
            .bind(version_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM cookbook_artifact_versions WHERE cookbook_artifact_id = $1",
        )
        .bind(artifact_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        if remaining == 0 {
            sqlx::query("DELETE FROM cookbook_artifacts WHERE id = $1")
                .bind(artifact_id)
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
            sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
                .bind(&authz_id)
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
        }
        let deleted_checksums =
            cleanup_unreferenced_checksums(&mut transaction, org_id, checksums.clone()).await?;
        transaction.commit().await.map_err(db)?;

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

impl PostgresStore {
    async fn artifact_identifiers(&self, artifact_id: i64) -> Result<Vec<String>, StoreError> {
        sqlx::query_scalar(
            "SELECT identifier FROM cookbook_artifact_versions
             WHERE cookbook_artifact_id = $1 ORDER BY identifier",
        )
        .bind(artifact_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)
    }

    async fn artifact_version_checksums(&self, version_id: i64) -> Result<Vec<String>, StoreError> {
        sqlx::query_scalar(
            "SELECT checksum FROM cookbook_artifact_version_checksums
             WHERE cookbook_artifact_version_id = $1 ORDER BY checksum",
        )
        .bind(version_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)
    }
}

async fn create_artifact_authz(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    creator_authz_id: &str,
) -> Result<String, StoreError> {
    let container_authz_id: String = sqlx::query_scalar(
        "SELECT authz_id FROM containers WHERE org_id = $1 AND name = 'cookbook_artifacts'",
    )
    .bind(org_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?
    .ok_or_else(|| StoreError::NotFound("container cookbook_artifacts".into()))?;
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
