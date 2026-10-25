//! Metadata database liveness probe used by `GET /_status`.

use async_trait::async_trait;
use microwave_store::{HealthRepository, StoreError};

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
}
