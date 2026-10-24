use async_trait::async_trait;
use microwave_store::{secret_kv_key, KvRepository, SecretsRepository, StoreError};

use crate::store::PostgresStore;

#[async_trait]
impl SecretsRepository for PostgresStore {
    async fn get_secret(&self, group: &str, name: &str) -> Result<Option<String>, StoreError> {
        self.get_kv(&secret_kv_key(group, name)).await
    }

    async fn set_secret(&self, group: &str, name: &str, value: &str) -> Result<(), StoreError> {
        validate_component("secret group", group)?;
        validate_component("secret name", name)?;
        if value.is_empty() {
            return Err(StoreError::Invalid("secret value must not be empty".into()));
        }
        self.set_kv(&secret_kv_key(group, name), value).await
    }

    async fn set_secret_if_absent(
        &self,
        group: &str,
        name: &str,
        value: &str,
    ) -> Result<String, StoreError> {
        validate_component("secret group", group)?;
        validate_component("secret name", name)?;
        if value.is_empty() {
            return Err(StoreError::Invalid("secret value must not be empty".into()));
        }
        let key = secret_kv_key(group, name);
        let inserted: Option<String> = sqlx::query_scalar(
            "INSERT INTO kvpair (key, value, updated_at) VALUES ($1, $2, $3)
             ON CONFLICT (key) DO NOTHING
             RETURNING value",
        )
        .bind(&key)
        .bind(value)
        .bind(crate::store::now())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::store::db)?;
        if let Some(inserted) = inserted {
            return Ok(inserted);
        }
        sqlx::query_scalar("SELECT value FROM kvpair WHERE key = $1")
            .bind(&key)
            .fetch_one(self.pool())
            .await
            .map_err(crate::store::db)
    }

    async fn remove_secret(&self, group: &str, name: &str) -> Result<(), StoreError> {
        validate_component("secret group", group)?;
        validate_component("secret name", name)?;
        if self.remove_kv(&secret_kv_key(group, name)).await? {
            Ok(())
        } else {
            Err(StoreError::NotFound(format!("secret {group}.{name}")))
        }
    }
}

fn validate_component(label: &str, value: &str) -> Result<(), StoreError> {
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-.".contains(character));
    if valid {
        Ok(())
    } else {
        Err(StoreError::Invalid(format!("invalid {label} {value:?}")))
    }
}
