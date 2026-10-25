use clap::{Parser, Subcommand, ValueEnum};
use microwave_config::{Config, StorageCompression};
use microwave_migrate::{
    apply_knife_backup_plan_postgres_with_options, apply_knife_backup_plan_sqlite_with_options,
    apply_server_secrets, export, export_postgres, from_api_with_options,
    import_postgres_with_compression, import_with_compression, prepare_knife_backup,
    prepare_server_secrets, redacted_postgres_url, validate_postgres_url, Compression,
    FromApiOptions, KnifeBackupApplyOptions, KnifeBackupPlan, MigrateError, ServerSecretsOptions,
    ServerSecretsPlan, DATABASE_FILE,
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
        /// Import server-owned private material from this directory.
        ///
        /// Reads only allowlisted files: pivotal.pem, pivotal.password,
        /// webui_priv.pem, webui_pub.pem, objectstore.secret,
        /// private-chef-secrets.json/private_chef_secrets.json, TLS
        /// certificate/key candidates, and ca/ or nginx/ca/ certificates.
        #[arg(long, value_name = "DIR", conflicts_with = "secrets_archive")]
        secrets_dir: Option<PathBuf>,
        /// Import server-owned private material from a .tar/.tar.gz archive.
        #[arg(long, value_name = "FILE", conflicts_with = "secrets_dir")]
        secrets_archive: Option<PathBuf>,
        /// Target data directory for server secrets.
        ///
        /// Defaults to --data-dir for SQLite or paths.data_dir from the loaded
        /// configuration when --database-url is used.
        #[arg(long, value_name = "DIR")]
        secrets_target_dir: Option<PathBuf>,
        /// Explicit TLS certificate target path (must also be a source file).
        #[arg(long, value_name = "PATH")]
        tls_cert: Option<PathBuf>,
        /// Explicit TLS private-key target path (must also be a source file).
        #[arg(long, value_name = "PATH")]
        tls_key: Option<PathBuf>,
        /// Overwrite existing server-secret target files.
        #[arg(long)]
        force: bool,
        /// Reuse existing target users with the same username.
        ///
        /// Without this flag a username collision (for example a bootstrapped
        /// pivotal user) fails with a clear error.
        #[arg(long)]
        merge_users: bool,
        /// Replace ACLs and group membership for targets present in the backup.
        ///
        /// Without this flag imports are additive and revoked grants survive.
        #[arg(long)]
        replace_acls: bool,
        /// Replace credentials/admin/profile/default key for merged users.
        ///
        /// By default --merge-users keeps non-empty target credentials, admin
        /// state, profile fields, and an existing target default key.
        #[arg(long)]
        replace_user_credentials: bool,
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
            secrets_dir,
            secrets_archive,
            secrets_target_dir,
            tls_cert,
            tls_key,
            force,
            merge_users,
            replace_acls,
            replace_user_credentials,
            yes,
            dry_run,
            compression,
            config,
        } => import_knife_backup_command(
            &input,
            data_dir,
            database_url,
            bookshelf_dir,
            secrets_dir,
            secrets_archive,
            secrets_target_dir,
            tls_cert,
            tls_key,
            force,
            merge_users,
            replace_acls,
            replace_user_credentials,
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
    secrets_dir: Option<PathBuf>,
    secrets_archive: Option<PathBuf>,
    secrets_target_dir: Option<PathBuf>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    force: bool,
    merge_users: bool,
    replace_acls: bool,
    replace_user_credentials: bool,
    yes: bool,
    dry_run: bool,
    compression: Option<CompressionArg>,
    config: Option<&Path>,
) -> Result<(), MigrateError> {
    let loaded = Config::load(config).map_err(|error| MigrateError::Config(error.to_string()))?;
    let compression = match compression {
        Some(compression) => compression.into(),
        None => compression_from_config(&loaded),
    };

    if replace_user_credentials && !merge_users {
        return Err(MigrateError::Config(
            "--replace-user-credentials requires --merge-users".into(),
        ));
    }
    if secrets_dir.is_none()
        && secrets_archive.is_none()
        && (tls_cert.is_some() || tls_key.is_some())
    {
        return Err(MigrateError::Config(
            "--tls-cert and --tls-key require --secrets-dir or --secrets-archive".into(),
        ));
    }

    let secrets_plan = if secrets_dir.is_some() || secrets_archive.is_some() {
        let target_data_dir = secrets_target_dir
            .clone()
            .or_else(|| data_dir.clone())
            .unwrap_or_else(|| PathBuf::from(&loaded.paths.data_dir));
        let configured_cert = configured_path(&loaded.server.tls.cert_path);
        let configured_key = configured_path(&loaded.server.tls.key_path);
        let plan = prepare_server_secrets(&ServerSecretsOptions {
            source_dir: secrets_dir,
            source_archive: secrets_archive,
            target_data_dir,
            tls_cert_target: tls_cert.clone().or(configured_cert),
            tls_key_target: tls_key.clone().or(configured_key),
            force,
        })?;
        if let Some(target) = &tls_cert {
            if !plan.writes.iter().any(|write| &write.target == target) {
                return Err(MigrateError::Config(format!(
                    "--tls-cert {} requires cert.pem or ssl/cert.pem in the secrets source",
                    target.display()
                )));
            }
        }
        if let Some(target) = &tls_key {
            if !plan.writes.iter().any(|write| &write.target == target) {
                return Err(MigrateError::Config(format!(
                    "--tls-key {} requires key.pem or ssl/key.pem in the secrets source",
                    target.display()
                )));
            }
        }
        Some(plan)
    } else {
        None
    };

    match (data_dir, database_url) {
        (Some(data_dir), None) => {
            let database = data_dir.join(DATABASE_FILE);
            let bookshelf = bookshelf_dir.unwrap_or_else(|| data_dir.join("bookshelf"));
            println!("Import target: sqlite:{}", database.display());
            println!("Bookshelf target: {}", bookshelf.display());
            println!("Import compression: {}", compression.as_str());
            let plan = prepare_knife_backup(input)?;
            print_knife_summary(&plan);
            print_server_secrets_summary(secrets_plan.as_ref());
            if dry_run {
                println!(
                    "Dry run: nothing was written; no metadata, bookshelf blobs, or server secrets were changed"
                );
                return Ok(());
            }
            print_knife_confirmation_scope(merge_users, replace_acls, replace_user_credentials);
            confirm_import(yes)?;
            apply_knife_backup_plan_sqlite_with_options(
                &data_dir,
                &plan,
                &bookshelf,
                compression,
                &KnifeBackupApplyOptions {
                    merge_users,
                    replace_acls,
                    replace_user_credentials,
                },
            )?;
            apply_secrets_if_present(secrets_plan.as_ref())?;
            println!("Imported knife backup from {}", input.display());
            Ok(())
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
            print_server_secrets_summary(secrets_plan.as_ref());
            if dry_run {
                println!(
                    "Dry run: nothing was written; no metadata, bookshelf blobs, or server secrets were changed"
                );
                return Ok(());
            }
            let Some(bookshelf) = bookshelf_dir else {
                return Err(MigrateError::Config(
                    "--bookshelf-dir is required with --database-url so cookbook blobs have a local object-store target"
                        .into(),
                ));
            };
            print_knife_confirmation_scope(merge_users, replace_acls, replace_user_credentials);
            confirm_import(yes)?;
            apply_knife_backup_plan_postgres_with_options(
                &database_url,
                &plan,
                &bookshelf,
                compression,
                &KnifeBackupApplyOptions {
                    merge_users,
                    replace_acls,
                    replace_user_credentials,
                },
            )?;
            apply_secrets_if_present(secrets_plan.as_ref())?;
            println!("Imported knife backup from {}", input.display());
            Ok(())
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

fn print_server_secrets_summary(plan: Option<&ServerSecretsPlan>) {
    let Some(plan) = plan else {
        return;
    };
    println!(
        "Server secrets: {} files ({} private, {} public)",
        plan.writes.len(),
        plan.private_count(),
        plan.public_count()
    );
    for warning in &plan.warnings {
        eprintln!("warning: {warning}");
    }
    for write in &plan.writes {
        println!(
            "  Server secret target: {} (mode {:04o})",
            write.target.display(),
            write.mode
        );
    }
}

fn print_knife_confirmation_scope(
    merge_users: bool,
    replace_acls: bool,
    replace_user_credentials: bool,
) {
    if merge_users {
        println!(
            "Confirmation scope: --merge-users will preserve target credentials and reconcile server-admins membership for merged users."
        );
    }
    if replace_acls {
        println!(
            "Confirmation scope: --replace-acls will replace ACLs and group membership for backup targets."
        );
    }
    if replace_user_credentials {
        println!(
            "Confirmation scope: --replace-user-credentials will replace credentials/admin/profile/default keys for merged users."
        );
    }
}

fn apply_secrets_if_present(plan: Option<&ServerSecretsPlan>) -> Result<(), MigrateError> {
    if let Some(plan) = plan {
        let count = apply_server_secrets(plan)?;
        println!("Imported {count} server secret files");
    }
    Ok(())
}

fn configured_path(value: &Option<String>) -> Option<PathBuf> {
    value
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

fn compression_from_config(config: &Config) -> Compression {
    match config.storage.compression {
        StorageCompression::Zlib => Compression::Zlib,
        StorageCompression::None => Compression::None,
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
            "import writes or merges into the target store/data directory and stdin is not a terminal; re-run with --yes after verifying the target"
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
