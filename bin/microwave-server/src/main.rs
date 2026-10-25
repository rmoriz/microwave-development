mod bootstrap;

use clap::{Parser, Subcommand};
use microwave_api::{router, AppState};
use microwave_config::{Config, DatabaseDriver, ObjectStoreBackend, StorageCompression};
use microwave_objectstore::{LocalObjectStore, ObjectStore, S3Config, S3ObjectStore};
use microwave_store::{Compression, MetadataStore, SqliteStore};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(name = "microwave-server", version, about = "Microwave Server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the server version.
    Version,
    /// Print the resolved configuration as JSON.
    Config {
        /// Path to a TOML configuration file.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// Start the server.
    Serve {
        /// Path to a TOML configuration file.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Load configuration and initialize storage, then exit.
        #[arg(long)]
        check: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
        }
        Command::Config { config } => {
            let mut config = Config::load(config.as_deref())?;
            // `config` is a user-facing diagnostic. Never print credentials.
            redact_config_for_output(&mut config);
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        Command::Serve { config, check } => {
            let mut config = Config::load(config.as_deref())?;
            // Load and validate the pair before touching the database so a bad
            // certificate fails fast for both `serve` and `serve --check`.
            let tls = if config.server.tls.is_enabled() {
                Some(microwave_tls::load(&config.server.tls).map_err(|error| error.to_string())?)
            } else {
                None
            };
            let scheme = if tls.is_some() { "https" } else { "http" };
            let database_url = config.resolved_database_url();
            let compression = store_compression(config.storage.compression);
            let (store, sqlite): (Arc<dyn MetadataStore>, Option<Arc<SqliteStore>>) =
                match config.database.driver {
                    DatabaseDriver::Sqlite => {
                        let path = sqlite_path(&database_url);
                        let store = Arc::new(SqliteStore::open_with_compression(
                            Path::new(path),
                            compression,
                        )?);
                        (store.clone(), Some(store))
                    }
                    DatabaseDriver::Postgres => {
                        if !(database_url.starts_with("postgres://")
                            || database_url.starts_with("postgresql://"))
                        {
                            return Err("database.driver = postgres requires database.url / \
                                 MW_DATABASE__URL with a postgres:// URL"
                                .into());
                        }
                        let store = Arc::new(
                            microwave_store_postgres::PostgresStore::connect_with_compression(
                                &database_url,
                                compression,
                            )
                            .await?,
                        );
                        (store, None)
                    }
                };
            bootstrap::ensure_superuser(store.as_ref(), &config).await?;
            let secret = match config.object_store.backend {
                ObjectStoreBackend::Local => {
                    let secret =
                        bootstrap::ensure_object_store_secret(&config, store.as_ref()).await?;
                    config.object_store.secret = Some(secret.clone());
                    secret
                }
                // S3/Garage clients use presigned URLs directly, so there is
                // no local bookshelf token secret to bootstrap. Credentials
                // may still come from the secret store.
                _ => {
                    bootstrap::apply_bookshelf_secrets(&mut config, store.as_ref()).await?;
                    config.object_store.secret.clone().unwrap_or_default()
                }
            };
            let object_store = build_object_store(&config, &secret)?;

            if check {
                // Never print the database URL: it may contain credentials.
                let backend = match config.database.driver {
                    DatabaseDriver::Sqlite => {
                        format!("sqlite ({})", sqlite_path(&database_url))
                    }
                    DatabaseDriver::Postgres => "postgres".to_string(),
                };
                eprintln!(
                    "serve: configuration ok (bind {}, scheme {scheme}, database backend {backend})",
                    config.server.bind
                );
                return Ok(());
            }

            let bind = config.server.bind.clone();
            let state = match sqlite {
                Some(sqlite) => AppState::from_sqlite(sqlite, Arc::new(config), object_store)?,
                None => AppState::new(store, Arc::new(config), object_store)?,
            };
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            let local_addr = listener.local_addr()?;
            tracing::info!(%bind, %local_addr, scheme, "microwave-server listening");
            match tls {
                Some(tls) => {
                    let rustls_config =
                        axum_server::tls_rustls::RustlsConfig::from_config(tls.config);
                    let listener = listener.into_std()?;
                    axum_server::from_tcp_rustls(listener, rustls_config)?
                        .serve(router(state).into_make_service())
                        .await?;
                }
                None => {
                    axum::serve(listener, router(state)).await?;
                }
            }
        }
    }

    Ok(())
}

fn sqlite_path(database_url: &str) -> &str {
    database_url
        .strip_prefix("sqlite://")
        .unwrap_or(database_url)
}

fn store_compression(compression: StorageCompression) -> Compression {
    match compression {
        StorageCompression::Zlib => Compression::Zlib,
        StorageCompression::None => Compression::None,
    }
}

/// Hide credentials before printing configuration.
fn redact_config_for_output(config: &mut Config) {
    config.database.url = config.database.url.as_deref().map(redact_database_url);
    config.data_collector.token = config
        .data_collector
        .token
        .as_ref()
        .map(|_| "***".to_string());
    config.object_store.secret = config
        .object_store
        .secret
        .as_ref()
        .map(|_| "***".to_string());
    config.object_store.access_key = config
        .object_store
        .access_key
        .as_ref()
        .map(|_| "***".to_string());
    config.object_store.secret_key = config
        .object_store
        .secret_key
        .as_ref()
        .map(|_| "***".to_string());
}

/// Hide the password in a database URL before printing configuration.
///
/// `postgres://user:secret@host/db` becomes `postgres://user:***@host/db`.
/// URLs without credentials are returned unchanged.
fn redact_database_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let Some((userinfo, host)) = rest.rsplit_once('@') else {
        return url.to_string();
    };
    let Some((user, _password)) = userinfo.split_once(':') else {
        return url.to_string();
    };
    format!("{scheme}://{user}:***@{host}")
}

