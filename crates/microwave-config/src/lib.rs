//! Configuration types for Microwave Server.

pub mod config;

pub use config::{
    AuthConfig, Config, ConfigError, DatabaseConfig, DatabaseDriver, ObjectStoreBackend,
    ObjectStoreConfig, PathsConfig, SearchBackend, SearchConfig, ServerConfig,
};
