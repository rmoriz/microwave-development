use clap::{Parser, Subcommand, ValueEnum};
use microwave_config::{Config, StorageCompression};
use microwave_migrate::{
    export_archive_postgres_with_private_keys, export_archive_with_private_keys,
    export_postgres_with_options, export_with_options, from_api_with_options,
    import_archive_postgres_with_private_keys, import_archive_with_private_keys,
    import_postgres_with_compression, import_with_compression, redacted_postgres_url,
    validate_postgres_url, Compression, FromApiOptions, MigrateError, PrivateKeyExportOptions,
    PrivateKeyImportOptions, DATABASE_FILE,
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
    /// Write a versioned JSON snapshot or a snapshot-plus-bookshelf archive.
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
        /// Snapshot file to write (exactly one of --output and --archive).
        #[arg(long, value_name = "FILE", conflicts_with = "archive")]
        output: Option<PathBuf>,
        /// Write a tar.gz archive containing snapshot.json and bookshelf/.
        #[arg(long, value_name = "FILE", conflicts_with = "output")]
        archive: Option<PathBuf>,
        /// Local bookshelf directory for a PostgreSQL --archive export.
        ///
        /// Defaults to <data-dir>/bookshelf for SQLite. S3/Garage objects are
        /// not fetched; replicate the bucket instead.
        #[arg(long, value_name = "DIR", requires = "archive")]
        bookshelf_dir: Option<PathBuf>,
        /// Include secret values: kvpair (secrets, sessions, maintenance),
        /// OAuth grants/tokens, and OAuth application secrets.
        #[arg(long)]
        include_secrets: bool,
        /// Include explicit private key and certificate files under secrets/.
        ///
        /// This reads <data-dir>/pivotal.pem, pivotal.password,
        /// objectstore.secret, private-chef-secrets.json, WebUI/oc-id key
        /// files, and the configured TLS cert/key paths. Off by default.
        #[arg(long, requires = "archive")]
        include_private_keys: bool,
        /// Configuration file used to find TLS certificate/key paths for
        /// --include-private-keys (and for import compression defaults).
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// Import a snapshot or archive into a local store, creating it when needed.
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
        /// Snapshot file to read (exactly one of --input and --archive).
        #[arg(long, value_name = "FILE", conflicts_with = "archive")]
        input: Option<PathBuf>,
        /// Read a tar.gz archive containing snapshot.json and bookshelf/.
        #[arg(long, value_name = "FILE", conflicts_with = "input")]
        archive: Option<PathBuf>,
        /// Local bookshelf directory to replace for a PostgreSQL --archive
        /// import. Not used for SQLite, which uses <data-dir>/bookshelf.
        #[arg(long, value_name = "DIR", requires = "archive")]
        bookshelf_dir: Option<PathBuf>,
        /// Restore private key/certificate files under this directory instead
        /// of the configured data-dir/TLS paths.
        #[arg(long, value_name = "DIR", requires = "archive")]
        secrets_dir: Option<PathBuf>,
        /// Replace existing private key/certificate files. Can invalidate
        /// existing clients that use the old keys.
        #[arg(long, requires = "archive")]
        force: bool,
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
            archive,
            bookshelf_dir,
            include_secrets,
            include_private_keys,
            config,
        } => export_command(
            data_dir,
            database_url,
            output.as_deref(),
            archive.as_deref(),
            bookshelf_dir.as_deref(),
            include_secrets,
            include_private_keys,
            config.as_deref(),
        ),
        Command::Import {
            data_dir,
            database_url,
            yes,
            input,
            archive,
            bookshelf_dir,
            secrets_dir,
            force,
            compression,
            config,
        } => import_command(
            data_dir,
            database_url,
            yes,
            input.as_deref(),
            archive.as_deref(),
            bookshelf_dir.as_deref(),
            secrets_dir.as_deref(),
            force,
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

#[allow(clippy::too_many_arguments)]
fn export_command(
    data_dir: Option<PathBuf>,
    database_url: Option<String>,
    output: Option<&Path>,
    archive: Option<&Path>,
    bookshelf_dir: Option<&Path>,
    include_secrets: bool,
    include_private_keys: bool,
    config_path: Option<&Path>,
) -> Result<(), MigrateError> {
    match (output, archive) {
        (Some(output), None) => {
            if bookshelf_dir.is_some() {
                return Err(MigrateError::Config(
                    "--bookshelf-dir is only valid with --archive".into(),
                ));
            }
            match (data_dir, database_url) {
                (Some(data_dir), None) => {
                    export_with_options(&data_dir, output, include_secrets)?;
                }
                (None, Some(database_url)) => {
                    export_postgres_with_options(&database_url, output, include_secrets)?;
                }
                _ => unreachable!("clap requires exactly one of --data-dir and --database-url"),
            }
            println!("Exported snapshot to {}", output.display());
            print_secret_mode(include_secrets);
            Ok(())
        }
        (None, Some(archive)) => {
            let private_options = if include_private_keys {
                Some(private_export_options(data_dir.as_deref(), config_path)?)
            } else {
                None
            };
            match (data_dir, database_url) {
                (Some(data_dir), None) => {
                    if bookshelf_dir.is_some() {
                        return Err(MigrateError::Config(
                            "--bookshelf-dir is only valid with --database-url; SQLite archives \
                             use <data-dir>/bookshelf"
                                .into(),
                        ));
                    }
                    export_archive_with_private_keys(
                        &data_dir,
                        archive,
                        include_secrets,
                        private_options.as_ref(),
                    )?;
                }
                (None, Some(database_url)) => {
                    let bookshelf_dir = bookshelf_dir.ok_or_else(|| {
                        MigrateError::Config(
                            "PostgreSQL archive export needs --bookshelf-dir pointing at the local \
                             bookshelf tree; for S3/Garage, replicate the bucket instead"
                                .into(),
                        )
                    })?;
                    export_archive_postgres_with_private_keys(
                        &database_url,
                        bookshelf_dir,
                        archive,
                        include_secrets,
                        private_options.as_ref(),
                    )?;
                    println!("Archive bookshelf source: {}", bookshelf_dir.display());
                }
                _ => unreachable!("clap requires exactly one of --data-dir and --database-url"),
            }
            println!("Exported archive to {}", archive.display());
            print_secret_mode(include_secrets);
            print_private_key_mode(include_private_keys);
            Ok(())
        }
        _ => Err(MigrateError::Config(
            "exactly one of --output and --archive is required".into(),
        )),
    }
}

fn private_export_options(
    data_dir: Option<&Path>,
    config_path: Option<&Path>,
) -> Result<PrivateKeyExportOptions, MigrateError> {
    let config =
        Config::load(config_path).map_err(|error| MigrateError::Config(error.to_string()))?;
    let data_dir = data_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(&config.paths.data_dir));
    Ok(PrivateKeyExportOptions {
        data_dir: Some(data_dir),
        tls_cert_path: config.server.tls.cert_path.as_ref().map(PathBuf::from),
        tls_key_path: config.server.tls.key_path.as_ref().map(PathBuf::from),
    })
}

