use crate::CtlError;
use microwave_config::{Config, DatabaseDriver, StorageCompression};
use microwave_store::{Compression, MetadataStore, SqliteStore};
use microwave_store_postgres::PostgresStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Resolved configuration and local paths for an operational command.
pub struct Context {
    pub config: Config,
    pub data_dir: PathBuf,
    /// Resolved database URL, including when a PostgreSQL backend is configured.
    pub database_url: String,
    /// Filesystem path for the SQLite backend, or `None` for PostgreSQL.
    pub database_path: Option<PathBuf>,
}

impl Context {
    pub fn load(config_path: Option<&Path>) -> Result<Self, CtlError> {
        let config = Config::load(config_path)?;
        let database_url = config.resolved_database_url();

        let database_path = match config.database.driver {
            DatabaseDriver::Sqlite => {
                let path = database_url.strip_prefix("sqlite://").ok_or_else(|| {
                    CtlError::message(format!(
                        "unsupported database URL {:?}; expected sqlite://<path>",
                        microwave_migrate::redacted_database_url(&database_url)
                    ))
                })?;
                if path.is_empty() {
                    return Err(CtlError::message("the sqlite database path is empty"));
                }
                Some(PathBuf::from(path))
            }
            DatabaseDriver::Postgres => {
                microwave_migrate::validate_postgres_url(&database_url)?;
                None
            }
        };

        Ok(Self {
            data_dir: PathBuf::from(&config.paths.data_dir),
            database_url,
            database_path,
            config,
        })
    }

    pub fn prepare_data_dir(&self) -> Result<(), CtlError> {
        std::fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }

    /// The SQLite database path. Commands that need the local metadata store
    /// fail clearly when PostgreSQL is configured instead.
    pub fn sqlite_path(&self) -> Result<&Path, CtlError> {
        self.database_path.as_deref().ok_or_else(|| {
            CtlError::message(
                "this command requires the sqlite database driver; configure sqlite://<path>",
            )
        })
    }

    /// Open the configured metadata store as a backend-neutral handle.
    ///
    /// Both backends satisfy [`MetadataStore`]. The handle owns a
    /// current-thread Tokio runtime so the synchronous CLI can call the async
    /// repository contract without turning every command into an async
    /// function. PostgreSQL connects with the embedded schema applied, matching
    /// server startup.
    pub fn open_metadata_store(&self) -> Result<StoreHandle, CtlError> {
        self.open_metadata_store_inner(true)
    }

    /// Open the store without applying PostgreSQL schema changes.
    ///
    /// Used by read-only diagnostics such as `status` and `check-config` so a
    /// probe does not create tables in an empty database. SQLite opens the
    /// existing file normally; callers that must not create it check
    /// `database_path` first.
    pub fn open_existing_metadata_store(&self) -> Result<StoreHandle, CtlError> {
        self.open_metadata_store_inner(false)
    }

    fn open_metadata_store_inner(&self, migrate: bool) -> Result<StoreHandle, CtlError> {
        let runtime = new_runtime()?;
        let compression = store_compression(self.config.storage.compression);
        let store: Arc<dyn MetadataStore> = match self.config.database.driver {
            DatabaseDriver::Sqlite => {
                self.prepare_data_dir()?;
                Arc::new(SqliteStore::open_with_compression(
                    self.sqlite_path()?,
                    compression,
                )?)
            }
            DatabaseDriver::Postgres if migrate => Arc::new(runtime.block_on(
                PostgresStore::connect_with_compression(&self.database_url, compression),
            )?),
            DatabaseDriver::Postgres => Arc::new(runtime.block_on(
                PostgresStore::connect_existing_with_compression(&self.database_url, compression),
            )?),
        };
        Ok(StoreHandle { store, runtime })
    }

    /// Open the SQLite store directly.
    ///
    /// Kept for operations that need SQLite-specific APIs (for example the
    /// `VACUUM INTO` backup path). Commands that only need the repository
    /// contract should use [`Context::open_metadata_store`].
    pub fn open_store(&self) -> Result<SqliteStore, CtlError> {
        self.prepare_data_dir()?;
        Ok(SqliteStore::open_with_compression(
            self.sqlite_path()?,
            store_compression(self.config.storage.compression),
        )?)
    }

    pub fn superuser_name(&self) -> &str {
        &self.config.auth.superuser_name
    }

    /// Serialized-object compression mode selected by configuration.
    pub fn store_compression(&self) -> Compression {
        store_compression(self.config.storage.compression)
    }
}

/// A backend-neutral store plus the runtime needed to drive its async traits.
pub struct StoreHandle {
    store: Arc<dyn MetadataStore>,
    runtime: tokio::runtime::Runtime,
}

impl StoreHandle {
    /// The underlying backend-neutral store.
    pub fn store(&self) -> &dyn MetadataStore {
        self.store.as_ref()
    }

    /// Run one repository future on the handle's current-thread runtime.
    ///
    /// Safety/usage: the CLI is synchronous, so this must only be called from
    /// ordinary synchronous code. Do not call it from inside another Tokio
    /// runtime or recursively from a future already driven by this handle; a
    /// current-thread runtime cannot be entered re-entrantly and would panic.
    pub fn run<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }
}

fn store_compression(compression: StorageCompression) -> Compression {
    match compression {
        StorageCompression::Zlib => Compression::Zlib,
        StorageCompression::None => Compression::None,
    }
}

fn new_runtime() -> Result<tokio::runtime::Runtime, CtlError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| CtlError::message(format!("failed to build tokio runtime: {error}")))
}
