//! Tar/gzip migration archives: a snapshot plus local bookshelf blobs.
//!
//! The JSON snapshot remains the metadata contract. An archive is a transport
//! envelope with exactly two members:
//!
//! * `snapshot.json` — the same versioned document written by the JSON export.
//! * `bookshelf/` — the local object-store tree (`organization-*/checksum-*`).
//!
//! Import extracts into a private staging directory before touching the live
//! database or bookshelf. Entry names are restricted to that allowlist, and
//! absolute paths, `..`, symlinks, hard links, and special files are rejected.
//!
//! Archives only carry a *local* bookshelf directory. Microwave does not fetch
//! S3/Garage objects here: use bucket replication or `rclone`/`aws s3 sync`
//! for a non-local object store.

use crate::{
    export_postgres_with_options, export_with_options, import_database_with_compression,
    import_postgres_with_compression, read_snapshot, Compression, MigrateError, DATABASE_FILE,
};
use flate2::write::GzEncoder;
use flate2::Compression as GzipLevel;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tar::{Builder, Header};

/// Snapshot member name inside an archive.
pub const ARCHIVE_SNAPSHOT_ENTRY: &str = "snapshot.json";
/// Bookshelf member prefix inside an archive.
pub const ARCHIVE_BOOKSHELF_PREFIX: &str = "bookshelf";

const MAX_ARCHIVE_ENTRY_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Export a local SQLite data directory and its `bookshelf/` tree with
/// secrets redacted.
pub fn export_archive(data_dir: &Path, output: &Path) -> Result<(), MigrateError> {
    export_archive_with_options(data_dir, output, false)
}

/// Export a local SQLite data directory and its `bookshelf/` tree.
pub fn export_archive_with_options(
    data_dir: &Path,
    output: &Path,
    include_secrets: bool,
) -> Result<(), MigrateError> {
    let scratch = tempfile::tempdir().map_err(|error| {
        MigrateError::Archive(format!(
            "failed to create snapshot staging directory: {error}"
        ))
    })?;
    let snapshot = scratch.path().join(ARCHIVE_SNAPSHOT_ENTRY);
    export_with_options(data_dir, &snapshot, include_secrets)?;
    write_archive(
        output,
        &snapshot,
        Some(&data_dir.join(ARCHIVE_BOOKSHELF_PREFIX)),
    )
}

/// Export a PostgreSQL metadata store plus a local bookshelf directory with
/// secrets redacted.
pub fn export_archive_postgres(
    database_url: &str,
    bookshelf_dir: &Path,
    output: &Path,
) -> Result<(), MigrateError> {
    export_archive_postgres_with_options(database_url, bookshelf_dir, output, false)
}

/// Export a PostgreSQL metadata store plus a local bookshelf directory.
///
/// `bookshelf_dir` must point at the local object-store root (normally
/// `<data_dir>/bookshelf`). For S3- or Garage-backed deployments there is no
/// local tree to copy; replicate the bucket instead.
pub fn export_archive_postgres_with_options(
    database_url: &str,
    bookshelf_dir: &Path,
    output: &Path,
    include_secrets: bool,
) -> Result<(), MigrateError> {
    if !bookshelf_dir.is_dir() {
        return Err(MigrateError::Archive(format!(
            "bookshelf directory {} does not exist; archive export only copies a local bookshelf. \
             For S3/Garage, replicate the bucket with your object-store tooling instead",
            bookshelf_dir.display()
        )));
    }
    let scratch = tempfile::tempdir().map_err(|error| {
        MigrateError::Archive(format!(
            "failed to create snapshot staging directory: {error}"
        ))
    })?;
    let snapshot = scratch.path().join(ARCHIVE_SNAPSHOT_ENTRY);
    export_postgres_with_options(database_url, &snapshot, include_secrets)?;
    write_archive(output, &snapshot, Some(bookshelf_dir))
}

/// Import an archive into a local SQLite data directory using the default
/// serialized-object compression.
///
/// The archive is fully validated and extracted to staging first. The database
/// import then runs in one transaction, and the staged bookshelf replaces the
/// target `bookshelf/` only after that succeeds.
pub fn import_archive(data_dir: &Path, input: &Path) -> Result<(), MigrateError> {
    import_archive_with_compression(data_dir, input, Compression::default())
}

