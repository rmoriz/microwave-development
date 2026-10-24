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
    pub search: SearchConfig,
    pub object_store: ObjectStoreConfig,
    pub auth: AuthConfig,
}

impl Config {
    /// Parse configuration from a TOML string.
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        toml::from_str(input).map_err(ConfigError::Parse)
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
    /// - `MW_SERVER__BIND`
    /// - `MW_PATHS__DATA_DIR`
    /// - `MW_DATABASE__DRIVER`, `MW_DATABASE__URL`
    /// - `MW_SEARCH__BACKEND`, `MW_SEARCH__URL`
    /// - `MW_OBJECT_STORE__BACKEND`, `MW_OBJECT_STORE__BUCKET`, `MW_OBJECT_STORE__ENDPOINT`
    /// - `MW_OBJECT_STORE__REGION`, `MW_OBJECT_STORE__ACCESS_KEY`,
    ///   `MW_OBJECT_STORE__SECRET_KEY`, `MW_OBJECT_STORE__PATH_STYLE`
    /// - `MW_OBJECT_STORE__SECRET` (local bookshelf token secret)
    /// - `MW_AUTH__SKEW_SECONDS`, `MW_AUTH__SUPERUSER_NAME`
    pub fn apply_env(&mut self) -> Result<(), ConfigError> {
        if let Ok(value) = std::env::var("MW_SERVER__BIND") {
            self.server.bind = value;
        }
        if let Ok(value) = std::env::var("MW_SERVER__WORKER_THREADS") {
            self.server.worker_threads = parse_value("MW_SERVER__WORKER_THREADS", value)?;
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
        Ok(())
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8000".into(),
            worker_threads: 0,
        }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SearchConfig {
    pub backend: SearchBackend,
    pub url: Option<String>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackend::Embedded,
            url: None,
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
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            skew_seconds: 900,
            superuser_name: "pivotal".into(),
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
