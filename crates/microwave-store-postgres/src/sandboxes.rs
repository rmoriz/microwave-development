use async_trait::async_trait;
use microwave_store::{SandboxCheck, SandboxRepository, StoreError};
use std::collections::HashSet;

use crate::store::{db, new_id, now, PostgresStore};

#[async_trait]
impl SandboxRepository for PostgresStore {
    async fn create_sandbox(
        &self,
        org_id: &str,
        checksums: &[String],
    ) -> Result<(String, Vec<SandboxCheck>), StoreError> {
        let sandbox_id = new_id();
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        // The sandbox row exists independently of its uploads so an empty
        // sandbox is still visible, matching SQLite.
        sqlx::query(
            "INSERT INTO sandboxes (org_id, sandbox_id, created_at)
             VALUES ($1, $2, $3)",
        )
        .bind(org_id)
        .bind(&sandbox_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        let mut checks = Vec::with_capacity(checksums.len());
        // One query for all checksums instead of one `EXISTS` per checksum.
        let existing: HashSet<String> = if checksums.is_empty() {
            HashSet::new()
        } else {
            sqlx::query_scalar(
                "SELECT checksum FROM checksums WHERE org_id = $1 AND checksum = ANY($2)",
            )
            .bind(org_id)
            .bind(checksums)
            .fetch_all(&mut *transaction)
            .await
            .map_err(db)?
            .into_iter()
            .collect()
        };
        if !checksums.is_empty() {
            sqlx::query(
                "INSERT INTO sandboxed_checksums (org_id, sandbox_id, checksum, created_at)
                 SELECT $1, $2, item, $4 FROM UNNEST($3::text[]) AS checksums(item)
                 ON CONFLICT DO NOTHING",
            )
            .bind(org_id)
            .bind(&sandbox_id)
            .bind(checksums)
            .bind(&timestamp)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        for checksum in checksums {
            checks.push(SandboxCheck {
                checksum: checksum.clone(),
                needs_upload: !existing.contains(checksum),
            });
        }
        transaction.commit().await.map_err(db)?;
        Ok((sandbox_id, checks))
    }

    async fn sandbox_checksums(
        &self,
        org_id: &str,
        sandbox_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        let checksums = sqlx::query_scalar(
            "SELECT checksum FROM sandboxed_checksums
             WHERE org_id = $1 AND sandbox_id = $2 ORDER BY checksum",
        )
        .bind(org_id)
        .bind(sandbox_id)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        Ok(checksums)
    }

    async fn sandbox_exists(&self, org_id: &str, sandbox_id: &str) -> Result<bool, StoreError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sandboxes WHERE org_id = $1 AND sandbox_id = $2)",
        )
        .bind(org_id)
        .bind(sandbox_id)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        Ok(exists)
    }

    async fn sandbox_created_at(
        &self,
        org_id: &str,
        sandbox_id: &str,
    ) -> Result<Option<String>, StoreError> {
        let created_at: Option<String> = sqlx::query_scalar(
            "SELECT created_at FROM sandboxes WHERE org_id = $1 AND sandbox_id = $2",
        )
        .bind(org_id)
        .bind(sandbox_id)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        Ok(created_at)
    }

    async fn commit_sandbox(&self, org_id: &str, sandbox_id: &str) -> Result<(), StoreError> {
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        sqlx::query(
            "INSERT INTO checksums (org_id, checksum, created_at)
             SELECT org_id, checksum, $3 FROM sandboxed_checksums
             WHERE org_id = $1 AND sandbox_id = $2
             ON CONFLICT DO NOTHING",
        )
        .bind(org_id)
        .bind(sandbox_id)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        sqlx::query("DELETE FROM sandboxed_checksums WHERE org_id = $1 AND sandbox_id = $2")
            .bind(org_id)
            .bind(sandbox_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM sandboxes WHERE org_id = $1 AND sandbox_id = $2")
            .bind(org_id)
            .bind(sandbox_id)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn checksum_exists(&self, org_id: &str, checksum: &str) -> Result<bool, StoreError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM checksums WHERE org_id = $1 AND checksum = $2)",
        )
        .bind(org_id)
        .bind(checksum)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        Ok(exists)
    }

    async fn existing_checksums(
        &self,
        org_id: &str,
        checksums: &[String],
    ) -> Result<Vec<String>, StoreError> {
        if checksums.is_empty() {
            return Ok(Vec::new());
        }
        let found: Vec<String> = sqlx::query_scalar(
            "SELECT checksum FROM checksums
             WHERE org_id = $1 AND checksum = ANY($2) ORDER BY checksum",
        )
        .bind(org_id)
        .bind(checksums)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        Ok(found)
    }

    async fn ensure_checksums(&self, org_id: &str, checksums: &[String]) -> Result<(), StoreError> {
        if checksums.is_empty() {
            return Ok(());
        }
        let timestamp = now();
        let mut transaction = self.pool().begin().await.map_err(db)?;
        for checksum in checksums {
            sqlx::query(
                "INSERT INTO checksums (org_id, checksum, created_at)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (org_id, checksum) DO NOTHING",
            )
            .bind(org_id)
            .bind(checksum)
            .bind(&timestamp)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn find_checksum(
        &self,
        org_id: &str,
        checksum: &str,
    ) -> Result<Option<String>, StoreError> {
        let found: Option<String> = sqlx::query_scalar(
            "SELECT checksum FROM checksums WHERE org_id = $1 AND checksum = $2",
        )
        .bind(org_id)
        .bind(checksum)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        Ok(found)
    }
}