/// Import an archive into a local SQLite data directory.
pub fn import_archive_with_compression(
    data_dir: &Path,
    input: &Path,
    compression: Compression,
) -> Result<(), MigrateError> {
    fs::create_dir_all(data_dir).map_err(|source| MigrateError::Write {
        path: data_dir.to_path_buf(),
        source,
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".microwave-archive-import-")
        .tempdir_in(data_dir)
        .map_err(|error| {
            MigrateError::Archive(format!(
                "failed to create archive staging directory: {error}"
            ))
        })?;
    let extracted = extract_archive(input, staging.path())?;
    import_database_with_compression(
        &data_dir.join(DATABASE_FILE),
        &extracted.snapshot_path,
        compression,
    )?;
    if let Some(bookshelf) = extracted.bookshelf {
        replace_bookshelf(&bookshelf, &data_dir.join(ARCHIVE_BOOKSHELF_PREFIX))?;
    }
    Ok(())
}

/// Import an archive into PostgreSQL and restore its bookshelf tree using the
/// default serialized-object compression.
///
/// `bookshelf_dir` is the local object-store root to replace. A PostgreSQL
/// metadata export created from an S3/Garage deployment has no local blobs;
/// use bucket replication for those deployments.
pub fn import_archive_postgres(
    database_url: &str,
    bookshelf_dir: &Path,
    input: &Path,
) -> Result<(), MigrateError> {
    import_archive_postgres_with_compression(
        database_url,
        bookshelf_dir,
        input,
        Compression::default(),
    )
}

/// Import an archive into PostgreSQL and restore its bookshelf tree.
pub fn import_archive_postgres_with_compression(
    database_url: &str,
    bookshelf_dir: &Path,
    input: &Path,
    compression: Compression,
) -> Result<(), MigrateError> {
    let parent = bookshelf_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent).map_err(|source| MigrateError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let staging_root = parent.unwrap_or_else(|| Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".microwave-archive-import-")
        .tempdir_in(staging_root)
        .map_err(|error| {
            MigrateError::Archive(format!(
                "failed to create archive staging directory: {error}"
            ))
        })?;
    let extracted = extract_archive(input, staging.path())?;
    import_postgres_with_compression(database_url, &extracted.snapshot_path, compression)?;
    if let Some(bookshelf) = extracted.bookshelf {
        replace_bookshelf(&bookshelf, bookshelf_dir)?;
    }
    Ok(())
}

struct ExtractedArchive {
    snapshot_path: PathBuf,
    bookshelf: Option<PathBuf>,
}

fn extract_archive(input: &Path, staging: &Path) -> Result<ExtractedArchive, MigrateError> {
    let file = File::open(input).map_err(|source| MigrateError::Read {
        path: input.to_path_buf(),
        source,
    })?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let snapshot_path = staging.join(ARCHIVE_SNAPSHOT_ENTRY);
    let bookshelf_path = staging.join(ARCHIVE_BOOKSHELF_PREFIX);
    let mut seen_snapshot = false;
    let mut seen_bookshelf = false;
    let mut total_bytes = 0_u64;

    let entries = archive.entries().map_err(archive_error)?;
    for entry in entries {
        let mut entry = entry.map_err(archive_error)?;
        let raw_path = entry.path().map_err(archive_error)?.into_owned();
        let path = safe_archive_path(&raw_path).ok_or_else(|| {
            MigrateError::Archive(format!(
                "rejected archive entry {:?}: absolute paths, '..', and empty names are not allowed",
                raw_path.display()
            ))
        })?;
        let entry_type = entry.header().entry_type();
        if !(entry_type.is_file() || entry_type.is_dir()) {
            return Err(MigrateError::Archive(format!(
                "rejected archive entry {:?}: only regular files and directories are allowed \
                 (symlinks, hard links, and special files are not)",
                path.display()
            )));
        }

        let is_snapshot = path == Path::new(ARCHIVE_SNAPSHOT_ENTRY);
        let is_bookshelf_root = path == Path::new(ARCHIVE_BOOKSHELF_PREFIX);
        let is_bookshelf_member = path.starts_with(ARCHIVE_BOOKSHELF_PREFIX) && !is_bookshelf_root;
        if !is_snapshot && !is_bookshelf_root && !is_bookshelf_member {
            return Err(MigrateError::Archive(format!(
                "rejected archive entry {:?}: only {ARCHIVE_SNAPSHOT_ENTRY} and \
                 {ARCHIVE_BOOKSHELF_PREFIX}/ are allowed",
                path.display()
            )));
        }

        if is_snapshot {
            if seen_snapshot || entry_type.is_dir() {
                return Err(MigrateError::Archive(
                    "archive contains more than one snapshot.json or a directory named snapshot.json"
                        .into(),
                ));
            }
            seen_snapshot = true;
            let size = entry.header().size().map_err(archive_error)?;
            check_entry_size(size, &mut total_bytes, &path)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&snapshot_path)
                .map_err(|source| MigrateError::Write {
                    path: snapshot_path.clone(),
                    source,
                })?;
            std::io::copy(&mut entry, &mut output).map_err(|source| MigrateError::Write {
                path: snapshot_path.clone(),
                source,
            })?;
            output.flush().map_err(|source| MigrateError::Write {
                path: snapshot_path.clone(),
                source,
            })?;
            continue;
        }

        if is_bookshelf_root {
            if !entry_type.is_dir() {
                return Err(MigrateError::Archive(
                    "archive entry bookshelf must be a directory".into(),
                ));
            }
            seen_bookshelf = true;
            fs::create_dir_all(&bookshelf_path).map_err(|source| MigrateError::Write {
                path: bookshelf_path.clone(),
                source,
            })?;
            continue;
        }

        let relative = path
            .strip_prefix(ARCHIVE_BOOKSHELF_PREFIX)
            .expect("bookshelf member was checked above");
        let destination = bookshelf_path.join(relative);
        if entry_type.is_dir() {
            seen_bookshelf = true;
            fs::create_dir_all(&destination).map_err(|source| MigrateError::Write {
                path: destination,
                source,
            })?;
        } else {
            seen_bookshelf = true;
            let size = entry.header().size().map_err(archive_error)?;
            check_entry_size(size, &mut total_bytes, &path)?;
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|source| MigrateError::Write {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|source| MigrateError::Write {
                    path: destination.clone(),
                    source,
                })?;
            std::io::copy(&mut entry, &mut output).map_err(|source| MigrateError::Write {
                path: destination.clone(),
                source,
            })?;
            output.flush().map_err(|source| MigrateError::Write {
                path: destination,
                source,
            })?;
        }
    }

    if !seen_snapshot || !snapshot_path.is_file() {
        return Err(MigrateError::Archive(format!(
            "archive does not contain a {ARCHIVE_SNAPSHOT_ENTRY} file"
        )));
    }

    // Parse and validate before the caller touches the database.
    read_snapshot(&snapshot_path)?;
    Ok(ExtractedArchive {
        snapshot_path,
        bookshelf: seen_bookshelf.then_some(bookshelf_path),
    })
}

