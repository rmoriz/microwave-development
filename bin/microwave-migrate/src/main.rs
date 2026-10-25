use clap::{Parser, Subcommand, ValueEnum};
use microwave_config::{Config, StorageCompression};
use microwave_migrate::{
    apply_knife_backup_plan_postgres, apply_knife_backup_plan_sqlite, export, export_postgres,
    from_api_with_options, import_postgres_with_compression, import_with_compression,
    prepare_knife_backup, redacted_postgres_url, validate_postgres_url, Compression,
    FromApiOptions, KnifeBackupPlan, MigrateError, DATABASE_FILE,
};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
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
        /// Compression for imported `serialized_object` values.
        ///
        /// Without this flag the default comes from --config,
        /// MW_CONFIG/./microwave.toml, or MW_STORAGE__COMPRESSION.
        #[arg(long, value_enum, value_name = "MODE")]
        compression: Option<CompressionArg>,
        /// Configuration file used to resolve the default compression.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// Import a `knife ec backup` repository directory without an HTTP server.
    ImportKnifeBackup {
        /// Backup directory created by `knife ec backup`.
        #[arg(long, value_name = "DIR")]
        input: PathBuf,
        /// Data directory receiving microwave.db (SQLite).
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
        /// Local object-store root for cookbook files.
        ///
        /// Defaults to <data-dir>/bookshelf for SQLite. Required with
        /// --database-url unless --dry-run is used.
        #[arg(long, value_name = "DIR")]
        bookshelf_dir: Option<PathBuf>,
        /// Skip the interactive confirmation. Required when stdin is not a
        /// terminal.
        #[arg(long)]
        yes: bool,
        /// Parse and validate the backup, print counts, and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Compression for imported `serialized_object` values.
        ///
        /// Without this flag the default comes from --config,
        /// MW_CONFIG/./microwave.toml, or MW_STORAGE__COMPRESSION.
        #[arg(long, value_enum, value_name = "MODE")]
        compression: Option<CompressionArg>,
        /// Configuration file used to resolve the default compression.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
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
            compression,
            config,
        } => import_command(
            data_dir,
            database_url,
            yes,
            &input,
            compression,
            config.as_deref(),
        ),
        Command::ImportKnifeBackup {
            input,
            data_dir,
            database_url,
            bookshelf_dir,
            yes,
            dry_run,
            compression,
            config,
        } => import_knife_backup_command(
            &input,
            data_dir,
            database_url,
            bookshelf_dir,
            yes,
            dry_run,
            compression,
            config.as_deref(),
        ),
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
    input: &Path,
    compression: Option<CompressionArg>,
    config: Option<&Path>,
) -> Result<(), MigrateError> {
    let compression = resolve_compression(compression, config)?;
    match (data_dir, database_url) {
        (Some(data_dir), None) => {
            let target = data_dir.join(DATABASE_FILE);
            println!("Import target: sqlite:{}", target.display());
            println!("Import compression: {}", compression.as_str());
            confirm_import(yes)?;
            import_with_compression(&data_dir, input, compression)
                .map(|()| println!("Imported snapshot from {}", input.display()))
        }
        (None, Some(database_url)) => {
            validate_postgres_url(&database_url)?;
            println!("Import target: {}", redacted_postgres_url(&database_url));
            println!("Import compression: {}", compression.as_str());
            confirm_import(yes)?;
            import_postgres_with_compression(&database_url, input, compression)
                .map(|()| println!("Imported snapshot from {}", input.display()))
        }
        _ => unreachable!("clap requires exactly one of --data-dir and --database-url"),
    }
}

#[allow(clippy::too_many_arguments)]
fn import_knife_backup_command(
    input: &Path,
    data_dir: Option<PathBuf>,
    database_url: Option<String>,
    bookshelf_dir: Option<PathBuf>,
    yes: bool,
    dry_run: bool,
    compression: Option<CompressionArg>,
    config: Option<&Path>,
) -> Result<(), MigrateError> {
    let compression = resolve_compression(compression, config)?;
    match (data_dir, database_url) {
        (Some(data_dir), None) => {
            let database = data_dir.join(DATABASE_FILE);
            let bookshelf = bookshelf_dir.unwrap_or_else(|| data_dir.join("bookshelf"));
            println!("Import target: sqlite:{}", database.display());
            println!("Bookshelf target: {}", bookshelf.display());
            println!("Import compression: {}", compression.as_str());
            let plan = prepare_knife_backup(input)?;
            print_knife_summary(&plan);
            if dry_run {
                println!("Dry run: no database or object-store changes were made");
                return Ok(());
            }
            confirm_import(yes)?;
            apply_knife_backup_plan_sqlite(&data_dir, &plan, &bookshelf, compression)
                .map(|_| println!("Imported knife backup from {}", input.display()))
        }
        (None, Some(database_url)) => {
            validate_postgres_url(&database_url)?;
            println!(
                "Import target: PostgreSQL {}",
                redacted_postgres_url(&database_url)
            );
            println!("Import compression: {}", compression.as_str());
            if !dry_run {
                if let Some(bookshelf) = &bookshelf_dir {
                    println!("Bookshelf target: {}", bookshelf.display());
                }
            }
            let plan = prepare_knife_backup(input)?;
            print_knife_summary(&plan);
            if dry_run {
                println!("Dry run: no database or object-store changes were made");
                return Ok(());
            }
            let Some(bookshelf) = bookshelf_dir else {
                return Err(MigrateError::Config(
                    "--bookshelf-dir is required with --database-url so cookbook blobs have a local object-store target"
                        .into(),
                ));
            };
            confirm_import(yes)?;
            apply_knife_backup_plan_postgres(&database_url, &plan, &bookshelf, compression)
                .map(|_| println!("Imported knife backup from {}", input.display()))
        }
        _ => unreachable!("clap requires exactly one of --data-dir and --database-url"),
    }
}

fn print_knife_summary(plan: &KnifeBackupPlan) {
    println!("Backup contents: {}", plan.summary.display_line());
    for warning in &plan.summary.warnings {
        eprintln!("warning: {warning}");
    }
}

/// Resolve import compression from the explicit flag, then the configuration
/// file (including `MW_STORAGE__COMPRESSION`), then the built-in default.
fn resolve_compression(
    explicit: Option<CompressionArg>,
    config_path: Option<&Path>,
) -> Result<Compression, MigrateError> {
    if let Some(compression) = explicit {
        return Ok(compression.into());
    }
    let config =
        Config::load(config_path).map_err(|error| MigrateError::Config(error.to_string()))?;
    Ok(match config.storage.compression {
        StorageCompression::Zlib => Compression::Zlib,
        StorageCompression::None => Compression::None,
    })
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CompressionArg {
    Zlib,
    None,
}

impl From<CompressionArg> for Compression {
    fn from(value: CompressionArg) -> Self {
        match value {
            CompressionArg::Zlib => Self::Zlib,
            CompressionArg::None => Self::None,
        }
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
