use crate::CtlError;
use microwave_config::{Config, DatabaseDriver};
use microwave_store::SqliteStore;
use std::path::{Path, PathBuf};

/// Resolved local paths and configuration for an operational command.
pub struct Context {
    pub config: Config,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
}

impl Context {
    pub fn load(config_path: Option<&Path>) -> Result<Self, CtlError> {
        let config = Config::load(config_path)?;
        if config.database.driver != DatabaseDriver::Sqlite {
            return Err(CtlError::message(
                "microwave-ctl only supports the sqlite database driver",
            ));
        }

        let url = config.resolved_database_url();
        let path = url.strip_prefix("sqlite://").ok_or_else(|| {
            CtlError::message(format!(
                "unsupported database URL {url:?}; expected sqlite://<path>"
            ))
        })?;
        if path.is_empty() {
            return Err(CtlError::message("the sqlite database path is empty"));
        }

        Ok(Self {
            data_dir: PathBuf::from(&config.paths.data_dir),
            database_path: PathBuf::from(path),
            config,
        })
    }

    pub fn prepare_data_dir(&self) -> Result<(), CtlError> {
        std::fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }

    pub fn open_store(&self) -> Result<SqliteStore, CtlError> {
        self.prepare_data_dir()?;
        Ok(SqliteStore::open(&self.database_path)?)
    }

    pub fn superuser_name(&self) -> &str {
        &self.config.auth.superuser_name
    }
}
