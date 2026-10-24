use crate::{context::Context, CtlError};
use microwave_store::SqliteStore;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const DATABASE_FILE: &str = "microwave.db";
const BOOKSHELF_DIR: &str = "bookshelf";

/// Create a backup directory containing a consistent SQLite snapshot and a
/// copy of the local bookshelf, when one exists.
pub fn backup(context: &Context, path: &Path) -> Result<(), CtlError> {
    if path.exists() && !path.is_dir() {
        return Err(CtlError::message(format!(
            "backup destination {} is not a directory",
            path.display()
        )));
    }
    std::fs::create_dir_all(path)?;

    let database_backup = path.join(DATABASE_FILE);
    if database_backup.exists() {
        return Err(CtlError::message(format!(
            "backup destination already contains {}",
            database_backup.display()
        )));
    }

    let store = context.open_store()?;
    store.backup_to(&database_backup)?;

    let bookshelf_backup = path.join(BOOKSHELF_DIR);
    remove_path(&bookshelf_backup)?;
    let bookshelf = context.data_dir.join(BOOKSHELF_DIR);
    if bookshelf.is_dir() {
        copy_dir_all(&bookshelf, &bookshelf_backup)?;
    }

    println!("Backup written to {}", path.display());
    Ok(())
}

/// Restore a backup directory created by [`backup`]. The live database and
/// bookshelf are replaced.
pub fn restore(context: &Context, path: &Path) -> Result<(), CtlError> {
    let database_backup = path.join(DATABASE_FILE);
    if !database_backup.is_file() {
        return Err(CtlError::message(format!(
            "backup {} does not contain {}",
            path.display(),
            DATABASE_FILE
        )));
    }
    SqliteStore::verify_database(&database_backup)?;

    if same_path(&database_backup, &context.database_path) {
        return Err(CtlError::message(
            "the backup database is the live database; choose a separate backup directory",
        ));
    }

    context.prepare_data_dir()?;
    if let Some(parent) = context.database_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Stage the snapshot before touching the live files so a failed copy does
    // not leave the database half-replaced.
    let staged = sidecar(
        &context.database_path,
        &format!(".restore-{}", Uuid::new_v4().simple()),
    );
    std::fs::copy(&database_backup, &staged)?;

    remove_file(&context.database_path)?;
    remove_file(&sidecar(&context.database_path, "-wal"))?;
    remove_file(&sidecar(&context.database_path, "-shm"))?;
    std::fs::rename(&staged, &context.database_path)?;

    let bookshelf = context.data_dir.join(BOOKSHELF_DIR);
    remove_path(&bookshelf)?;
    let bookshelf_backup = path.join(BOOKSHELF_DIR);
    if bookshelf_backup.is_dir() {
        copy_dir_all(&bookshelf_backup, &bookshelf)?;
    }

    // Open the restored database so it is migrated to the current schema and
    // any incompatibility is reported before the command succeeds.
    SqliteStore::open(&context.database_path)?;

    println!("Restored from {}", path.display());
    Ok(())
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
