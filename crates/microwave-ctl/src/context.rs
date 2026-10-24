use crate::CtlError;
use microwave_config::{Config, DatabaseDriver};
use microwave_store::SqliteStore;
use std::path::{Path, PathBuf};

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
                        "unsupported database URL {database_url:?}; expected sqlite://<path>"
                    ))
                })?;
                if path.is_empty() {
                    return Err(CtlError::message("the sqlite database path is empty"));
                }
                Some(PathBuf::from(path))
            }
            DatabaseDriver::Postgres => {
                if !(database_url.starts_with("postgres://")
                    || database_url.starts_with("postgresql://"))
                {
                    return Err(CtlError::message(format!(
                        "unsupported database URL {database_url:?}; expected postgres://<url>"
                    )));
                }
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

    pub fn open_store(&self) -> Result<SqliteStore, CtlError> {
        self.prepare_data_dir()?;
        Ok(SqliteStore::open(self.sqlite_path()?)?)
    }

    pub fn superuser_name(&self) -> &str {
        &self.config.auth.superuser_name
    }
}
