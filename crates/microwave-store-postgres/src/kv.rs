use async_trait::async_trait;
use microwave_store::{KvRepository, StoreError, MAINTENANCE_KV_KEY};

use crate::store::{db, now, PostgresStore};

#[async_trait]
impl KvRepository for PostgresStore {
    async fn get_kv(&self, key: &str) -> Result<Option<String>, StoreError> {
        sqlx::query_scalar("SELECT value FROM kvpair WHERE key = $1")
            .bind(key)
            .fetch_optional(self.pool())
            .await
            .map_err(db)
    }

    async fn set_kv(&self, key: &str, value: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO kvpair (key, value, updated_at) VALUES ($1, $2, $3)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value,
                                            updated_at = EXCLUDED.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn remove_kv(&self, key: &str) -> Result<bool, StoreError> {
        let changed = sqlx::query("DELETE FROM kvpair WHERE key = $1")
            .bind(key)
            .execute(self.pool())
            .await
            .map_err(db)?;
        Ok(changed.rows_affected() != 0)
    }

    async fn maintenance_enabled(&self) -> Result<bool, StoreError> {
        Ok(self.get_kv(MAINTENANCE_KV_KEY).await?.as_deref() == Some("true"))
    }

    async fn set_maintenance_enabled(&self, enabled: bool) -> Result<(), StoreError> {
        self.set_kv(MAINTENANCE_KV_KEY, if enabled { "true" } else { "false" })
            .await
    }
}
