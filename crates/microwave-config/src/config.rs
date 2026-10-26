use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Top-level configuration for Microwave Server.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub paths: PathsConfig,
    pub database: DatabaseConfig,
    pub storage: StorageConfig,
    pub search: SearchConfig,
    pub object_store: ObjectStoreConfig,
    pub auth: AuthConfig,
    pub keygen: KeygenConfig,
    pub license: LicenseConfig,
    pub data_collector: DataCollectorConfig,
    pub compliance_proxy: ComplianceProxyConfig,
    pub required_recipe: RequiredRecipeConfig,
    pub telemetry: TelemetryConfig,
}

impl Config {
    /// Parse configuration from a TOML string.
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(input).map_err(ConfigError::Parse)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate cross-field bounds that TOML types alone cannot express.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.keygen
            .validate()
            .map_err(|message| ConfigError::InvalidValue {
                key: "keygen".into(),
                value: message,
            })?;
        self.server
            .tls
            .validate()
            .map_err(|message| ConfigError::InvalidValue {
                key: "server.tls".into(),
                value: message,
            })?;
        if self.search.timeout_ms == 0 {
            return Err(ConfigError::InvalidValue {
                key: "search.timeout_ms".into(),
                value: "0".into(),
            });
        }
        if self.data_collector.timeout == 0 {
            return Err(ConfigError::InvalidValue {
                key: "data_collector.timeout".into(),
                value: "0".into(),
            });
        }
        if self.compliance_proxy.timeout == 0 {
            return Err(ConfigError::InvalidValue {
                key: "compliance_proxy.timeout".into(),
                value: "0".into(),
            });
        }
        self.compliance_proxy
            .validate()
            .map_err(|message| ConfigError::InvalidValue {
                key: "compliance_proxy".into(),
                value: message,
            })?;
        Ok(())
    }

    /// Load configuration from an explicit path, `MW_CONFIG`, `./microwave.toml`,
    /// or built-in defaults, in that order. Environment overrides are applied last.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let candidate = path
            .map(Path::to_path_buf)
            .or_else(|| std::env::var("MW_CONFIG").ok().map(PathBuf::from))
            .or_else(|| {
                let local = PathBuf::from("microwave.toml");
                local.exists().then_some(local)
            });

        let mut config = match candidate {
            Some(path) => {
                let input = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
                    path: path.display().to_string(),
                    source,
                })?;
                Self::from_toml_str(&input)?
            }
            None => Self::default(),
        };

        config.apply_env()?;
        Ok(config)
    }

    /// Apply `MW_*` environment overrides.
    ///
    /// Supported keys:
    /// - `MW_SERVER__BIND`, `MW_SERVER__TLS__ENABLED`,
    ///   `MW_SERVER__TLS__CERT_PATH`, `MW_SERVER__TLS__KEY_PATH`,
    ///   `MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS`
    /// - `MW_PATHS__DATA_DIR`
    /// - `MW_DATABASE__DRIVER`, `MW_DATABASE__URL`
    /// - `MW_STORAGE__COMPRESSION`
    /// - `MW_SEARCH__BACKEND`, `MW_SEARCH__URL`, `MW_SEARCH__TIMEOUT_MS`,
    ///   `MW_SEARCH__STRICT_RESULT_ACLS`
    /// - `MW_OBJECT_STORE__BACKEND`, `MW_OBJECT_STORE__BUCKET`, `MW_OBJECT_STORE__ENDPOINT`
    /// - `MW_OBJECT_STORE__REGION`, `MW_OBJECT_STORE__ACCESS_KEY`,
    ///   `MW_OBJECT_STORE__SECRET_KEY`, `MW_OBJECT_STORE__PATH_STYLE`
    /// - `MW_OBJECT_STORE__SECRET` (local bookshelf token secret)
    /// - `MW_AUTH__SKEW_SECONDS`, `MW_AUTH__SUPERUSER_NAME`,
    ///   `MW_AUTH__PASSWORD_RESET_TOKEN_IN_RESPONSE`
    /// - `MW_KEYGEN__SIZE`, `MW_KEYGEN__WORKERS`, `MW_KEYGEN__WARM_UP`
    /// - `MW_LICENSE__NODE_LICENSE`, `MW_LICENSE__UPGRADE_URL`
    /// - `MW_DATA_COLLECTOR__ROOT_URL`, `MW_DATA_COLLECTOR__TOKEN`,
    ///   `MW_DATA_COLLECTOR__TIMEOUT` (alias `MW_DATA_COLLECTOR__TIMEOUT_MS`),
    ///   `MW_DATA_COLLECTOR__ACTIONS_ENABLED`, `MW_DATA_COLLECTOR__SERVICE_HOSTNAME`
    /// - `MW_COMPLIANCE_PROXY__URL`, `MW_COMPLIANCE_PROXY__TOKEN`,
    ///   `MW_COMPLIANCE_PROXY__TOKEN_HEADER`, `MW_COMPLIANCE_PROXY__TIMEOUT`
    /// - `MW_REQUIRED_RECIPE__ENABLED` (alias `MW_REQUIRED_RECIPE__ENABLE`),
    ///   `MW_REQUIRED_RECIPE__PATH`
    /// - `MW_TELEMETRY__ENABLED`, `MW_TELEMETRY__REPORTING_URL`,
    ///   `MW_TELEMETRY__INTERVAL_SECONDS`
    pub fn apply_env(&mut self) -> Result<(), ConfigError> {
        if let Ok(value) = std::env::var("MW_SERVER__BIND") {
            self.server.bind = value;
        }
        if let Ok(value) = std::env::var("MW_SERVER__WORKER_THREADS") {
            self.server.worker_threads = parse_value("MW_SERVER__WORKER_THREADS", value)?;
        }
        if let Ok(value) = std::env::var("MW_SERVER__TLS__ENABLED") {
            self.server.tls.enabled = Some(parse_bool("MW_SERVER__TLS__ENABLED", &value)?);
        }
        if let Ok(value) = std::env::var("MW_SERVER__TLS__CERT_PATH") {
            self.server.tls.cert_path = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_SERVER__TLS__KEY_PATH") {
            self.server.tls.key_path = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS") {
            self.server.tls.reload_interval_seconds =
                parse_value("MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS", value)?;
        }
        if let Ok(value) = std::env::var("MW_PATHS__DATA_DIR") {
            self.paths.data_dir = value;
        }
        if let Ok(value) = std::env::var("MW_DATABASE__DRIVER") {
            self.database.driver = match value.to_ascii_lowercase().as_str() {
                "sqlite" => DatabaseDriver::Sqlite,
                "postgres" | "postgresql" => DatabaseDriver::Postgres,
                _ => {
                    return Err(ConfigError::InvalidValue {
                        key: "MW_DATABASE__DRIVER".into(),
                        value,
                    })
                }
            };
        }
        if let Ok(value) = std::env::var("MW_DATABASE__URL") {
            self.database.url = Some(value);
        }
        if let Ok(value) = std::env::var("MW_STORAGE__COMPRESSION") {
            self.storage.compression = match value.to_ascii_lowercase().as_str() {
                "zlib" => StorageCompression::Zlib,
                "none" => StorageCompression::None,
                _ => {
                    return Err(ConfigError::InvalidValue {
                        key: "MW_STORAGE__COMPRESSION".into(),
                        value,
                    })
                }
            };
        }
        if let Ok(value) = std::env::var("MW_SEARCH__BACKEND") {
            self.search.backend = match value.to_ascii_lowercase().as_str() {
                "embedded" => SearchBackend::Embedded,
                "opensearch" => SearchBackend::OpenSearch,
                "elasticsearch" => SearchBackend::Elasticsearch,
                _ => {
                    return Err(ConfigError::InvalidValue {
                        key: "MW_SEARCH__BACKEND".into(),
                        value,
                    })
                }
            };
        }
        if let Ok(value) = std::env::var("MW_SEARCH__URL") {
            self.search.url = Some(value);
        }
        if let Ok(value) = std::env::var("MW_SEARCH__STRICT_RESULT_ACLS") {
            self.search.strict_result_acls = parse_bool("MW_SEARCH__STRICT_RESULT_ACLS", &value)?;
        }
        if let Ok(value) = std::env::var("MW_SEARCH__TIMEOUT_MS") {
            self.search.timeout_ms = parse_value("MW_SEARCH__TIMEOUT_MS", value)?;
        }
        if let Ok(value) = std::env::var("MW_OBJECT_STORE__BACKEND") {
            self.object_store.backend = match value.to_ascii_lowercase().as_str() {
                "local" => ObjectStoreBackend::Local,
                "s3" => ObjectStoreBackend::S3,
                "garage" => ObjectStoreBackend::Garage,
                _ => {
                    return Err(ConfigError::InvalidValue {
                        key: "MW_OBJECT_STORE__BACKEND".into(),
                        value,
                    })
                }
            };
        }
        if let Ok(value) = std::env::var("MW_OBJECT_STORE__BUCKET") {
            self.object_store.bucket = Some(value);
        }
        if let Ok(value) = std::env::var("MW_OBJECT_STORE__ENDPOINT") {
            self.object_store.endpoint = Some(value);
        }
        if let Ok(value) = std::env::var("MW_OBJECT_STORE__REGION") {
            self.object_store.region = value;
        }
        if let Ok(value) = std::env::var("MW_OBJECT_STORE__ACCESS_KEY") {
            self.object_store.access_key = Some(value);
        }
        if let Ok(value) = std::env::var("MW_OBJECT_STORE__SECRET_KEY") {
            self.object_store.secret_key = Some(value);
        }
        if let Ok(value) = std::env::var("MW_OBJECT_STORE__PATH_STYLE") {
            self.object_store.path_style = parse_bool("MW_OBJECT_STORE__PATH_STYLE", &value)?;
        }
        if let Ok(value) = std::env::var("MW_OBJECT_STORE__SECRET") {
            self.object_store.secret = Some(value);
        }
        if let Ok(value) = std::env::var("MW_AUTH__SKEW_SECONDS") {
            self.auth.skew_seconds = parse_value("MW_AUTH__SKEW_SECONDS", value)?;
        }
        if let Ok(value) = std::env::var("MW_AUTH__SUPERUSER_NAME") {
            self.auth.superuser_name = value;
        }
        if let Ok(value) = std::env::var("MW_AUTH__PASSWORD_RESET_TOKEN_IN_RESPONSE") {
            self.auth.password_reset_token_in_response =
                parse_bool("MW_AUTH__PASSWORD_RESET_TOKEN_IN_RESPONSE", &value)?;
        }
        if let Ok(value) = std::env::var("MW_KEYGEN__SIZE") {
            self.keygen.size = parse_value("MW_KEYGEN__SIZE", value)?;
        }
        if let Ok(value) = std::env::var("MW_KEYGEN__WORKERS") {
            self.keygen.workers = parse_value("MW_KEYGEN__WORKERS", value)?;
        }
        if let Ok(value) = std::env::var("MW_KEYGEN__WARM_UP") {
            self.keygen.warm_up = parse_bool("MW_KEYGEN__WARM_UP", &value)?;
        }
        if let Ok(value) = std::env::var("MW_LICENSE__NODE_LICENSE") {
            self.license.node_license = parse_value("MW_LICENSE__NODE_LICENSE", value)?;
        }
        if let Ok(value) = std::env::var("MW_LICENSE__UPGRADE_URL") {
            self.license.upgrade_url = value;
        }
        if let Ok(value) = std::env::var("MW_DATA_COLLECTOR__ROOT_URL") {
            self.data_collector.root_url = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_DATA_COLLECTOR__TOKEN") {
            self.data_collector.token = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_DATA_COLLECTOR__TIMEOUT")
            .or_else(|_| std::env::var("MW_DATA_COLLECTOR__TIMEOUT_MS"))
        {
            self.data_collector.timeout = parse_value("MW_DATA_COLLECTOR__TIMEOUT", value)?;
        }
        if let Ok(value) = std::env::var("MW_DATA_COLLECTOR__ACTIONS_ENABLED") {
            self.data_collector.actions_enabled =
                parse_bool("MW_DATA_COLLECTOR__ACTIONS_ENABLED", &value)?;
        }
        if let Ok(value) = std::env::var("MW_DATA_COLLECTOR__SERVICE_HOSTNAME") {
            self.data_collector.service_hostname = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_COMPLIANCE_PROXY__URL") {
            self.compliance_proxy.url = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_COMPLIANCE_PROXY__TOKEN") {
            self.compliance_proxy.token = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_COMPLIANCE_PROXY__TOKEN_HEADER") {
            self.compliance_proxy.token_header = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_COMPLIANCE_PROXY__TIMEOUT")
            .or_else(|_| std::env::var("MW_COMPLIANCE_PROXY__TIMEOUT_MS"))
        {
            self.compliance_proxy.timeout = parse_value("MW_COMPLIANCE_PROXY__TIMEOUT", value)?;
        }
        if let Ok(value) = std::env::var("MW_COMPLIANCE_PROXY__INSECURE_SKIP_TLS_VERIFY") {
            self.compliance_proxy.insecure_skip_tls_verify =
                parse_bool("MW_COMPLIANCE_PROXY__INSECURE_SKIP_TLS_VERIFY", &value)?;
        }
        if let Ok(value) = std::env::var("MW_REQUIRED_RECIPE__ENABLED")
            .or_else(|_| std::env::var("MW_REQUIRED_RECIPE__ENABLE"))
        {
            self.required_recipe.enabled = parse_bool("MW_REQUIRED_RECIPE__ENABLED", &value)?;
        }
        if let Ok(value) = std::env::var("MW_REQUIRED_RECIPE__PATH") {
            self.required_recipe.path = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_TELEMETRY__ENABLED") {
            self.telemetry.enabled = parse_bool("MW_TELEMETRY__ENABLED", &value)?;
        }
        if let Ok(value) = std::env::var("MW_TELEMETRY__REPORTING_URL") {
            self.telemetry.reporting_url = non_empty(value);
        }
        if let Ok(value) = std::env::var("MW_TELEMETRY__INTERVAL_SECONDS") {
            self.telemetry.interval_seconds = parse_value("MW_TELEMETRY__INTERVAL_SECONDS", value)?;
        }
        self.validate()
    }

    /// Resolve the database URL. When no URL is configured, SQLite lives under the
    /// configured data directory.
    pub fn resolved_database_url(&self) -> String {
        if let Some(url) = &self.database.url {
            if !url.is_empty() {
                return url.clone();
            }
        }
        let data_dir = self.paths.data_dir.trim_end_matches('/');
        format!("sqlite://{data_dir}/microwave.db")
    }
}

fn parse_value<T>(key: &str, value: String) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    value.parse().map_err(|_| ConfigError::InvalidValue {
        key: key.into(),
        value,
    })
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn parse_bool(key: &str, value: &str) -> Result<bool, ConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(ConfigError::InvalidValue {
            key: key.into(),
            value: value.to_string(),
        }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    pub worker_threads: usize,
    pub tls: TlsConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8000".into(),
            worker_threads: 0,
            tls: TlsConfig::default(),
        }
    }
}

