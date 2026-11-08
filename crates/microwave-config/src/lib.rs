//! Configuration types for Microwave Server.

pub mod config;
pub mod tenant_map;

pub use config::{
    is_loopback_host, valid_bucket_name, valid_org_name, AuthConfig, ComplianceProxyConfig, Config,
    ConfigError, DataCollectorConfig, DataCollectorStore, DatabaseConfig, DatabaseDriver,
    KeygenConfig, LicenseConfig, LimitsConfig, LimitsGlobalConfig, LimitsOrgConfig,
    LimitsOrgOverride, LimitsPrincipalConfig, LimitsRateConfig, MailAuth, MailConfig, MailSecurity,
    McpConfig, McpOrgConfig, ObjectStoreBackend, ObjectStoreConfig, ObjectStoreOrgConfig,
    OrgCredentials, OrgQuotas, OtelConfig, PathsConfig, QuotasConfig, RequiredRecipeConfig,
    SearchBackend, SearchConfig, ServerConfig, StorageCompression, StorageConfig, TelemetryConfig,
    TlsConfig,
};
pub use tenant_map::{tenant_issues, TenantIssue};
