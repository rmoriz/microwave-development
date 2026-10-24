use clap::{Parser, Subcommand};
use microwave_migrate::{export, from_api_with_options, import, FromApiOptions};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "microwave-migrate",
    version,
    about = "Export, import, and crawl Microwave Server migration snapshots"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Write a versioned JSON snapshot of the local store.
    Export {
        /// Data directory containing microwave.db.
        #[arg(long, value_name = "DIR")]
        data_dir: PathBuf,
        /// Snapshot file to write.
        #[arg(long, value_name = "FILE")]
        output: PathBuf,
    },
    /// Import a snapshot into a local store, creating it when needed.
    Import {
        /// Data directory containing (or receiving) microwave.db.
        #[arg(long, value_name = "DIR")]
        data_dir: PathBuf,
        /// Snapshot file to read.
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
    },
    /// Crawl a live legacy Chef Infra Server into a snapshot.
    FromApi {
        /// Base URL of the legacy server.
        #[arg(long, value_name = "URL")]
        url: String,
        /// Client or user name used to sign requests.
        #[arg(long, value_name = "NAME")]
        client_name: String,
        /// PEM private key used to sign requests.
        #[arg(long, value_name = "PATH")]
        key: PathBuf,
        /// Snapshot file to write.
        #[arg(long, value_name = "FILE")]
        output: PathBuf,
        /// Crawl only this organization.
        #[arg(long, value_name = "NAME")]
        org: Option<String>,
        /// X-Ops-Server-API-Version to send with every request.
        #[arg(long, value_name = "VERSION", default_value_t = 2)]
        api_version: u32,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Export { data_dir, output } => export(&data_dir, &output)
            .map(|()| println!("Exported snapshot to {}", output.display())),
        Command::Import { data_dir, input } => import(&data_dir, &input)
            .map(|()| println!("Imported snapshot from {}", input.display())),
        Command::FromApi {
            url,
            client_name,
            key,
            output,
            org,
            api_version,
        } => {
            let mut options = FromApiOptions::new(&url, &client_name, &key, &output);
            options.org = org;
            options.api_version = api_version;
            from_api_with_options(&options)
                .map(|()| println!("Wrote legacy API snapshot to {}", output.display()))
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
