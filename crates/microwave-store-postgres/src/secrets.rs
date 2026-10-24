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
        // One statement handles both insert and conflict. A pre-existing NULL
        // (possible in imported/legacy rows) is treated as absent and filled
        // with the candidate, so the caller never receives a value the store
        // did not actually retain.
        let stored: Option<String> = sqlx::query_scalar(
            "INSERT INTO kvpair (key, value, updated_at) VALUES ($1, $2, $3)
             ON CONFLICT (key) DO UPDATE
             SET value = COALESCE(kvpair.value, EXCLUDED.value),
                 updated_at = CASE
                     WHEN kvpair.value IS NULL THEN EXCLUDED.updated_at
                     ELSE kvpair.updated_at
                 END
             RETURNING value",
        )
        .bind(&key)
        .bind(value)
        .bind(crate::store::now())
        .fetch_one(self.pool())
        .await
        .map_err(crate::store::db)?;
        stored.ok_or_else(|| {
            StoreError::Postgres(format!(
                "secret {group}.{name} returned NULL from set_secret_if_absent"
            ))
        })
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
