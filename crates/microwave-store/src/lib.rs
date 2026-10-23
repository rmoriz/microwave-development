//! SQLite-backed metadata store for Microwave Server.

mod sqlite;

pub use sqlite::{SqliteStore, StoreError};
