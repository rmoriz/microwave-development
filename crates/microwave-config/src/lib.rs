//! Configuration types for Microwave Server.

pub mod config;

pub use config::{
    AuthConfig, Config, ConfigError, DataCollectorConfig, DatabaseConfig, DatabaseDriver,
    LicenseConfig, ObjectStoreBackend, ObjectStoreConfig, PathsConfig, RequiredRecipeConfig,
    SearchBackend, SearchConfig, ServerConfig, TelemetryConfig,
};
