use clap::{Parser, Subcommand, ValueEnum};
use microwave_config::{Config, StorageCompression};
use microwave_migrate::{
    apply_knife_backup_plan_postgres_with_options, apply_knife_backup_plan_sqlite_with_options,
    apply_server_secrets, archive_secrets_included, export_archive_postgres_with_private_keys,
    export_archive_with_private_keys, export_postgres_with_options, export_with_options,
    from_api_with_options_with_summary, import_archive_postgres_with_private_keys,
    import_archive_with_private_keys, import_postgres_with_compression, import_with_compression,
    prepare_knife_backup, prepare_server_secrets, read_snapshot, redacted_postgres_url,
    validate_postgres_url, Compression, FromApiOptions, KnifeBackupApplyOptions, KnifeBackupPlan,
    MigrateError, PrivateKeyExportOptions, PrivateKeyImportOptions, ServerSecretsOptions,
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
        /// Download cookbook file blobs into this local object-store root.
        ///
        /// Files are written as organization-<org_id>/checksum-<md5>.
        /// Existing blobs are skipped. Without this flag only metadata and
        /// checksum references are crawled.
        #[arg(long, value_name = "DIR")]
        bookshelf_dir: Option<PathBuf>,
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
            bookshelf_dir,
        } => {
            let mut options = FromApiOptions::new(&url, &client_name, &key, &output);
            options.org = org;
            options.api_version = api_version;
            options.bookshelf_dir = bookshelf_dir;
            from_api_with_options_with_summary(&options).map(|summary| {
                println!("Wrote legacy API snapshot to {}", output.display());
                if let Some(dir) = &options.bookshelf_dir {
                    println!("Bookshelf: {}", dir.display());
                    println!(
                        "Cookbook blobs: {} downloaded, {} already present",
                        summary.downloaded, summary.skipped
                    );
                }
            })
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
                    warn_if_partial_restore(read_snapshot(input)?.secrets_included);
                    import_with_compression(&data_dir, input, compression)
                        .map(|()| println!("Imported snapshot from {}", input.display()))
                }
                (None, Some(database_url)) => {
                    validate_postgres_url(&database_url)?;
                    println!("Import target: {}", redacted_postgres_url(&database_url));
                    println!("Import compression: {}", compression.as_str());
                    confirm_import(yes)?;
                    warn_if_partial_restore(read_snapshot(input)?.secrets_included);
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
                warn_if_partial_restore(archive_secrets_included(archive)?);
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
                warn_if_partial_restore(archive_secrets_included(archive)?);
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

fn warn_if_partial_restore(secrets_included: bool) {
    if !secrets_included {
        eprintln!(
            "warning: snapshot has secrets_included=false; database secret values (kvpair, \
             OAuth grants/tokens, and OAuth application secrets) are not in this archive. \
             Re-export with --include-secrets for a complete restore."
        );
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
