use clap::{Parser, Subcommand};
use microwave_migrate::{
    export, export_postgres, from_api_with_options, import, import_postgres, redacted_postgres_url,
    validate_postgres_url, FromApiOptions, MigrateError, DATABASE_FILE,
};
use std::io::{IsTerminal, Write};
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
    /// Write a versioned JSON snapshot of a local store.
    Export {
        /// Data directory containing microwave.db (SQLite).
        #[arg(
            long,
            value_name = "DIR",
            conflicts_with = "database_url",
            required_unless_present = "database_url"
        )]
        data_dir: Option<PathBuf>,
        /// PostgreSQL connection URL to export.
        #[arg(
            long,
            value_name = "URL",
            conflicts_with = "data_dir",
            required_unless_present = "data_dir"
        )]
        database_url: Option<String>,
        /// Snapshot file to write.
        #[arg(long, value_name = "FILE")]
        output: PathBuf,
    },
    /// Import a snapshot into a local store, creating it when needed.
    Import {
        /// Data directory containing (or receiving) microwave.db (SQLite).
        #[arg(
            long,
            value_name = "DIR",
            conflicts_with = "database_url",
            required_unless_present = "database_url"
        )]
        data_dir: Option<PathBuf>,
        /// PostgreSQL connection URL to import into.
        #[arg(
            long,
            value_name = "URL",
            conflicts_with = "data_dir",
            required_unless_present = "data_dir"
        )]
        database_url: Option<String>,
        /// Skip the interactive confirmation. Required when stdin is not a
        /// terminal.
        #[arg(long)]
        yes: bool,
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
        Command::Export {
            data_dir,
            database_url,
            output,
        } => export_command(data_dir, database_url, &output),
        Command::Import {
            data_dir,
            database_url,
            yes,
            input,
        } => import_command(data_dir, database_url, yes, &input),
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

fn export_command(
    data_dir: Option<PathBuf>,
    database_url: Option<String>,
    output: &std::path::Path,
) -> Result<(), MigrateError> {
    match (data_dir, database_url) {
        (Some(data_dir), None) => export(&data_dir, output)
            .map(|()| println!("Exported snapshot to {}", output.display())),
        (None, Some(database_url)) => export_postgres(&database_url, output)
            .map(|()| println!("Exported snapshot to {}", output.display())),
        _ => unreachable!("clap requires exactly one of --data-dir and --database-url"),
    }
}

fn import_command(
    data_dir: Option<PathBuf>,
    database_url: Option<String>,
    yes: bool,
    input: &std::path::Path,
) -> Result<(), MigrateError> {
    match (data_dir, database_url) {
        (Some(data_dir), None) => {
            let target = data_dir.join(DATABASE_FILE);
            println!("Import target: sqlite:{}", target.display());
            confirm_import(yes)?;
            import(&data_dir, input)
                .map(|()| println!("Imported snapshot from {}", input.display()))
        }
        (None, Some(database_url)) => {
            validate_postgres_url(&database_url)?;
            println!("Import target: {}", redacted_postgres_url(&database_url));
            confirm_import(yes)?;
            import_postgres(&database_url, input)
                .map(|()| println!("Imported snapshot from {}", input.display()))
        }
        _ => unreachable!("clap requires exactly one of --data-dir and --database-url"),
    }
}

fn confirm_import(yes: bool) -> Result<(), MigrateError> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(MigrateError::Confirmation(
            "import merges into live metadata and stdin is not a terminal; re-run with --yes after verifying the target"
                .into(),
        ));
    }
    print!("Type 'yes' to continue: ");
    std::io::stdout()
        .flush()
        .map_err(|error| MigrateError::Confirmation(error.to_string()))?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| MigrateError::Confirmation(error.to_string()))?;
    if answer.trim() == "yes" {
        Ok(())
    } else {
        Err(MigrateError::Confirmation("import cancelled".into()))
    }
}
