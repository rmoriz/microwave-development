use crate::pg::{normalize_database_name, PgTarget};
use crate::{context::Context, BackupFormat, CtlError};
use chrono::Utc;
use microwave_config::DatabaseDriver;
use microwave_store::SqliteStore;
use std::io::{IsTerminal, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const DATABASE_FILE: &str = "microwave.db";
const POSTGRES_DUMP_FILE: &str = "microwave.dump";
const CONFIG_FILE: &str = "microwave-config.json";
const BOOKSHELF_DIR: &str = "bookshelf";

/// Compatibility options accepted by `backup`.
pub struct BackupOptions<'a> {
    /// Upstream `--yes`. Microwave backups run online, so this is accepted
    /// without stopping the service.
    pub yes: bool,
    /// Write the resolved configuration only; skip metadata and bookshelf.
    pub config_only: bool,
    /// Extra flags for `pg_dump` (PostgreSQL only).
    pub pg_options: Option<&'a str>,
}

/// Default destination for a backup taken without an explicit path.
pub fn default_backup_path(format: BackupFormat) -> PathBuf {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let name = format!("microwave-backup-{timestamp}");
    match format {
        BackupFormat::Directory => PathBuf::from(name),
        BackupFormat::Tar => PathBuf::from(format!("{name}.tar")),
    }
}

/// Create a backup containing a consistent database dump and a copy of the
/// local bookshelf, when one exists.
///
/// SQLite uses `VACUUM INTO`; PostgreSQL uses a custom-format `pg_dump`
/// archive. Both include committed WAL/transaction contents and are safe to
/// take while a server is running. The directory format writes the files
/// as-is; the tar format writes the same layout into one uncompressed archive.
pub fn backup(
    context: &Context,
    path: Option<&Path>,
    format: BackupFormat,
    options: &BackupOptions<'_>,
) -> Result<(), CtlError> {
    let owned_path;
    let path = match path {
        Some(path) => path,
        None => {
            owned_path = default_backup_path(format);
            &owned_path
        }
    };

    if options.yes {
        println!("note: --yes accepted; this backup runs online and does not stop the service.");
    }
    if options.pg_options.is_some() && context.config.database.driver == DatabaseDriver::Sqlite {
        println!("note: --pg-options is ignored for the sqlite backend.");
    }

    match format {
        BackupFormat::Directory => backup_directory(context, path, options)?,
        BackupFormat::Tar => backup_tar(context, path, options)?,
    }

    println!("Backup written to {}", path.display());
    if options.config_only {
        println!("note: this backup contains configuration only; no metadata or bookshelf data was included.");
    } else {
        println!(
            "note: the bookshelf copy is not atomic with the database dump; drain writes for a strict backup."
        );
    }
    Ok(())
}

fn backup_directory(
    context: &Context,
    path: &Path,
    options: &BackupOptions<'_>,
) -> Result<(), CtlError> {
    if path.exists() && !path.is_dir() {
        return Err(CtlError::message(format!(
            "backup destination {} is not a directory; use --format tar for a file destination",
            path.display()
        )));
    }
    std::fs::create_dir_all(path)?;
    write_backup_payload(context, path, options)
}