fn listener_scheme(config: &Config) -> &'static str {
    if config.server.tls.is_enabled() {
        "https"
    } else {
        "http"
    }
}

/// Base URL for local `/bookshelf` presigned links. It follows the listener
/// scheme because the server may serve HTTPS directly.
fn local_object_store_base_url(config: &Config) -> String {
    format!("{}://{}", listener_scheme(config), config.server.bind)
}

fn build_object_store(
    config: &Config,
    secret: &str,
) -> Result<Arc<dyn ObjectStore + Send + Sync>, Box<dyn std::error::Error>> {
    match config.object_store.backend {
        ObjectStoreBackend::Local => Ok(Arc::new(LocalObjectStore::new(
            Path::new(&config.paths.data_dir).join("bookshelf"),
            secret,
            local_object_store_base_url(config),
        ))),
        ObjectStoreBackend::S3 | ObjectStoreBackend::Garage => {
            let endpoint = config
                .object_store
                .endpoint
                .clone()
                .ok_or("object_store.endpoint is required for the s3/garage backend")?;
            let bucket = config
                .object_store
                .bucket
                .clone()
                .ok_or("object_store.bucket is required for the s3/garage backend")?;
            let access_key = config
                .object_store
                .access_key
                .clone()
                .ok_or("object_store.access_key is required for the s3/garage backend")?;
            let secret_key = config
                .object_store
                .secret_key
                .clone()
                .ok_or("object_store.secret_key is required for the s3/garage backend")?;
            let mut s3_config = S3Config::new(
                endpoint,
                bucket,
                config.object_store.region.clone(),
                access_key,
                secret_key,
            );
            s3_config.path_style = config.object_store.path_style;
            Ok(Arc::new(S3ObjectStore::new(s3_config)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{local_object_store_base_url, redact_config_for_output, redact_database_url};
    use microwave_config::Config;

    #[test]
    fn local_object_store_uses_https_only_when_tls_is_enabled() {
        let plain = Config::default();
        assert_eq!(local_object_store_base_url(&plain), "http://0.0.0.0:8000");

        let mut tls = Config::default();
        tls.server.tls.cert_path = Some("/etc/microwave/tls.crt".into());
        tls.server.tls.key_path = Some("/etc/microwave/tls.key".into());
        assert_eq!(local_object_store_base_url(&tls), "https://0.0.0.0:8000");

        let mut disabled = tls;
        disabled.server.tls.enabled = Some(false);
        assert_eq!(
            local_object_store_base_url(&disabled),
            "http://0.0.0.0:8000"
        );
    }

    #[test]
    fn redacts_database_data_collector_and_object_store_credentials() {
        let mut config = Config::default();
        config.database.url =
            Some("postgres://microwave:s3cr3t@db.example.com/microwave".to_string());
        config.data_collector.token = Some("collector-token".to_string());
        config.object_store.secret = Some("bookshelf-secret".to_string());
        config.object_store.access_key = Some("access-key".to_string());
        config.object_store.secret_key = Some("secret-key".to_string());

        redact_config_for_output(&mut config);

        assert_eq!(
            config.database.url.as_deref(),
            Some("postgres://microwave:***@db.example.com/microwave")
        );
        assert_eq!(config.data_collector.token.as_deref(), Some("***"));
        assert_eq!(config.object_store.secret.as_deref(), Some("***"));
        assert_eq!(config.object_store.access_key.as_deref(), Some("***"));
        assert_eq!(config.object_store.secret_key.as_deref(), Some("***"));
    }

    #[test]
    fn redacts_the_password_from_a_database_url() {
        assert_eq!(
            redact_database_url("postgres://microwave:s3cr3t@db.example.com/microwave"),
            "postgres://microwave:***@db.example.com/microwave"
        );
        assert_eq!(
            redact_database_url("postgresql://user:p:a@ss@db/microwave"),
            "postgresql://user:***@db/microwave"
        );
    }

    #[test]
    fn leaves_credential_free_urls_unchanged() {
        assert_eq!(
            redact_database_url("sqlite:///var/lib/microwave/microwave.db"),
            "sqlite:///var/lib/microwave/microwave.db"
        );
        assert_eq!(
            redact_database_url("postgres://db.example.com/microwave"),
            "postgres://db.example.com/microwave"
        );
    }
}
