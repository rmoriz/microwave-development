use crate::pg::{normalize_database_name, PgTarget};
use crate::{context::Context, CtlError};
use microwave_config::DatabaseDriver;
use microwave_store::SqliteStore;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const DATABASE_FILE: &str = "microwave.db";
const POSTGRES_DUMP_FILE: &str = "microwave.dump";
const BOOKSHELF_DIR: &str = "bookshelf";

/// Create a backup directory containing a consistent database dump and a copy
/// of the local bookshelf, when one exists.
///
/// SQLite uses `VACUUM INTO`; PostgreSQL uses a custom-format `pg_dump`
/// archive. Both include committed WAL/transaction contents and are safe to
/// take while a server is running.
pub fn backup(context: &Context, path: &Path) -> Result<(), CtlError> {
    if path.exists() && !path.is_dir() {
        return Err(CtlError::message(format!(
            "backup destination {} is not a directory",
            path.display()
        )));
    }
    std::fs::create_dir_all(path)?;

    match context.config.database.driver {
        DatabaseDriver::Sqlite => backup_sqlite(context, path)?,
        DatabaseDriver::Postgres => backup_postgres(context, path)?,
    }

    let bookshelf_backup = path.join(BOOKSHELF_DIR);
    remove_path(&bookshelf_backup)?;
    let bookshelf = context.data_dir.join(BOOKSHELF_DIR);
    if bookshelf.is_dir() {
        copy_dir_all(&bookshelf, &bookshelf_backup)?;
    }

    println!("Backup written to {}", path.display());
    println!(
        "note: the bookshelf copy is not atomic with the database dump; drain writes for a strict backup."
    );
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

fn backup_postgres(context: &Context, path: &Path) -> Result<(), CtlError> {
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
    run_tool(
        "pg_dump",
        &[
            "--format=custom",
            "--no-owner",
            "--no-password",
            "--file",
            dump_arg,
            "--dbname",
            target.url(),
        ],
        &target,
    )?;

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

/// Restore a backup directory created by [`backup`]. The live database and
/// bookshelf are replaced.
pub fn restore(context: &Context, path: &Path, yes: bool) -> Result<(), CtlError> {
    match context.config.database.driver {
        DatabaseDriver::Sqlite => restore_sqlite(context, path, yes)?,
        DatabaseDriver::Postgres => restore_postgres(context, path, yes)?,
    }

    let bookshelf = context.data_dir.join(BOOKSHELF_DIR);
    remove_path(&bookshelf)?;
    let bookshelf_backup = path.join(BOOKSHELF_DIR);
    if bookshelf_backup.is_dir() {
        copy_dir_all(&bookshelf_backup, &bookshelf)?;
    }

    println!("Restored from {}", path.display());
    Ok(())
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

fn restore_postgres(context: &Context, path: &Path, yes: bool) -> Result<(), CtlError> {
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
    run_tool(
        "pg_restore",
        &[
            "--clean",
            "--if-exists",
            "--no-owner",
            "--no-privileges",
            "--single-transaction",
            "--no-password",
            "--dbname",
            target.url(),
            dump_arg,
        ],
        &target,
    )?;
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
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{archive_database, database_names_match};

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
}
