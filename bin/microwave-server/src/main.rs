mod bootstrap;

use clap::{Parser, Subcommand};
use microwave_api::{router, AppState};
use microwave_config::{Config, ObjectStoreBackend};
use microwave_objectstore::{LocalObjectStore, ObjectStore, S3Config, S3ObjectStore};
use microwave_store::SqliteStore;
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
            let config = Config::load(config.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        Command::Serve { config, check } => {
            let mut config = Config::load(config.as_deref())?;
            let database_path = database_path(&config);
            let store = Arc::new(SqliteStore::open(Path::new(&database_path))?);
            bootstrap::ensure_superuser(&store, &config)?;
            let secret = match config.object_store.backend {
                ObjectStoreBackend::Local => {
                    let secret = bootstrap::ensure_object_store_secret(&config, &store)?;
                    config.object_store.secret = Some(secret.clone());
                    secret
                }
                // S3/Garage clients use presigned URLs directly, so there is
                // no local bookshelf token secret to bootstrap. Credentials
                // may still come from the secret store.
                _ => {
                    bootstrap::apply_bookshelf_secrets(&mut config, &store)?;
                    config.object_store.secret.clone().unwrap_or_default()
                }
            };
            let object_store = build_object_store(&config, &secret)?;

            if check {
                eprintln!(
                    "serve: configuration ok (bind {}, database {})",
                    config.server.bind, database_path
                );
                return Ok(());
            }

            let bind = config.server.bind.clone();
            let state = AppState::new(store, Arc::new(config), object_store)?;
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            tracing::info!(%bind, "microwave-server listening");
            axum::serve(listener, router(state)).await?;
        }
    }

    Ok(())
}

fn database_path(config: &Config) -> String {
    let url = config.resolved_database_url();
    url.strip_prefix("sqlite://").unwrap_or(&url).to_string()
}

fn build_object_store(
    config: &Config,
    secret: &str,
) -> Result<Arc<dyn ObjectStore + Send + Sync>, Box<dyn std::error::Error>> {
    match config.object_store.backend {
        ObjectStoreBackend::Local => Ok(Arc::new(LocalObjectStore::new(
            Path::new(&config.paths.data_dir).join("bookshelf"),
            secret,
            format!("http://{}", config.server.bind),
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
