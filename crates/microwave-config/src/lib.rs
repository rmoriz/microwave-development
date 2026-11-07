//! Configuration types for Microwave Server.

pub mod config;

pub use config::{
    is_loopback_host, valid_bucket_name, valid_org_name, AuthConfig, ComplianceProxyConfig, Config,
    ConfigError, DataCollectorConfig, DataCollectorStore, DatabaseConfig, DatabaseDriver,
    KeygenConfig, LicenseConfig, LimitsConfig, LimitsGlobalConfig, LimitsOrgConfig,
    LimitsOrgOverride, LimitsPrincipalConfig, LimitsRateConfig, MailAuth, MailConfig, MailSecurity,
    McpConfig, McpOrgConfig, ObjectStoreBackend, ObjectStoreConfig, ObjectStoreOrgConfig,
    OrgCredentials, OtelConfig, PathsConfig, RequiredRecipeConfig, SearchBackend, SearchConfig,
    ServerConfig, StorageCompression, StorageConfig, TelemetryConfig, TlsConfig,
};
