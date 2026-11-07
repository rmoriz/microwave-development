//! Operator-managed secrets stored in the unified `kvpair` table.
//!
//! Each secret is addressed by a `(group, name)` pair and stored under the
//! namespaced key `secret:<group>:<name>`. Keeping secrets in the metadata
//! store means `microwave-ctl` can manage them without a separate secret
//! backend. Callers must treat values as sensitive: only the explicit
//! `show-secret` command prints them.

use crate::identity::now;
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::OptionalExtension;

/// Group used for the local object-store HMAC secret.
pub const OBJECT_STORE_SECRET_GROUP: &str = "object_store";
/// Name used for the local object-store HMAC secret.
pub const OBJECT_STORE_SECRET_NAME: &str = "secret";
/// Group used for bookshelf (blob store) credentials.
pub const BOOKSHELF_SECRET_GROUP: &str = "bookshelf";
/// Access key used by the S3/Garage object-store backend.
pub const BOOKSHELF_ACCESS_KEY_ID: &str = "access_key_id";
/// Secret key used by the S3/Garage object-store backend.
pub const BOOKSHELF_SECRET_ACCESS_KEY: &str = "secret_access_key";
/// Group used for the OTLP header secret.
pub const OTEL_SECRET_GROUP: &str = "otel";
/// Name used for the OTLP header secret.
pub const OTEL_SECRET_NAME: &str = "headers";
/// Group used for the optional dedicated WebUI keypair.
pub const WEBUI_SECRET_GROUP: &str = "webui";
/// Private half of the optional dedicated WebUI keypair.
pub const WEBUI_PRIVATE_KEY_NAME: &str = "private_key";
/// Public half of the optional dedicated WebUI keypair.
pub const WEBUI_PUBLIC_KEY_NAME: &str = "public_key";

/// Return the raw `kvpair` key for a secret.
pub fn secret_kv_key(group: &str, name: &str) -> String {
    format!("secret:{group}:{name}")
}

impl SqliteStore {
    /// Read a secret value, if it has been set.
    pub fn get_secret(&self, group: &str, name: &str) -> Result<Option<String>, StoreError> {
        self.get_kv(&secret_kv_key(group, name))
    }

    /// Set or overwrite a secret value.
    pub fn set_secret(&self, group: &str, name: &str, value: &str) -> Result<(), StoreError> {
        validate_component("secret group", group)?;
        validate_component("secret name", name)?;
        if value.is_empty() {
            return Err(StoreError::Invalid("secret value must not be empty".into()));
        }
        self.set_kv(&secret_kv_key(group, name), value)
    }

    /// Insert a secret only when it does not exist yet.
    ///
    /// Returns the value that is stored after the call: `value` when this call
    /// inserted it, or the existing value when another writer won the race.
    /// This makes first-start bootstrap safe across multiple HA replicas.
    pub fn set_secret_if_absent(
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
        let connection = self.connection();
        // One statement handles both insert and conflict, and returns the
        // value actually retained by the row. A legacy NULL value is treated
        // as absent and filled with the candidate.
        let stored: Option<String> = connection
            .query_row(
                "INSERT INTO kvpair (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE
                 SET value = COALESCE(kvpair.value, excluded.value),
                     updated_at = CASE
                         WHEN kvpair.value IS NULL THEN excluded.updated_at
                         ELSE kvpair.updated_at
                     END
                 RETURNING value",
                rusqlite::params![key, value, now()],
                |row| row.get(0),
            )
            .optional()?;
        stored.ok_or_else(|| {
            StoreError::Invalid(format!(
                "secret {group}.{name} was not retained by set_secret_if_absent"
            ))
        })
    }

    /// Remove a secret. Removing a missing secret is reported as `NotFound` so
    /// callers do not silently claim success.
    pub fn remove_secret(&self, group: &str, name: &str) -> Result<(), StoreError> {
        validate_component("secret group", group)?;
        validate_component("secret name", name)?;
        if self.remove_kv(&secret_kv_key(group, name))? {
            Ok(())
        } else {
            Err(StoreError::NotFound(format!("secret {group}.{name}")))
        }
    }

    /// Newest `updated_at` over every secret whose name starts with
    /// `name_prefix`, or `None` when there is none.
    ///
    /// `updated_at` is RFC 3339 to the second, so the lexicographic `MAX` is
    /// also the newest write. `name_prefix` is matched after `secret:`, so
    /// callers pass the group including its colon (`"object_store:"`).
    pub fn secret_stamp(&self, name_prefix: &str) -> Result<Option<String>, StoreError> {
        let pattern = format!("secret:{name_prefix}%");
        let connection = self.connection();
        let stamp = connection.query_row(
            "SELECT MAX(updated_at) FROM kvpair WHERE key LIKE ?1",
            rusqlite::params![pattern],
            |row| row.get(0),
        )?;
        Ok(stamp)
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

#[cfg(test)]
mod tests {
    use super::{secret_kv_key, SqliteStore};

    /// Per-organization credential names keep the `.` that separates the
    /// organization from the field.
    #[test]
    fn dotted_secret_names_are_stored_verbatim() {
        assert_eq!(
            secret_kv_key("object_store", "acme.access_key"),
            "secret:object_store:acme.access_key"
        );
    }

    #[test]
    fn set_secret_accepts_a_dotted_name() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .set_secret("object_store", "acme.secret_key", "s3cr3t")
            .unwrap();
        assert_eq!(
            store
                .get_secret("object_store", "acme.secret_key")
                .unwrap()
                .as_deref(),
            Some("s3cr3t")
        );
    }
}
