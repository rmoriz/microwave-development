use clap::{Parser, Subcommand};
use microwave_config::Config;
use std::path::PathBuf;

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
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
        }
        Command::Config { config } => {
            let config = Config::load(config.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        Command::Serve { config } => {
            let config = Config::load(config.as_deref())?;
            eprintln!(
                "serve: not implemented yet (bind {}, database {})",
                config.server.bind,
                config.resolved_database_url()
            );
        }
    }

    Ok(())
}
