//! Maintenance-mode state stored in the unified `kvpair` table.
//!
//! This module only persists the operator intent. HTTP enforcement of
//! maintenance mode is not wired into the request middleware yet, so running
//! servers do not return `503 Service Unavailable` while the flag is set.

use crate::sqlite::{SqliteStore, StoreError};

/// Raw `kvpair` key that holds the maintenance flag.
pub const MAINTENANCE_KV_KEY: &str = "maintenance:enabled";

impl SqliteStore {
    /// Whether maintenance mode is enabled. A missing value means disabled.
    pub fn maintenance_enabled(&self) -> Result<bool, StoreError> {
        Ok(self.get_kv(MAINTENANCE_KV_KEY)?.as_deref() == Some("true"))
    }

    /// Persist the maintenance flag.
    pub fn set_maintenance_enabled(&self, enabled: bool) -> Result<(), StoreError> {
        self.set_kv(MAINTENANCE_KV_KEY, if enabled { "true" } else { "false" })
    }
}
