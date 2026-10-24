use crate::identity::now;
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension};

impl SqliteStore {
    /// Read a raw key/value pair. Commands use namespaced keys so unrelated
    /// features cannot collide.
    pub fn get_kv(&self, key: &str) -> Result<Option<String>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row("SELECT value FROM kvpair WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Insert or replace a raw key/value pair.
    pub fn set_kv(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let connection = self.connection();
        connection.execute(
            "INSERT INTO kvpair (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                           updated_at = excluded.updated_at",
            params![key, value, now()],
        )?;
        Ok(())
    }

    /// Remove a raw key/value pair. Returns `true` when a row was deleted.
    pub fn remove_kv(&self, key: &str) -> Result<bool, StoreError> {
        let connection = self.connection();
        let changed = connection.execute("DELETE FROM kvpair WHERE key = ?1", [key])?;
        Ok(changed != 0)
    }
}