/// Optional TLS listener configuration.
///
/// Plain HTTP remains the default. TLS is enabled automatically when a
/// certificate or key path is configured, or explicitly with
/// `server.tls.enabled = true` (which then requires both paths). Setting
/// `enabled = false` keeps the listener on plain HTTP and ignores the paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TlsConfig {
    /// `None` enables TLS automatically when either path is set.
    pub enabled: Option<bool>,
    /// PEM certificate chain served to clients.
    pub cert_path: Option<String>,
    /// PEM private key matching the certificate. Never printed or logged.
    pub key_path: Option<String>,
    /// Periodic certificate/key reload interval in seconds. `0` disables the
    /// timer; SIGHUP still reloads on Unix. Requires TLS to be enabled.
    pub reload_interval_seconds: u64,
}

impl TlsConfig {
    /// Maximum accepted `reload_interval_seconds` (seven days).
    pub const MAX_RELOAD_INTERVAL_SECONDS: u64 = 604_800;

    /// Resolve whether the listener should speak TLS.
    pub fn is_enabled(&self) -> bool {
        self.enabled
            .unwrap_or_else(|| self.has_cert_path() || self.has_key_path())
    }

    /// Reject reload settings that cannot take effect.
    pub fn validate(&self) -> Result<(), String> {
        if self.reload_interval_seconds > Self::MAX_RELOAD_INTERVAL_SECONDS {
            return Err(format!(
                "server.tls.reload_interval_seconds must be at most {} (got {})",
                Self::MAX_RELOAD_INTERVAL_SECONDS,
                self.reload_interval_seconds
            ));
        }
        if self.reload_interval_seconds > 0 && !self.is_enabled() {
            return Err("server.tls.reload_interval_seconds requires TLS to be enabled".into());
        }
        Ok(())
    }