fn check_entry_size(size: u64, total: &mut u64, path: &Path) -> Result<(), MigrateError> {
    if size > MAX_ARCHIVE_ENTRY_BYTES {
        return Err(MigrateError::Archive(format!(
            "archive entry {:?} is too large ({size} bytes)",
            path.display()
        )));
    }
    *total = total.saturating_add(size);
    if *total > MAX_ARCHIVE_ENTRY_BYTES {
        return Err(MigrateError::Archive(format!(
            "archive expands beyond the {MAX_ARCHIVE_ENTRY_BYTES}-byte safety limit"
        )));
    }
    Ok(())
}

/// Normalize an archive entry path and reject anything that could escape the
/// staging directory or rely on platform-specific separators.
fn safe_archive_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str()?;
                if value.contains('\\') || value.contains('\0') {
                    return None;
                }
                normalized.push(value);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if normalized.as_os_str().is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn write_archive(
    output: &Path,
    snapshot: &Path,
    bookshelf: Option<&Path>,
) -> Result<(), MigrateError> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| MigrateError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let temporary = crate::temporary_path(output);
    let result = (|| -> Result<(), MigrateError> {
        let file = File::create(&temporary).map_err(|source| MigrateError::Write {
            path: temporary.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).map_err(
                |source| MigrateError::Write {
                    path: temporary.clone(),
                    source,
                },
            )?;
        }
        let encoder = GzEncoder::new(BufWriter::new(file), GzipLevel::default());
        let mut builder = Builder::new(encoder);
        builder.follow_symlinks(false);

        let snapshot_bytes = fs::read(snapshot).map_err(|source| MigrateError::Read {
            path: snapshot.to_path_buf(),
            source,
        })?;
        append_bytes(
            &mut builder,
            Path::new(ARCHIVE_SNAPSHOT_ENTRY),
            &snapshot_bytes,
            0o600,
        )?;

        if let Some(root) = bookshelf {
            append_bookshelf(&mut builder, root)?;
        } else {
            append_directory(&mut builder, Path::new(ARCHIVE_BOOKSHELF_PREFIX))?;
        }

        let encoder = builder.into_inner().map_err(archive_error)?;
        let mut writer = encoder.finish().map_err(archive_error)?;
        writer.flush().map_err(|source| MigrateError::Write {
            path: temporary.clone(),
            source,
        })?;
        fs::rename(&temporary, output).map_err(|source| MigrateError::Write {
            path: output.to_path_buf(),
            source,
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn append_bookshelf<W: Write>(builder: &mut Builder<W>, root: &Path) -> Result<(), MigrateError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(MigrateError::Archive(format!(
                "bookshelf source {} is not a directory",
                root.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            append_directory(builder, Path::new(ARCHIVE_BOOKSHELF_PREFIX))?;
            return Ok(());
        }
        Err(error) => {
            return Err(MigrateError::Read {
                path: root.to_path_buf(),
                source: error,
            })
        }
    }
    append_directory(builder, Path::new(ARCHIVE_BOOKSHELF_PREFIX))?;
    append_directory_contents(builder, root, Path::new(ARCHIVE_BOOKSHELF_PREFIX))
}

fn append_directory_contents<W: Write>(
    builder: &mut Builder<W>,
    root: &Path,
    prefix: &Path,
) -> Result<(), MigrateError> {
    let mut entries = fs::read_dir(root)
        .map_err(|source| MigrateError::Read {
            path: root.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| MigrateError::Read {
            path: root.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            MigrateError::Archive(format!(
                "bookshelf path {:?} is not valid UTF-8; archive entries must be UTF-8",
                entry.path()
            ))
        })?;
        if name.contains('\\') {
            return Err(MigrateError::Archive(format!(
                "bookshelf entry {name:?} contains a backslash; archive paths must use '/'"
            )));
        }
        let path = entry.path();
        let relative = prefix.join(name);
        let metadata = fs::symlink_metadata(&path).map_err(|source| MigrateError::Read {
            path: path.clone(),
            source,
        })?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return Err(MigrateError::Archive(format!(
                "refusing to archive symlink {}; bookshelf trees must contain only regular \
                 files and directories",
                path.display()
            )));
        }
        if file_type.is_dir() {
            append_directory(builder, &relative)?;
            append_directory_contents(builder, &path, &relative)?;
        } else if file_type.is_file() {
            let data = fs::read(&path).map_err(|source| MigrateError::Read {
                path: path.clone(),
                source,
            })?;
            append_bytes(builder, &relative, &data, 0o644)?;
        } else {
            return Err(MigrateError::Archive(format!(
                "refusing to archive non-regular file {}; bookshelf trees must contain only \
                 regular files and directories",
                path.display()
            )));
        }
    }
    Ok(())
}

fn append_directory<W: Write>(builder: &mut Builder<W>, path: &Path) -> Result<(), MigrateError> {
    let mut header = Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(0o755);
    header.set_mtime(now_seconds());
    header.set_cksum();
    builder
        .append_data(&mut header, path, std::io::empty())
        .map_err(archive_error)
}

fn append_bytes<W: Write>(
    builder: &mut Builder<W>,
    path: &Path,
    data: &[u8],
    mode: u32,
) -> Result<(), MigrateError> {
    let mut header = Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(data.len() as u64);
    header.set_mode(mode);
    header.set_mtime(now_seconds());
    header.set_cksum();
    builder
        .append_data(&mut header, path, data)
        .map_err(archive_error)
}

fn replace_bookshelf(staged: &Path, target: &Path) -> Result<(), MigrateError> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| MigrateError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let backup = target.with_file_name(format!(
        ".bookshelf.replaced-{}-{}",
        std::process::id(),
        now_nanos()
    ));
    let had_target = fs::symlink_metadata(target).is_ok();
    if had_target {
        fs::rename(target, &backup).map_err(|source| MigrateError::Write {
            path: target.to_path_buf(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(staged, target) {
        if had_target {
            let _ = fs::rename(&backup, target);
        }
        return Err(MigrateError::Write {
            path: target.to_path_buf(),
            source,
        });
    }
    if had_target {
        if backup.is_dir() {
            let _ = fs::remove_dir_all(&backup);
        } else {
            let _ = fs::remove_file(&backup);
        }
    }
    Ok(())
}

fn archive_error(error: impl std::fmt::Display) -> MigrateError {
    MigrateError::Archive(error.to_string())
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}