fn print_private_key_mode(include_private_keys: bool) {
    if include_private_keys {
        println!(
            "Private keys: included under secrets/ with secrets/manifest.json; treat this archive \
             as a credential"
        );
    } else {
        println!(
            "Private keys: excluded (pass --include-private-keys to add pivotal, object-store, \
             WebUI/oc-id, and configured TLS files)"
        );
    }
}

fn print_secret_mode(include_secrets: bool) {
    if include_secrets {
        println!(
            "Secrets: included (kvpair, OAuth grants/tokens, and OAuth application secrets); treat \
             this file as a credential"
        );
    } else {
        println!(
            "Secrets: excluded (pass --include-secrets to include kvpair and OAuth grant/token \
             values)"
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn import_command(
    data_dir: Option<PathBuf>,
    database_url: Option<String>,
    yes: bool,
    input: Option<&Path>,
    archive: Option<&Path>,
    bookshelf_dir: Option<&Path>,
    secrets_dir: Option<&Path>,
    force: bool,
    compression: Option<CompressionArg>,
    config: Option<&Path>,
) -> Result<(), MigrateError> {
    let compression = resolve_compression(compression, config)?;
    match (input, archive) {
        (Some(input), None) => {
            if bookshelf_dir.is_some() || secrets_dir.is_some() || force {
                return Err(MigrateError::Config(
                    "--bookshelf-dir, --secrets-dir, and --force are only valid with --archive"
                        .into(),
                ));
            }
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
        (None, Some(archive)) => match (data_dir, database_url) {
            (Some(data_dir), None) => {
                if bookshelf_dir.is_some() {
                    return Err(MigrateError::Config(
                        "--bookshelf-dir is only valid when importing a PostgreSQL archive".into(),
                    ));
                }
                let private_options =
                    private_import_options(Some(&data_dir), secrets_dir, force, config)?;
                let target = data_dir.join(DATABASE_FILE);
                println!("Import target: sqlite:{}", target.display());
                println!(
                    "Archive bookshelf target: {}",
                    data_dir.join("bookshelf").display()
                );
                print_private_import_mode(secrets_dir);
                println!("Import compression: {}", compression.as_str());
                confirm_import(yes)?;
                import_archive_with_private_keys(&data_dir, archive, compression, &private_options)
                    .map(|()| println!("Imported archive from {}", archive.display()))
            }
            (None, Some(database_url)) => {
                validate_postgres_url(&database_url)?;
                let bookshelf_dir = bookshelf_dir.ok_or_else(|| {
                    MigrateError::Config(
                        "PostgreSQL archive import needs --bookshelf-dir pointing at the local \
                         bookshelf tree to replace; for S3/Garage, use bucket replication instead"
                            .into(),
                    )
                })?;
                let private_options = private_import_options(None, secrets_dir, force, config)?;
                println!("Import target: {}", redacted_postgres_url(&database_url));
                println!("Archive bookshelf target: {}", bookshelf_dir.display());
                print_private_import_mode(secrets_dir);
                println!("Import compression: {}", compression.as_str());
                confirm_import(yes)?;
                import_archive_postgres_with_private_keys(
                    &database_url,
                    bookshelf_dir,
                    archive,
                    compression,
                    &private_options,
                )
                .map(|()| println!("Imported archive from {}", archive.display()))
            }
            _ => unreachable!("clap requires exactly one of --data-dir and --database-url"),
        },
        _ => Err(MigrateError::Config(
            "exactly one of --input and --archive is required".into(),
        )),
    }
}

fn private_import_options(
    data_dir: Option<&Path>,
    secrets_dir: Option<&Path>,
    force: bool,
    config_path: Option<&Path>,
) -> Result<PrivateKeyImportOptions, MigrateError> {
    let config =
        Config::load(config_path).map_err(|error| MigrateError::Config(error.to_string()))?;
    let data_dir = data_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(&config.paths.data_dir));
    Ok(PrivateKeyImportOptions {
        force,
        secrets_dir: secrets_dir.map(Path::to_path_buf),
        data_dir: Some(data_dir),
        tls_cert_path: config.server.tls.cert_path.as_ref().map(PathBuf::from),
        tls_key_path: config.server.tls.key_path.as_ref().map(PathBuf::from),
    })
}

fn print_private_import_mode(secrets_dir: Option<&Path>) {
    match secrets_dir {
        Some(dir) => println!("Private keys: restore under {}", dir.display()),
        None => println!(
            "Private keys: restore to data-dir-relative and configured TLS paths (existing files \
             require --force)"
        ),
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
