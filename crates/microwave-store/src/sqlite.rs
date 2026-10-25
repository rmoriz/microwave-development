use crate::compression::Compression;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("postgres error: {0}")]
    Postgres(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid value: {0}")]
    Invalid(String),
    #[error("serialized object compression error: {0}")]
    Compression(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// A SQLite-backed metadata store.
pub struct SqliteStore {
    connection: Arc<Mutex<Connection>>,
    compression: Compression,
}

impl SqliteStore {
    /// Open a database on disk, creating parent directories if needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_compression(path, Compression::default())
    }

    /// Open a database on disk with an explicit serialized-object compression
    /// mode. Reads always accept both compressed and legacy plain values.
    pub fn open_with_compression(
        path: &Path,
        compression: Compression,
    ) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let connection = Connection::open(path)?;
        configure(&connection, true)?;
        migrate(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            compression,
        })
    }

    /// Open a private in-memory database. Intended for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::open_in_memory_with_compression(Compression::default())
    }

    /// Open a private in-memory database with an explicit compression mode.
    pub fn open_in_memory_with_compression(compression: Compression) -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        configure(&connection, false)?;
        migrate(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            compression,
        })
    }

    /// Open an existing database without running migrations or acquiring a
    /// write lock. Used by read-only export commands.
    pub fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        if !path.is_file() {
            return Err(StoreError::NotFound(format!("database {}", path.display())));
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            compression: Compression::default(),
        })
    }

    /// Compression mode used for newly written serialized objects.
    pub fn compression(&self) -> Compression {
        self.compression
    }

    pub(crate) fn connection(&self) -> MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Clone the shared connection handle so a blocking health probe can move
    /// its lock acquisition and query off the async runtime.
    pub(crate) fn connection_handle(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.connection)
    }

    /// Hold the shared connection lock to simulate a long-running store call.
    ///
    /// This deliberately exposes the connection guard only when the
    /// `test-support` feature is enabled so end-to-end health probe tests can
    /// verify that a held lock cannot stall `/_status`.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn lock_connection_for_test(&self) -> MutexGuard<'_, Connection> {
        self.connection()
    }

    /// Write a consistent snapshot of this database to `destination`.
    ///
    /// SQLite's `VACUUM INTO` includes committed WAL contents, so the copy is
    /// safe to take while the store is open. The destination must not exist.
    pub fn backup_to(&self, destination: &Path) -> Result<(), StoreError> {
        let destination = destination
            .to_str()
            .ok_or_else(|| StoreError::Invalid("backup path is not valid UTF-8".into()))?;
        let connection = self.connection();
        connection.execute("VACUUM INTO ?1", [destination])?;
        Ok(())
    }

    /// Check that `path` is an intact Microwave SQLite database without
    /// modifying it.
    pub fn verify_database(path: &Path) -> Result<(), StoreError> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        let integrity: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(StoreError::Invalid(format!(
                "database integrity check failed: {integrity}"
            )));
        }

        let has_users: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'users'",
            [],
            |row| row.get(0),
        )?;
        if has_users == 0 {
            return Err(StoreError::Invalid(
                "file is not a Microwave database".into(),
            ));
        }
        Ok(())
    }

    pub fn table_names(&self) -> Result<Vec<String>, StoreError> {
        let connection = self.connection();
        let mut statement = connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row?);
        }
        Ok(names)
    }

    pub fn journal_mode(&self) -> Result<String, StoreError> {
        let connection = self.connection();
        Ok(connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?)
    }

    pub fn foreign_keys_enabled(&self) -> Result<bool, StoreError> {
        let connection = self.connection();
        let enabled: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        Ok(enabled != 0)
    }
}

fn configure(connection: &Connection, disk: bool) -> Result<(), StoreError> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
    if disk {
        let _: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    }
    Ok(())
}

fn migrate(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(include_str!("../migrations/0001_initial.sql"))?;

    // `0001_initial.sql` uses `CREATE TABLE IF NOT EXISTS`, so databases
    // created before the user profile columns existed keep their old shape.
    // Add each column only when it is missing to keep startup idempotent.
    for column in ["display_name", "first_name", "middle_name", "last_name"] {
        add_column_if_missing(connection, "users", column, "TEXT")?;
    }
    Ok(())
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<(), StoreError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(());
        }
    }
    connection.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
    ))?;
    Ok(())
}
