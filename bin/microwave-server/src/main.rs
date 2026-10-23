mod bootstrap;

use clap::{Parser, Subcommand};
use microwave_api::{router, AppState};
use microwave_config::Config;
use microwave_objectstore::LocalObjectStore;
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
            let secret = bootstrap::ensure_object_store_secret(&config)?;
            let object_store = Arc::new(LocalObjectStore::new(
                Path::new(&config.paths.data_dir).join("bookshelf"),
                &secret,
                format!("http://{}", config.server.bind),
            ));
            config.object_store.secret = Some(secret);

            if check {
                eprintln!(
                    "serve: configuration ok (bind {}, database {})",
                    config.server.bind, database_path
                );
                return Ok(());
            }

            let bind = config.server.bind.clone();
            let state = AppState {
                store,
                config: Arc::new(config),
                object_store,
            };
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