fn backup_tar(context: &Context, path: &Path, options: &BackupOptions<'_>) -> Result<(), CtlError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            return Err(CtlError::message(format!(
                "backup destination {} is a directory; choose a file path for --format tar",
                path.display()
            )));
        }
        Ok(_) => {
            return Err(CtlError::message(format!(
                "backup destination {} already exists; remove it or choose a new path",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)?;
    }
    let staging = tempfile::Builder::new()
        .prefix(".microwave-backup-")
        .tempdir_in(parent.unwrap_or_else(|| Path::new(".")))?;
    write_backup_payload(context, staging.path(), options)?;

    // Build the archive beside the destination so the final rename is atomic
    // and a failure never leaves a partial archive under the requested name.
    let archive_temp = tempfile::Builder::new()
        .prefix(".microwave-backup-")
        .suffix(".tar")
        .tempfile_in(parent.unwrap_or_else(|| Path::new(".")))?;
    let archive_path = archive_temp.into_temp_path();
    let file = std::fs::File::create(&archive_path)?;
    let mut builder = tar::Builder::new(file);
    // Never archive a link target. The staging copy rejects symlinks before
    // this point; disabling follow_symlinks keeps the tar builder from
    // dereferencing anything that appears later as well.
    builder.follow_symlinks(false);

    for name in [DATABASE_FILE, POSTGRES_DUMP_FILE, CONFIG_FILE] {
        let payload = staging.path().join(name);
        if payload.is_file() {
            builder.append_path_with_name(&payload, name)?;
        }
    }

    let bookshelf = staging.path().join(BOOKSHELF_DIR);
    if bookshelf.is_dir() {
        builder.append_dir_all(BOOKSHELF_DIR, &bookshelf)?;
    }
    builder.finish()?;
    drop(builder);

    std::fs::rename(&archive_path, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn write_backup_payload(
    context: &Context,
    path: &Path,
    options: &BackupOptions<'_>,
) -> Result<(), CtlError> {
    if options.config_only {
        return write_config_snapshot(context, path);
    }

    match context.config.database.driver {
        DatabaseDriver::Sqlite => backup_sqlite(context, path)?,
        DatabaseDriver::Postgres => backup_postgres(context, path, options.pg_options)?,
    }

    let bookshelf_backup = path.join(BOOKSHELF_DIR);
    remove_path(&bookshelf_backup)?;
    let bookshelf = context.data_dir.join(BOOKSHELF_DIR);
    if bookshelf.is_dir() {
        copy_dir_all(&bookshelf, &bookshelf_backup)?;
    }
    Ok(())
}

fn write_config_snapshot(context: &Context, path: &Path) -> Result<(), CtlError> {
    let destination = path.join(CONFIG_FILE);
    if destination.exists() {
        return Err(CtlError::message(format!(
            "backup destination already contains {}",
            destination.display()
        )));
    }
    let serialized = serde_json::to_vec_pretty(&context.config).map_err(|error| {
        CtlError::message(format!("failed to serialize configuration: {error}"))
    })?;
    write_private_file(&destination, &serialized)
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), CtlError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn backup_sqlite(context: &Context, path: &Path) -> Result<(), CtlError> {
    let database_backup = path.join(DATABASE_FILE);
    if database_backup.exists() {
        return Err(CtlError::message(format!(
            "backup destination already contains {}",
            database_backup.display()
        )));
    }

    let store = context.open_store()?;
    store.backup_to(&database_backup)?;
    Ok(())
}

fn backup_postgres(
    context: &Context,
    path: &Path,
    pg_options: Option<&str>,
) -> Result<(), CtlError> {
    let dump = path.join(POSTGRES_DUMP_FILE);
    if dump.exists() {
        return Err(CtlError::message(format!(
            "backup destination already contains {}",
            dump.display()
        )));
    }

    let dump_arg = dump.to_str().ok_or_else(|| {
        CtlError::message(format!("backup path {} is not valid UTF-8", dump.display()))
    })?;
    let target = PgTarget::parse(&context.database_url)?;
    let extra = parse_pg_options(pg_options, PgTool::Dump)?;
    let mut args: Vec<String> = vec![
        "--format=custom".into(),
        "--no-owner".into(),
        "--no-password".into(),
    ];
    args.extend(extra);
    // Put the managed file/database options last so a user option cannot
    // redirect the dump away from the requested destination.
    args.extend([
        "--file".to_string(),
        dump_arg.to_string(),
        "--dbname".to_string(),
        target.url().to_string(),
    ]);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_tool("pg_dump", &arg_refs, &target)?;

    if !dump.is_file() {
        return Err(CtlError::message(format!(
            "pg_dump reported success but {} was not created",
            dump.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dump, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Restore a backup created by [`backup`]. The live database and bookshelf are
/// replaced.
///
/// A directory and a tar archive are both accepted. The format is detected
/// from the path when it is not specified explicitly; explicit
/// [`BackupFormat::Tar`] requires a file and [`BackupFormat::Directory`]
/// requires a directory.
pub fn restore(
    context: &Context,
    path: &Path,
    yes: bool,
    format: Option<BackupFormat>,
    pg_options: Option<&str>,
) -> Result<(), CtlError> {
    let format = match (format, path.is_dir()) {
        (Some(format), _) => format,
        (None, true) => BackupFormat::Directory,
        (None, false) if path.is_file() => BackupFormat::Tar,
        (None, false) => {
            return Err(CtlError::message(format!(
                "backup {} does not exist or is neither a directory nor a tar archive",
                path.display()
            )));
        }
    };

    if pg_options.is_some() && context.config.database.driver == DatabaseDriver::Sqlite {
        println!("note: --pg-options is ignored for the sqlite backend.");
    }

    match format {
        BackupFormat::Directory => {
            if !path.is_dir() {
                return Err(CtlError::message(format!(
                    "--format directory requires a backup directory, but {} is not a directory",
                    path.display()
                )));
            }
            restore_directory(context, path, yes, pg_options)?;
        }
        BackupFormat::Tar => {
            if !path.is_file() {
                return Err(CtlError::message(format!(
                    "--format tar requires a backup file, but {} is not a file",
                    path.display()
                )));
            }
            let staging = tempfile::Builder::new()
                .prefix("microwave-restore-")
                .tempdir()?;
            extract_tar(path, staging.path())?;
            restore_directory(context, staging.path(), yes, pg_options)?;
        }
    }

    println!("Restored from {}", path.display());
    Ok(())
}

fn restore_directory(
    context: &Context,
    path: &Path,
    yes: bool,
    pg_options: Option<&str>,
) -> Result<(), CtlError> {
    // Validate and copy the backup bookshelf before the database or live
    // bookshelf is touched. A symlink/FIFO (or any read error) in the backup
    // must fail here, while the live installation is still intact.
    let staged_bookshelf = stage_bookshelf(context, &path.join(BOOKSHELF_DIR))?;

    match context.config.database.driver {
        DatabaseDriver::Sqlite => restore_sqlite(context, path, yes)?,
        DatabaseDriver::Postgres => restore_postgres(context, path, yes, pg_options)?,
    }

    swap_bookshelf(context, staged_bookshelf)?;
    Ok(())
}

/// A validated copy of the backup bookshelf, ready to replace the live one.
///
/// The temp directory must stay alive until the rename; dropping it removes the
/// staged tree.
type StagedBookshelf = Option<(tempfile::TempDir, PathBuf)>;

fn stage_bookshelf(context: &Context, source: &Path) -> Result<StagedBookshelf, CtlError> {
    match std::fs::symlink_metadata(source) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(CtlError::message(format!(
                "backup bookshelf {} is a symlink; refusing to restore it",
                source.display()
            )));
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(CtlError::message(format!(
                "backup bookshelf {} is not a directory",
                source.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }

    context.prepare_data_dir()?;
    let staging = tempfile::Builder::new()
        .prefix(".microwave-restore-bookshelf-")
        .tempdir_in(&context.data_dir)?;
    let staged = staging.path().join(BOOKSHELF_DIR);
    copy_dir_all(source, &staged)?;
    Ok(Some((staging, staged)))
}

/// Replace the live bookshelf only after the database restore succeeded.
///
/// The staged tree is renamed into place, keeping the old tree under a
/// sidecar name until the rename succeeds so a failure does not lose it.
fn swap_bookshelf(context: &Context, staged: StagedBookshelf) -> Result<(), CtlError> {
    let bookshelf = context.data_dir.join(BOOKSHELF_DIR);
    let Some((_staging, staged_bookshelf)) = staged else {
        // A backup without a bookshelf intentionally leaves the installation
        // without one, matching the previous restore semantics.
        remove_path(&bookshelf)?;
        return Ok(());
    };

    let had_live = match std::fs::symlink_metadata(&bookshelf) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let old = sidecar(
        &bookshelf,
        &format!(".restore-old-{}", Uuid::new_v4().simple()),
    );
    if had_live {
        std::fs::rename(&bookshelf, &old)?;
    }

    if let Err(error) = std::fs::rename(&staged_bookshelf, &bookshelf) {
        if had_live {
            let _ = std::fs::rename(&old, &bookshelf);
        }
        return Err(error.into());
    }

    remove_path(&old)?;
    Ok(())
}

/// Extract a backup tar archive into a staging directory.
///
/// Only the files this command writes are accepted: the database snapshot or
/// dump and the `bookshelf/` tree. Absolute paths, `..`, symlinks, devices, and
/// every other entry type are rejected, so a malformed or hostile archive
/// cannot write outside the staging directory or plant a link that a later
/// live-data replacement would follow.
fn extract_tar(path: &Path, destination: &Path) -> Result<(), CtlError> {
    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(file);
    let mut saw_payload = false;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.into_owned();
        if !allowed_tar_entry(&entry_path) {
            return Err(CtlError::message(format!(
                "backup archive contains an unexpected entry {}; expected {} or {} or {}/",
                entry_path.display(),
                DATABASE_FILE,
                POSTGRES_DUMP_FILE,
                BOOKSHELF_DIR
            )));
        }

        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(CtlError::message(format!(
                "backup archive entry {} is not a regular file or directory",
                entry_path.display()
            )));
        }

        if !entry.unpack_in(destination)? {
            return Err(CtlError::message(format!(
                "backup archive entry {} escapes the extraction directory",
                entry_path.display()
            )));
        }
        saw_payload = true;
    }

    if !saw_payload {
        return Err(CtlError::message(format!(
            "backup archive {} is empty",
            path.display()
        )));
    }
    Ok(())
}

fn allowed_tar_entry(path: &Path) -> bool {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return false;
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return false;
    }
    path == Path::new(DATABASE_FILE)
        || path == Path::new(POSTGRES_DUMP_FILE)
        || path == Path::new(CONFIG_FILE)
        || path.starts_with(BOOKSHELF_DIR)
}

fn restore_sqlite(context: &Context, path: &Path, yes: bool) -> Result<(), CtlError> {
    let database_path = context.sqlite_path()?;
    let database_backup = path.join(DATABASE_FILE);
    if !database_backup.is_file() {
        return Err(CtlError::message(format!(
            "backup {} does not contain {}",
            path.display(),
            DATABASE_FILE
        )));
    }
    SqliteStore::verify_database(&database_backup)?;

    if same_path(&database_backup, database_path) {
        return Err(CtlError::message(
            "the backup database is the live database; choose a separate backup directory",
        ));
    }

    confirm_restore(
        &format!("sqlite:{}", database_path.display()),
        None,
        None,
        yes,
    )?;

    context.prepare_data_dir()?;
    if let Some(parent) = database_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Stage the snapshot before touching the live files so a failed copy does
    // not leave the database half-replaced.
    let staged = sidecar(
        database_path,
        &format!(".restore-{}", Uuid::new_v4().simple()),
    );
    std::fs::copy(&database_backup, &staged)?;

    remove_file(database_path)?;
    remove_file(&sidecar(database_path, "-wal"))?;
    remove_file(&sidecar(database_path, "-shm"))?;
    std::fs::rename(&staged, database_path)?;

    // Open the restored database so it is migrated to the current schema and
    // any incompatibility is reported before the command succeeds.
    SqliteStore::open(database_path)?;
    Ok(())
}

fn restore_postgres(
    context: &Context,
    path: &Path,
    yes: bool,
    pg_options: Option<&str>,
) -> Result<(), CtlError> {
    let dump = path.join(POSTGRES_DUMP_FILE);
    if !dump.is_file() {
        return Err(CtlError::message(format!(
            "backup {} does not contain {}",
            path.display(),
            POSTGRES_DUMP_FILE
        )));
    }

    let dump_arg = dump.to_str().ok_or_else(|| {
        CtlError::message(format!("backup path {} is not valid UTF-8", dump.display()))
    })?;

    let target = PgTarget::parse(&context.database_url)?;
    // Validate the archive and read its source database name before dropping
    // any live object. The TOC is captured, not printed.
    let toc = pg_restore_list(dump_arg, &target)?;
    let archive_database = archive_database(&toc);
    confirm_restore(
        &format!("PostgreSQL {}", target.display()),
        archive_database,
        target.database(),
        yes,
    )?;
    let extra = parse_pg_options(pg_options, PgTool::Restore)?;
    let mut args: Vec<String> = vec![
        "--clean".into(),
        "--if-exists".into(),
        "--no-owner".into(),
        "--no-privileges".into(),
        "--single-transaction".into(),
        "--no-password".into(),
    ];
    args.extend(extra);
    args.extend([
        "--dbname".to_string(),
        target.url().to_string(),
        dump_arg.to_string(),
    ]);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_tool("pg_restore", &arg_refs, &target)?;
    Ok(())
}

fn pg_restore_list(dump_arg: &str, target: &PgTarget) -> Result<String, CtlError> {
    let mut command = Command::new("pg_restore");
    command.args(["--list", dump_arg]);
    target.apply_environment(&mut command);
    let output = command.output().map_err(|error| {
        CtlError::message(format!(
            "failed to run pg_restore: {error}; install the PostgreSQL client tools or use a separate backup workflow"
        ))
    })?;
    if !output.status.success() {
        return Err(CtlError::message(format!(
            "pg_restore --list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn archive_database(toc: &str) -> Option<&str> {
    toc.lines().find_map(|line| {
        line.trim_start_matches(';')
            .trim()
            .strip_prefix("dbname:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

fn confirm_restore(
    target: &str,
    archive_database: Option<&str>,
    target_database: Option<&str>,
    yes: bool,
) -> Result<(), CtlError> {
    println!("Restore target: {target}");
    if let Some(archive_database) = archive_database {
        let archive_database = normalize_database_name(archive_database);
        println!("Backup database: {archive_database}");
        if let Some(target_database) = target_database {
            let target_database = normalize_database_name(target_database);
            if !database_names_match(&archive_database, &target_database) {
                if !yes {
                    return Err(CtlError::message(format!(
                        "backup was created from database {archive_database:?} but the target is {target_database:?}; re-run with --yes only after verifying the target"
                    )));
                }
                eprintln!(
                    "warning: backup database {archive_database:?} differs from target {target_database:?}; continuing because --yes was given"
                );
            }
        } else {
            println!("Target database: client default (archive source is {archive_database})");
        }
    }
    confirm_destructive("restore", yes)
}

fn database_names_match(archive_database: &str, target_database: &str) -> bool {
    normalize_database_name(archive_database) == normalize_database_name(target_database)
}

/// Require an explicit confirmation for an operation that replaces or merges
/// into live metadata.
///
/// The caller prints a credential-free target first. In non-interactive
/// environments this fails unless `yes` (the `--yes` flag) is set.
pub(crate) fn confirm_destructive(action: &str, yes: bool) -> Result<(), CtlError> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(CtlError::message(format!(
            "{action} is destructive or merges into live metadata and stdin is not a terminal; re-run with --yes after verifying the target"
        )));
    }
    print!("Type 'yes' to continue: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if answer.trim() == "yes" {
        Ok(())
    } else {
        Err(CtlError::message(format!("{action} cancelled")))
    }
}

fn run_tool(program: &str, args: &[&str], target: &PgTarget) -> Result<(), CtlError> {
    let mut command = Command::new(program);
    command.args(args);
    target.apply_environment(&mut command);
    let status = command.status().map_err(|error| {
        CtlError::message(format!(
            "failed to run {program}: {error}; install the PostgreSQL client tools or use a separate backup workflow"
        ))
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CtlError::message(format!("{program} exited with {status}")))
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn remove_file(path: &Path) -> Result<(), CtlError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_path(path: &Path) -> Result<(), CtlError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(path)?;
            Ok(())
        }
        Ok(_) => remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn copy_dir_all(source: &Path, destination: &Path) -> Result<(), CtlError> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        // `DirEntry::file_type` does not follow symlinks. Reject everything
        // that is not a regular file or directory so a link cannot leak a file
        // from outside the bookshelf and a FIFO/device cannot block or hang the
        // copy.
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_symlink() {
            return Err(CtlError::message(format!(
                "refusing to copy symlink {}; backup and restore only handle regular files and directories",
                path.display()
            )));
        }
        if file_type.is_dir() {
            copy_dir_all(&path, &target)?;
        } else if file_type.is_file() {
            copy_regular_file(&path, &target)?;
        } else {
            return Err(CtlError::message(format!(
                "refusing to copy {}; it is not a regular file or directory",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Copy one regular file without following a symlink that may have replaced it
/// after the directory entry type check.
///
/// `O_NOFOLLOW` rejects a swapped-in symlink and `O_NONBLOCK` keeps a swapped-in
/// FIFO from blocking before the metadata check rejects it. Directories still
/// have a narrow check-then-traverse window because this iterates with relative
/// paths rather than `openat`; bookshelf trees are expected to be quiescent
/// while a backup or restore runs.
#[cfg(unix)]
fn copy_regular_file(source: &Path, destination: &Path) -> Result<(), CtlError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut input = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)?;
    let metadata = input.metadata()?;
    if !metadata.is_file() {
        return Err(CtlError::message(format!(
            "refusing to copy {}; it is not a regular file",
            source.display()
        )));
    }

    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    std::fs::set_permissions(
        destination,
        std::fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
    )?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_regular_file(source: &Path, destination: &Path) -> Result<(), CtlError> {
    std::fs::copy(source, destination)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PgTool {
    Dump,
    Restore,
}

impl PgTool {
    fn program(self) -> &'static str {
        match self {
            Self::Dump => "pg_dump",
            Self::Restore => "pg_restore",
        }
    }

    /// Options this CLI already controls or that only apply to the other tool.
    fn controlled_options(self) -> &'static [&'static str] {
        match self {
            Self::Dump => &[
                "-f",
                "--file",
                "-F",
                "--format",
                "-d",
                "--dbname",
                "-h",
                "--host",
                "-p",
                "--port",
                "-U",
                "--username",
                "-W",
                "--password",
                "--no-password",
                "-a",
                "--data-only",
                "-s",
                "--schema-only",
                "-C",
                "--create",
                "-c",
                "--clean",
                "-1",
                "--single-transaction",
                "-l",
                "--list",
            ],
            Self::Restore => &[
                "-f",
                "--file",
                "-F",
                "--format",
                "-d",
                "--dbname",
                "-h",
                "--host",
                "-p",
                "--port",
                "-U",
                "--username",
                "-W",
                "--password",
                "--no-password",
                "-c",
                "--clean",
                "--if-exists",
                "-1",
                "--single-transaction",
                "-l",
                "--list",
                "-L",
                "--use-list",
                "-e",
                "--exit-on-error",
                "--no-owner",
                "--no-privileges",
            ],
        }
    }
}

/// Split and validate a `--pg-options` value.
///
/// Values are tokenized on whitespace and passed directly to the child
/// process; there is no shell. Options that this CLI sets itself (or that
/// belong only to the other PostgreSQL tool) are rejected with a clear error
/// rather than producing a confusing failure halfway through a backup.
fn parse_pg_options(value: Option<&str>, tool: PgTool) -> Result<Vec<String>, CtlError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    let mut parsed = Vec::new();
    for token in value.split_whitespace() {
        if token == "--" {
            return Err(CtlError::message(
                "--pg-options must not contain the `--` option terminator".to_string(),
            ));
        }
        if !token.starts_with('-') {
            return Err(CtlError::message(format!(
                "--pg-options value {token:?} is not an option; pass flags such as `--no-sync` or `-Z9`"
            )));
        }
        let name = token.split_once('=').map(|(name, _)| name).unwrap_or(token);
        let controlled = tool
            .controlled_options()
            .iter()
            .any(|option| *option == name || short_option_with_value(token, option));
        if controlled {
            return Err(CtlError::message(format!(
                "--pg-options cannot include {token:?}; {} controls that option",
                tool.program()
            )));
        }
        parsed.push(token.to_string());
    }
    Ok(parsed)
}

/// Match an attached short-option value such as `-fout.dump`.
fn short_option_with_value(token: &str, option: &str) -> bool {
    option.len() == 2
        && option.starts_with('-')
        && !option.starts_with("--")
        && token.starts_with(option)
        && token.len() > option.len()
        && !token.starts_with("--")
}

#[cfg(test)]
mod tests {
    use super::{
        allowed_tar_entry, archive_database, database_names_match, default_backup_path,
        parse_pg_options, BackupFormat, PgTool,
    };
    use std::path::Path;

    #[test]
    fn reads_the_database_name_from_a_pg_restore_toc() {
        let toc = ";     dbname: microwave%2Dprod\n;     TOC Entries: 12\n";
        assert_eq!(archive_database(toc), Some("microwave%2Dprod"));
    }

    #[test]
    fn compares_normalized_database_names() {
        assert!(database_names_match("microwave%2Dprod", "microwave-prod"));
        assert!(database_names_match("/microwave", "microwave"));
        assert!(!database_names_match("microwave", "other"));
    }

    #[test]
    fn only_accepts_the_expected_backup_layout_in_a_tar_archive() {
        assert!(allowed_tar_entry(Path::new("microwave.db")));
        assert!(allowed_tar_entry(Path::new("microwave.dump")));
        assert!(allowed_tar_entry(Path::new("microwave-config.json")));
        assert!(allowed_tar_entry(Path::new("bookshelf")));
        assert!(allowed_tar_entry(Path::new(
            "bookshelf/organization-abc/checksum-def"
        )));

        assert!(!allowed_tar_entry(Path::new("../microwave.db")));
        assert!(!allowed_tar_entry(Path::new(
            "bookshelf/../../microwave.db"
        )));
        assert!(!allowed_tar_entry(Path::new("/etc/passwd")));
        assert!(!allowed_tar_entry(Path::new("pivotal.pem")));
        assert!(!allowed_tar_entry(Path::new("")));
    }

    #[test]
    fn default_backup_paths_are_timestamped() {
        let directory = default_backup_path(BackupFormat::Directory);
        let name = directory.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("microwave-backup-"),
            "unexpected default: {name}"
        );
        assert!(directory.extension().is_none());

        let archive = default_backup_path(BackupFormat::Tar);
        let name = archive.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("microwave-backup-"));
        assert_eq!(
            archive.extension().and_then(|value| value.to_str()),
            Some("tar")
        );
    }

    #[test]
    fn accepts_supported_pg_options_and_rejects_managed_ones() {
        assert_eq!(
            parse_pg_options(Some("--no-sync -Z9"), PgTool::Dump).unwrap(),
            vec!["--no-sync".to_string(), "-Z9".to_string()]
        );
        assert!(parse_pg_options(Some("--file=/tmp/other.dump"), PgTool::Dump).is_err());
        assert!(parse_pg_options(Some("-f/tmp/other.dump"), PgTool::Dump).is_err());
        assert!(parse_pg_options(Some("--clean"), PgTool::Dump).is_err());
        assert!(parse_pg_options(Some("value"), PgTool::Dump).is_err());
        assert!(parse_pg_options(Some("--format=plain"), PgTool::Restore).is_err());
        assert!(parse_pg_options(Some("--no-data"), PgTool::Restore).is_ok());
        assert_eq!(
            parse_pg_options(None, PgTool::Dump).unwrap(),
            Vec::<String>::new()
        );
    }
}
