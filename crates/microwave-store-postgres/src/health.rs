//! Metadata database liveness probe used by `GET /_status`.

use async_trait::async_trait;
use microwave_store::{HealthRepository, PoolStats, StoreError};

use crate::store::{db, PostgresStore};

#[async_trait]
impl HealthRepository for PostgresStore {
    async fn health_check(&self) -> Result<(), StoreError> {
        let value: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(self.pool())
            .await
            .map_err(db)?;
        if value == 1 {
            Ok(())
        } else {
            Err(StoreError::Invalid(format!(
                "PostgreSQL health probe returned unexpected value {value}"
            )))
        }
    }

    async fn pool_stats(&self) -> Option<PoolStats> {
        let to_u32 = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
        let max = self.pool().options().get_max_connections();
        let idle = to_u32(self.pool().num_idle());
        let in_use = self.pool().size().saturating_sub(idle);
        Some(PoolStats { idle, in_use, max })
    }
}