    fn has_cert_path(&self) -> bool {
        self.cert_path
            .as_deref()
            .is_some_and(|path| !path.trim().is_empty())
    }

    fn has_key_path(&self) -> bool {
        self.key_path
            .as_deref()
            .is_some_and(|path| !path.trim().is_empty())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PathsConfig {
    pub data_dir: String,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            data_dir: "./data".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DatabaseConfig {
    pub driver: DatabaseDriver,
    pub url: Option<String>,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            driver: DatabaseDriver::Sqlite,
            url: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseDriver {
    #[default]
    Sqlite,
    Postgres,
}

/// Metadata-storage behavior that applies to both database backends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StorageConfig {
    /// Compression for new `serialized_object` writes. Existing values are
    /// always readable, including after switching this to `none`.
    pub compression: StorageCompression,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            compression: StorageCompression::Zlib,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageCompression {
    /// Custom MWZ1 envelope: `mwz1:` + base64(zlib(JSON)). This is not
    /// byte-compatible with upstream storage; legacy plain JSON remains
    /// readable.
    #[default]
    Zlib,
    /// Plain JSON text, for operators who need raw database visibility.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SearchConfig {
    pub backend: SearchBackend,
    pub url: Option<String>,
    /// Request timeout in milliseconds for remote backends.
    ///
    /// A bounded timeout keeps `_bulk`, `_delete_by_query`, and `_refresh`
    /// requests from hanging forever when the cluster is unreachable or
    /// unresponsive. Must be greater than zero.
    pub timeout_ms: u64,
    /// Hide search rows the caller cannot READ through object ACLs. Upstream
    /// calls this `strict_search_result_acls`; Microwave defaults to `true` so
    /// remote and embedded backends both filter unless explicitly disabled.
    pub strict_result_acls: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackend::Embedded,
            url: None,
            timeout_ms: 30_000,
            strict_result_acls: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchBackend {
    #[default]
    Embedded,
    OpenSearch,
    Elasticsearch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ObjectStoreConfig {
    pub backend: ObjectStoreBackend,
    pub bucket: Option<String>,
    pub endpoint: Option<String>,
    /// Shared secret for local `/bookshelf` HMAC tokens.
    pub secret: Option<String>,
    pub region: String,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    pub path_style: bool,
}

impl Default for ObjectStoreConfig {
    fn default() -> Self {
        Self {
            backend: ObjectStoreBackend::Local,
            bucket: None,
            endpoint: None,
            secret: None,
            region: "us-east-1".into(),
            access_key: None,
            secret_key: None,
            path_style: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ObjectStoreBackend {
    #[default]
    Local,
    S3,
    Garage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AuthConfig {
    pub skew_seconds: u64,
    pub superuser_name: String,
    /// Development-only: include a usable password-reset URL in the HTTP
    /// response. Off by default because the caller is unauthenticated.
    pub password_reset_token_in_response: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            skew_seconds: 900,
            superuser_name: "pivotal".into(),
            password_reset_token_in_response: false,
        }
    }
}

/// Pre-generated RSA keypair cache settings.
///
/// API key creation paths normally take a keypair from this cache instead of
/// generating one inline. Background workers refill the cache as soon as room
/// appears; `warm_up` additionally fills it synchronously during startup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct KeygenConfig {
    /// Maximum number of keypairs kept ready. `0` disables caching and makes
    /// every request generate a keypair on demand.
    pub size: usize,
    /// Background worker threads. Capped at `size`; `0` disables background
    /// generation (the cache is then only filled by `warm_up` or on demand).
    pub workers: usize,
    /// Fill the cache synchronously at startup instead of in the background.
    pub warm_up: bool,
}

impl KeygenConfig {
    /// Hard upper bound for `size`. Mirrors the cache's defensive cap.
    pub const MAX_SIZE: usize = 1024;
    /// Hard upper bound for `workers`. Mirrors the cache's defensive cap.
    pub const MAX_WORKERS: usize = 8;

    /// Reject combinations that would make startup allocate or spawn too much.
    pub fn validate(&self) -> Result<(), String> {
        if self.size > Self::MAX_SIZE {
            return Err(format!(
                "keygen.size must be at most {} (got {})",
                Self::MAX_SIZE,
                self.size
            ));
        }
        if self.workers > Self::MAX_WORKERS {
            return Err(format!(
                "keygen.workers must be at most {} (got {})",
                Self::MAX_WORKERS,
                self.workers
            ));
        }
        if self.workers > self.size {
            return Err(format!(
                "keygen.workers must not exceed keygen.size ({} > {})",
                self.workers, self.size
            ));
        }
        if self.size == 0 && self.warm_up {
            return Err("keygen.warm_up requires keygen.size > 0".into());
        }
        Ok(())
    }
}

impl Default for KeygenConfig {
    fn default() -> Self {
        Self {
            size: 3,
            workers: 1,
            warm_up: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LicenseConfig {
    /// Maximum number of nodes permitted by the license. Upstream reads
    /// `node_license` from `oc_chef_wm` configuration.
    pub node_license: u64,
    /// URL advertised by `GET /license`.
    pub upgrade_url: String,
}

impl Default for LicenseConfig {
    fn default() -> Self {
        Self {
            node_license: 1000,
            upgrade_url: "https://www.chef.io/pricing".into(),
        }
    }
}

/// Data Collector forwarding configuration, mirroring upstream's
/// `data_collector['root_url']`, `data_collector['token']` and
/// `data_collector['timeout']` settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DataCollectorConfig {
    /// Base URL of the external data collector. Setting this enables
    /// `POST /organizations/:org/data-collector`.
    pub root_url: Option<String>,
    /// Shared secret accepted from clients as `x-data-collector-token` and
    /// sent upstream. The forwarded value always comes from this server-side
    /// setting (or the `data_collector.token` secret), never from the client.
    pub token: Option<String>,
    /// Request timeout in milliseconds. Upstream key is `timeout` and the
    /// default is 30000.
    #[serde(alias = "timeout_ms")]
    pub timeout: u64,
    /// Send `oc_chef_action` mutation notifications for successful writes.
    /// This is only active when `root_url` is set. It defaults to false so a
    /// configuration that only enables client payload forwarding does not
    /// unexpectedly start server-side writes to the collector.
    pub actions_enabled: bool,
    /// Hostname reported as `service_hostname` in action messages. When unset
    /// the request `X-Forwarded-Host` (or `Host`) is used.
    pub service_hostname: Option<String>,
}

impl DataCollectorConfig {
    /// True when a non-empty `root_url` is configured.
    pub fn is_configured(&self) -> bool {
        self.root_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
    }

    /// True when server-side mutation notifications should be dispatched.
    pub fn actions_enabled(&self) -> bool {
        self.is_configured() && self.actions_enabled
    }
}

impl Default for DataCollectorConfig {
    fn default() -> Self {
        Self {
            root_url: None,
            token: None,
            timeout: 30_000,
            actions_enabled: false,
            service_hostname: None,
        }
    }
}

/// Compliance profile proxy configuration. When `url` is set, the two
/// upstream nginx compatibility routes are forwarded to Automate:
///
/// - `GET /organizations/:org/owners/:owner/compliance[/profile]`
/// - `GET /compliance/organizations/:org/owners/:owner/compliance[/profile]`
///
/// The upstream path is rewritten to `/compliance/profiles/:owner[/profile]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ComplianceProxyConfig {
    /// Base URL of the Automate/compliance service, for example
    /// `https://automate.example.com`. A non-empty value enables the routes.
    pub url: Option<String>,
    /// Optional service token sent on the forwarded request. When omitted the
    /// server falls back to the configured `data_collector.token` (or the
    /// `data_collector.token` secret), matching upstream nginx.
    pub token: Option<String>,
    /// Header used to carry `token`. Defaults to `x-data-collector-token`.
    pub token_header: Option<String>,
    /// Request timeout in milliseconds. Defaults to 30000.
    #[serde(alias = "timeout_ms")]
    pub timeout: u64,
    /// Skip TLS certificate verification for the compliance upstream. Keep
    /// this false outside test environments or a trusted private CA setup.
    pub insecure_skip_tls_verify: bool,
}

impl ComplianceProxyConfig {
    /// True when a non-empty `url` is configured.
    pub fn is_configured(&self) -> bool {
        self.url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
    }

    /// Validate the upstream URL and token header. Called by
    /// [`Config::validate`] so a bad value fails at startup, not on the first
    /// proxied request.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(url) = self
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            let parsed = url::Url::parse(url)
                .map_err(|error| format!("compliance_proxy.url is invalid: {error}"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(format!(
                    "compliance_proxy.url must use http or https (got {})",
                    parsed.scheme()
                ));
            }
            if parsed.host_str().is_none() {
                return Err("compliance_proxy.url must include a host".into());
            }
        }

        if let Some(header) = self
            .token_header
            .as_deref()
            .map(str::trim)
            .filter(|header| !header.is_empty())
        {
            validate_token_header_name(header)?;
        }
        Ok(())
    }
}

/// Header names that must never carry the compliance service token: either
/// they change request framing/routing, or they are explicitly managed by the
/// proxy. The list also covers hop-by-hop headers that must not be set by an
/// application.
const FORBIDDEN_TOKEN_HEADERS: &[&str] = &[
    "host",
    "content-type",
    "content-length",
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "accept",
    "accept-encoding",
    "user-agent",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-real-ip",
    "forwarded",
    "x-data-collector-auth",
];

fn validate_token_header_name(header: &str) -> Result<(), String> {
    if !header
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!(
            "compliance_proxy.token_header {header:?} is not a valid header name"
        ));
    }
    if FORBIDDEN_TOKEN_HEADERS.contains(&header.to_ascii_lowercase().as_str()) {
        return Err(format!(
            "compliance_proxy.token_header must not be a framing, routing, or hop-by-hop header ({header:?})"
        ));
    }
    Ok(())
}

impl Default for ComplianceProxyConfig {
    fn default() -> Self {
        Self {
            url: None,
            token: None,
            token_header: None,
            timeout: 30_000,
            insecure_skip_tls_verify: false,
        }
    }
}

/// Optional server-enforced recipe, mirroring upstream's
/// `required_recipe['enable']` and `required_recipe['path']`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RequiredRecipeConfig {
    #[serde(alias = "enable")]
    pub enabled: bool,
    /// Path to the recipe file served by `GET /organizations/:org/required_recipe`.
    pub path: Option<String>,
}

/// Telemetry settings. Export is opt-in and disabled by default: Microwave
/// never phones home unless an operator both enables telemetry and configures
/// a reporting URL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub reporting_url: Option<String>,
    pub interval_seconds: u64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            reporting_url: None,
            interval_seconds: 86_400,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid value for {key}: {value}")]
    InvalidValue { key: String, value: String },
}
