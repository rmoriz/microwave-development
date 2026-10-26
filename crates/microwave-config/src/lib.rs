//! Configuration types for Microwave Server.

pub mod config;

pub use config::{
    is_loopback_host, AuthConfig, ComplianceProxyConfig, Config, ConfigError, DataCollectorConfig,
    DatabaseConfig, DatabaseDriver, KeygenConfig, LicenseConfig, MailAuth, MailConfig,
    MailSecurity, ObjectStoreBackend, ObjectStoreConfig, PathsConfig, RequiredRecipeConfig,
    SearchBackend, SearchConfig, ServerConfig, StorageCompression, StorageConfig, TelemetryConfig,
    TlsConfig,
};
