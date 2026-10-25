//! Tar/gzip migration archives: a snapshot plus local bookshelf blobs.
//!
//! The JSON snapshot remains the metadata contract. An archive is a transport
//! envelope with these members:
//!
//! * `snapshot.json` — the same versioned document written by the JSON export.
//! * `bookshelf/` — the local object-store tree (`organization-*/checksum-*`).
//! * `secrets/manifest.json` + `secrets/files/*` — optional, explicit
//!   private-key and certificate material collected only when the caller
//!   supplies [`PrivateKeyExportOptions`].
//!
//! Import extracts into a private staging directory before touching the live
//! database, bookshelf, or private files. Entry names are restricted to that
//! allowlist, and absolute paths, `..`, symlinks, hard links, and special
//! files are rejected. Archives are capped at one million entries.
//! Private destinations, parent directories, existing-file conflicts, and
//! empty-over-populated bookshelf replacements are checked, and private files
//! are staged, before the database transaction starts. Extracted private files
//! are always written mode `0600`; parent directories created by the importer
//! are mode `0700`.
//!
//! A live bookshelf path that is a symlink is moved aside as a symlink entry;
//! its target is never followed or modified.
//!
//! Archives only carry a *local* bookshelf directory. Microwave does not fetch
//! S3/Garage objects here: use bucket replication or `rclone`/`aws s3 sync`
//! for a non-local object store.

use crate::{
    export_postgres_with_options, export_with_options, import_database_with_compression,
    import_postgres_with_compression, read_snapshot, Compression, MigrateError, Snapshot,
    DATABASE_FILE,
};
use flate2::write::GzEncoder;
use flate2::Compression as GzipLevel;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tar::{Builder, Header};

/// Snapshot member name inside an archive.
pub const ARCHIVE_SNAPSHOT_ENTRY: &str = "snapshot.json";
/// Bookshelf member prefix inside an archive.
pub const ARCHIVE_BOOKSHELF_PREFIX: &str = "bookshelf";
/// Private material directory inside an archive.
pub const ARCHIVE_SECRETS_PREFIX: &str = "secrets";
/// Private material manifest inside an archive.
pub const ARCHIVE_SECRETS_MANIFEST: &str = "secrets/manifest.json";
/// Prefix for private material payload files.
pub const ARCHIVE_SECRETS_FILES_PREFIX: &str = "secrets/files";

const SECRETS_MANIFEST_FORMAT: &str = "microwave-migrate-private-files";
const SECRETS_MANIFEST_VERSION: u32 = 1;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 1_000_000;

/// Explicit data-directory filenames that may contain private material.
///
/// This is deliberately a fixed manifest, not a glob. Unknown files are never
/// read or archived.
const DATA_DIR_PRIVATE_FILES: &[&str] = &[
    "pivotal.pem",
    "pivotal.password",
    "objectstore.secret",
    "private-chef-secrets.json",
    "private_chef_secrets.json",
    "webui_priv.pem",
    "webui_pub.pem",
    "oc_id_priv.pem",
    "oc_id_pub.pem",
    "ocid_priv.pem",
    "ocid_pub.pem",
    "oc-id.pem",
    "oc_id.pem",
];

/// Export options for private key and certificate material.
///
/// The fields are explicit paths supplied by the caller. No directory glob is
/// performed, and an absent path is simply skipped.
#[derive(Debug, Clone, Default)]
pub struct PrivateKeyExportOptions {
    /// Data directory that contains the well-known `pivotal.*`,
    /// `objectstore.secret`, WebUI/oc-id, and `private-chef-secrets.json`
    /// files. Defaults to the archive's `data_dir` argument.
    pub data_dir: Option<PathBuf>,
    /// Configured `server.tls.cert_path` (may be a PEM chain bundle).
    pub tls_cert_path: Option<PathBuf>,
    /// Configured `server.tls.key_path`.
    pub tls_key_path: Option<PathBuf>,
}

/// Import options for private key and certificate material.
#[derive(Debug, Clone, Default)]
pub struct PrivateKeyImportOptions {
    /// Overwrite existing destination files. Without this, any existing key
    /// file aborts the import before the database is touched.
    pub force: bool,
    /// Restore every private file under this directory instead of resolving
    /// the configured data-dir/TLS destinations.
    pub secrets_dir: Option<PathBuf>,
    /// Target data directory for data-dir-relative files.
    pub data_dir: Option<PathBuf>,
    /// Target `server.tls.cert_path`.
    pub tls_cert_path: Option<PathBuf>,
    /// Target `server.tls.key_path`.
    pub tls_key_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SecretsManifest {
    format: String,
    version: u32,
    entries: Vec<SecretsManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SecretsManifestEntry {
    kind: PrivateFileKind,
    archive_path: String,
    /// Source path as configured or as constructed under the source data dir.
    original_path: String,
    /// Safe path relative to the source data dir, for data-dir files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relative_path: Option<String>,
    /// Basename used for `--secrets-dir` restores.
    suggested_name: String,
    /// Source Unix mode. Extraction always writes private files as 0600.
    mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PrivateFileKind {
    DataDir,
    TlsCertificate,
    TlsPrivateKey,
}

struct PrivateFile {
    kind: PrivateFileKind,
    original_path: String,
    relative_path: Option<String>,
    suggested_name: String,
    mode: u32,
    data: Vec<u8>,
}

struct ExtractedPrivateFile {
    entry: SecretsManifestEntry,
    staging_path: PathBuf,
}

struct PlannedPrivateFile {
    source: PathBuf,
    destination: PathBuf,
}

#[derive(Clone)]
struct StagedPrivateFile {
    temporary: PathBuf,
    destination: PathBuf,
    backup: Option<PathBuf>,
}

struct BookshelfSwap {
    target: PathBuf,
    backup: Option<PathBuf>,
}

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
    export_archive_with_private_keys(data_dir, output, include_secrets, None)
}

/// Export a local SQLite data directory with an explicit private-key manifest.
///
/// Passing `None` is identical to [`export_archive_with_options`] and never
/// reads private material.
pub fn export_archive_with_private_keys(
    data_dir: &Path,
    output: &Path,
    include_secrets: bool,
    private_keys: Option<&PrivateKeyExportOptions>,
) -> Result<(), MigrateError> {
    let scratch = tempfile::tempdir().map_err(|error| {
        MigrateError::Archive(format!(
            "failed to create snapshot staging directory: {error}"
        ))
    })?;
    let snapshot = scratch.path().join(ARCHIVE_SNAPSHOT_ENTRY);
    export_with_options(data_dir, &snapshot, include_secrets)?;
    let private_files = match private_keys {
        Some(options) => collect_private_files(data_dir, options)?,
        None => Vec::new(),
    };
    write_archive(
        output,
        &snapshot,
        Some(&data_dir.join(ARCHIVE_BOOKSHELF_PREFIX)),
        &private_files,
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
    export_archive_postgres_with_private_keys(
        database_url,
        bookshelf_dir,
        output,
        include_secrets,
        None,
    )
}

/// Export a PostgreSQL metadata store with an explicit private-key manifest.
pub fn export_archive_postgres_with_private_keys(
    database_url: &str,
    bookshelf_dir: &Path,
    output: &Path,
    include_secrets: bool,
    private_keys: Option<&PrivateKeyExportOptions>,
) -> Result<(), MigrateError> {
    if !bookshelf_dir.is_dir() {
        return Err(MigrateError::Archive(format!(
            "bookshelf directory {} does not exist; archive export only copies a local bookshelf. \
             For S3/Garage, replicate the bucket with your object-store tooling instead",
            bookshelf_dir.display()
        )));
    }
    let default_data_dir = bookshelf_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let private_files = match private_keys {
        Some(options) => collect_private_files(default_data_dir, options)?,
        None => Vec::new(),
    };
    let scratch = tempfile::tempdir().map_err(|error| {
        MigrateError::Archive(format!(
            "failed to create snapshot staging directory: {error}"
        ))
    })?;
    let snapshot = scratch.path().join(ARCHIVE_SNAPSHOT_ENTRY);
    export_postgres_with_options(database_url, &snapshot, include_secrets)?;
    write_archive(output, &snapshot, Some(bookshelf_dir), &private_files)
}

fn collect_private_files(
    default_data_dir: &Path,
    options: &PrivateKeyExportOptions,
) -> Result<Vec<PrivateFile>, MigrateError> {
    let data_dir = options.data_dir.as_deref().unwrap_or(default_data_dir);
    let mut files = Vec::new();
    let mut seen = HashSet::new();

    for name in DATA_DIR_PRIVATE_FILES {
        let path = data_dir.join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(MigrateError::Read {
                    path: path.clone(),
                    source: error,
                })
            }
        };
        if metadata.file_type().is_symlink() {
            return Err(MigrateError::Archive(format!(
                "refusing to archive symlinked private file {}; use an explicit regular file",
                path.display()
            )));
        }
        if !metadata.is_file() {
            return Err(MigrateError::Archive(format!(
                "private file {} is not a regular file",
                path.display()
            )));
        }
        add_private_file(
            &mut files,
            &mut seen,
            PrivateFileKind::DataDir,
            &path,
            Some(name.to_string()),
            &metadata,
        )?;
    }

    for (kind, path) in [
        (
            PrivateFileKind::TlsCertificate,
            options.tls_cert_path.as_ref(),
        ),
        (
            PrivateFileKind::TlsPrivateKey,
            options.tls_key_path.as_ref(),
        ),
    ] {
        let Some(path) = path else { continue };
        // TLS paths are explicit operator configuration, and deployments
        // commonly point them at managed symlinks (for example Let's Encrypt's
        // `live/current`). Follow those explicitly configured paths.
        let metadata = fs::metadata(path).map_err(|source| MigrateError::Read {
            path: path.clone(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(MigrateError::Archive(format!(
                "TLS material {} is not a regular file",
                path.display()
            )));
        }
        add_private_file(&mut files, &mut seen, kind, path, None, &metadata)?;
    }

    // `--secrets-dir` restores by suggested basename, so keep those unique
    // even when two source directories use the same file name.
    let mut used_names = HashSet::new();
    for private in &mut files {
        let original = private.suggested_name.clone();
        if used_names.insert(original.clone()) {
            continue;
        }
        let mut suffix = 2;
        loop {
            let candidate = format!("{suffix}-{original}");
            if used_names.insert(candidate.clone()) {
                private.suggested_name = candidate;
                break;
            }
            suffix += 1;
        }
    }

    Ok(files)
}

fn add_private_file(
    files: &mut Vec<PrivateFile>,
    seen: &mut HashSet<PathBuf>,
    kind: PrivateFileKind,
    path: &Path,
    relative_path: Option<String>,
    metadata: &fs::Metadata,
) -> Result<(), MigrateError> {
    let canonical = fs::canonicalize(path).map_err(|source| MigrateError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !seen.insert(canonical) {
        return Ok(());
    }
    let data = fs::read(path).map_err(|source| MigrateError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = 0o600;
    let suggested_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_private_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "private-file".to_string());
    files.push(PrivateFile {
        kind,
        original_path: path.display().to_string(),
        relative_path,
        suggested_name,
        mode,
        data,
    });
    Ok(())
}

fn sanitize_private_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Read only the snapshot envelope from an archive and report whether it
/// contains database secret values.
///
/// The CLI uses this to warn before a partial (`secrets_included: false`)
/// restore. It does not extract bookshelf or private material.
pub fn archive_secrets_included(input: &Path) -> Result<bool, MigrateError> {
    let file = File::open(input).map_err(|source| MigrateError::Read {
        path: input.to_path_buf(),
        source,
    })?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut entry_count = 0_usize;
    for entry in archive.entries().map_err(archive_error)? {
        entry_count += 1;
        check_entry_count(entry_count)?;
        let mut entry = entry.map_err(archive_error)?;
        let path = entry.path().map_err(archive_error)?;
        if path == Path::new(ARCHIVE_SNAPSHOT_ENTRY) {
            let size = entry.header().size().map_err(archive_error)?;
            if size > MAX_ARCHIVE_ENTRY_BYTES {
                return Err(MigrateError::Archive(
                    "snapshot.json exceeds the archive entry size limit".into(),
                ));
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(archive_error)?;
            let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
            return Ok(snapshot.secrets_included);
        }
    }
    Err(MigrateError::Archive(format!(
        "archive does not contain a {ARCHIVE_SNAPSHOT_ENTRY} file"
    )))
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
    import_archive_with_private_keys(
        data_dir,
        input,
        compression,
        &PrivateKeyImportOptions::default(),
    )
}

/// Import an archive into SQLite and restore any private key/certificate
/// material under the configured target paths or `secrets_dir`.
pub fn import_archive_with_private_keys(
    data_dir: &Path,
    input: &Path,
    compression: Compression,
    private_keys: &PrivateKeyImportOptions,
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
    let bookshelf_target = data_dir.join(ARCHIVE_BOOKSHELF_PREFIX);
    // Resolve and check every private destination and the bookshelf
    // replacement before the database or live bookshelf is touched.
    let private_plan = plan_private_files(&extracted.secrets, data_dir, private_keys)?;
    preflight_bookshelf(
        extracted.bookshelf.as_deref(),
        &bookshelf_target,
        private_keys.force,
    )?;
    let staged_private = stage_private_files(&private_plan)?;

    if let Err(error) = import_database_with_compression(
        &data_dir.join(DATABASE_FILE),
        &extracted.snapshot_path,
        compression,
    ) {
        cleanup_staged_private_files(&staged_private);
        return Err(error);
    }

    let mut bookshelf_swap = None;
    if let Some(bookshelf) = extracted.bookshelf {
        match swap_bookshelf(&bookshelf, &bookshelf_target) {
            Ok(swap) => bookshelf_swap = Some(swap),
            Err(error) => {
                cleanup_staged_private_files(&staged_private);
                return Err(error);
            }
        }
    }

    if let Err(error) = commit_staged_private_files(staged_private, private_keys.force) {
        if let Some(swap) = bookshelf_swap {
            rollback_bookshelf_swap(swap);
        }
        return Err(error);
    }
    if let Some(swap) = bookshelf_swap {
        finalize_bookshelf_swap(swap);
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
    import_archive_postgres_with_private_keys(
        database_url,
        bookshelf_dir,
        input,
        compression,
        &PrivateKeyImportOptions::default(),
    )
}

/// Import an archive into PostgreSQL and restore private key/certificate
/// material under the configured target paths or `secrets_dir`.
pub fn import_archive_postgres_with_private_keys(
    database_url: &str,
    bookshelf_dir: &Path,
    input: &Path,
    compression: Compression,
    private_keys: &PrivateKeyImportOptions,
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
    let default_data_dir = bookshelf_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let extracted = extract_archive(input, staging.path())?;
    let private_plan = plan_private_files(&extracted.secrets, default_data_dir, private_keys)?;
    preflight_bookshelf(
        extracted.bookshelf.as_deref(),
        bookshelf_dir,
        private_keys.force,
    )?;
    let staged_private = stage_private_files(&private_plan)?;

    if let Err(error) =
        import_postgres_with_compression(database_url, &extracted.snapshot_path, compression)
    {
        cleanup_staged_private_files(&staged_private);
        return Err(error);
    }

    let mut bookshelf_swap = None;
    if let Some(bookshelf) = extracted.bookshelf {
        match swap_bookshelf(&bookshelf, bookshelf_dir) {
            Ok(swap) => bookshelf_swap = Some(swap),
            Err(error) => {
                cleanup_staged_private_files(&staged_private);
                return Err(error);
            }
        }
    }

    if let Err(error) = commit_staged_private_files(staged_private, private_keys.force) {
        if let Some(swap) = bookshelf_swap {
            rollback_bookshelf_swap(swap);
        }
        return Err(error);
    }
    if let Some(swap) = bookshelf_swap {
        finalize_bookshelf_swap(swap);
    }
    Ok(())
}

struct ExtractedArchive {
    snapshot_path: PathBuf,
    bookshelf: Option<PathBuf>,
    secrets: Vec<ExtractedPrivateFile>,
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
    let secrets_path = staging.join(ARCHIVE_SECRETS_PREFIX);
    let secrets_manifest_path = staging.join(ARCHIVE_SECRETS_MANIFEST);
    let mut seen_snapshot = false;
    let mut seen_bookshelf = false;
    let mut seen_secrets = false;
    let mut seen_manifest = false;
    let mut secret_staging_paths = Vec::new();
    let mut total_bytes = 0_u64;
    let mut entry_count = 0_usize;

    let entries = archive.entries().map_err(archive_error)?;
    for entry in entries {
        entry_count += 1;
        check_entry_count(entry_count)?;
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
        let is_secrets_root = path == Path::new(ARCHIVE_SECRETS_PREFIX);
        let is_secrets_files_root = path == Path::new(ARCHIVE_SECRETS_FILES_PREFIX);
        let is_manifest = path == Path::new(ARCHIVE_SECRETS_MANIFEST);
        let is_secret_member =
            path.starts_with(ARCHIVE_SECRETS_FILES_PREFIX) && !is_secrets_files_root;
        let allowed = is_snapshot
            || is_bookshelf_root
            || is_bookshelf_member
            || is_secrets_root
            || is_secrets_files_root
            || is_manifest
            || is_secret_member;
        if !allowed {
            return Err(MigrateError::Archive(format!(
                "rejected archive entry {:?}: only {ARCHIVE_SNAPSHOT_ENTRY}, \
                 {ARCHIVE_BOOKSHELF_PREFIX}/, and {ARCHIVE_SECRETS_PREFIX}/ are allowed",
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

        if is_bookshelf_member {
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
            continue;
        }

        if is_secrets_root || is_secrets_files_root {
            if !entry_type.is_dir() {
                return Err(MigrateError::Archive(format!(
                    "archive entry {} must be a directory",
                    path.display()
                )));
            }
            seen_secrets = true;
            fs::create_dir_all(&secrets_path).map_err(|source| MigrateError::Write {
                path: secrets_path.clone(),
                source,
            })?;
            if is_secrets_files_root {
                fs::create_dir_all(staging.join(ARCHIVE_SECRETS_FILES_PREFIX)).map_err(
                    |source| MigrateError::Write {
                        path: staging.join(ARCHIVE_SECRETS_FILES_PREFIX),
                        source,
                    },
                )?;
            }
            continue;
        }

        if is_manifest {
            if seen_manifest || entry_type.is_dir() {
                return Err(MigrateError::Archive(
                    "archive contains more than one secrets/manifest.json or a directory with that name"
                        .into(),
                ));
            }
            seen_manifest = true;
            seen_secrets = true;
            let size = entry.header().size().map_err(archive_error)?;
            check_entry_size(size, &mut total_bytes, &path)?;
            if let Some(parent) = secrets_manifest_path.parent() {
                fs::create_dir_all(parent).map_err(|source| MigrateError::Write {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&secrets_manifest_path)
                .map_err(|source| MigrateError::Write {
                    path: secrets_manifest_path.clone(),
                    source,
                })?;
            std::io::copy(&mut entry, &mut output).map_err(|source| MigrateError::Write {
                path: secrets_manifest_path.clone(),
                source,
            })?;
            output.flush().map_err(|source| MigrateError::Write {
                path: secrets_manifest_path.clone(),
                source,
            })?;
            continue;
        }

        // A private material payload must be a regular file directly under
        // secrets/files/. Nested directories are not part of the format.
        if entry_type.is_dir() {
            return Err(MigrateError::Archive(format!(
                "archive entry {} must not be a directory",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(ARCHIVE_SECRETS_FILES_PREFIX)
            .expect("secrets member was checked above");
        if relative.components().count() != 1 {
            return Err(MigrateError::Archive(format!(
                "archive entry {} must be directly under {ARCHIVE_SECRETS_FILES_PREFIX}/",
                path.display()
            )));
        }
        seen_secrets = true;
        let size = entry.header().size().map_err(archive_error)?;
        check_entry_size(size, &mut total_bytes, &path)?;
        let destination = staging.join(&path);
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
            path: destination.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).map_err(
                |source| MigrateError::Write {
                    path: destination.clone(),
                    source,
                },
            )?;
        }
        secret_staging_paths.push(destination);
    }

    if !seen_snapshot || !snapshot_path.is_file() {
        return Err(MigrateError::Archive(format!(
            "archive does not contain a {ARCHIVE_SNAPSHOT_ENTRY} file"
        )));
    }

    // Parse and validate before the caller touches the database.
    read_snapshot(&snapshot_path)?;

    let secrets = if seen_secrets {
        if !seen_manifest || !secrets_manifest_path.is_file() {
            return Err(MigrateError::Archive(
                "archive contains private material without secrets/manifest.json".into(),
            ));
        }
        read_private_manifest(&secrets_manifest_path, staging, &secret_staging_paths)?
    } else {
        Vec::new()
    };

    Ok(ExtractedArchive {
        snapshot_path,
        bookshelf: seen_bookshelf.then_some(bookshelf_path),
        secrets,
    })
}

fn read_private_manifest(
    manifest_path: &Path,
    staging: &Path,
    extracted_paths: &[PathBuf],
) -> Result<Vec<ExtractedPrivateFile>, MigrateError> {
    let bytes = fs::read(manifest_path).map_err(|source| MigrateError::Read {
        path: manifest_path.to_path_buf(),
        source,
    })?;
    let manifest: SecretsManifest = serde_json::from_slice(&bytes).map_err(|error| {
        MigrateError::Archive(format!("invalid secrets/manifest.json: {error}"))
    })?;
    if manifest.format != SECRETS_MANIFEST_FORMAT {
        return Err(MigrateError::Archive(format!(
            "unsupported secrets manifest format {:?}; expected {SECRETS_MANIFEST_FORMAT:?}",
            manifest.format
        )));
    }
    if manifest.version != SECRETS_MANIFEST_VERSION {
        return Err(MigrateError::Archive(format!(
            "unsupported secrets manifest version {}; this build supports {}",
            manifest.version, SECRETS_MANIFEST_VERSION
        )));
    }

    let mut seen_archive_paths = HashSet::new();
    let mut entries = Vec::new();
    for entry in manifest.entries {
        let archive_path = safe_archive_path(Path::new(&entry.archive_path)).ok_or_else(|| {
            MigrateError::Archive(format!(
                "secrets manifest path {:?} is not a safe relative path",
                entry.archive_path
            ))
        })?;
        let Some(relative) = archive_path.strip_prefix(ARCHIVE_SECRETS_FILES_PREFIX).ok() else {
            return Err(MigrateError::Archive(format!(
                "secrets manifest path {:?} is outside {ARCHIVE_SECRETS_FILES_PREFIX}/",
                entry.archive_path
            )));
        };
        if relative.components().count() != 1 {
            return Err(MigrateError::Archive(format!(
                "secrets manifest path {:?} must be directly under {ARCHIVE_SECRETS_FILES_PREFIX}/",
                entry.archive_path
            )));
        }
        if !seen_archive_paths.insert(archive_path.clone()) {
            return Err(MigrateError::Archive(format!(
                "secrets manifest repeats archive path {:?}",
                entry.archive_path
            )));
        }
        safe_single_name(&entry.suggested_name).ok_or_else(|| {
            MigrateError::Archive(format!(
                "secrets manifest suggested_name {:?} is not a safe file name",
                entry.suggested_name
            ))
        })?;
        if entry.mode > 0o777 {
            return Err(MigrateError::Archive(format!(
                "secrets manifest entry {:?} has invalid mode {:o}",
                entry.archive_path, entry.mode
            )));
        }
        match entry.kind {
            PrivateFileKind::DataDir => {
                let Some(relative_path) = &entry.relative_path else {
                    return Err(MigrateError::Archive(format!(
                        "secrets manifest data-dir entry {:?} has no relative_path",
                        entry.archive_path
                    )));
                };
                let Some(fixed_name) = data_dir_private_relative_path(relative_path) else {
                    return Err(MigrateError::Archive(format!(
                        "secrets manifest data-dir path {relative_path:?} is not an allowed \
                         private filename"
                    )));
                };
                if entry.suggested_name != fixed_name {
                    return Err(MigrateError::Archive(format!(
                        "secrets manifest data-dir entry {:?} has suggested_name {:?}; expected {:?}",
                        entry.archive_path, entry.suggested_name, fixed_name
                    )));
                }
            }
            PrivateFileKind::TlsCertificate | PrivateFileKind::TlsPrivateKey => {
                if entry.relative_path.is_some() {
                    return Err(MigrateError::Archive(format!(
                        "secrets manifest TLS entry {:?} must not carry a data-dir relative_path",
                        entry.archive_path
                    )));
                }
            }
        }
        let staging_path = staging.join(&archive_path);
        if !staging_path.is_file() {
            return Err(MigrateError::Archive(format!(
                "secrets manifest references missing file {:?}",
                entry.archive_path
            )));
        }
        entries.push(ExtractedPrivateFile {
            entry,
            staging_path,
        });
    }

    let expected: HashSet<PathBuf> = entries
        .iter()
        .map(|entry| entry.staging_path.clone())
        .collect();
    for extracted in extracted_paths {
        if !expected.contains(extracted) {
            return Err(MigrateError::Archive(format!(
                "archive contains unreferenced private file {:?}",
                extracted
                    .strip_prefix(staging)
                    .unwrap_or(extracted)
                    .display()
            )));
        }
    }
    Ok(entries)
}

/// Return the fixed data-dir filename for an allowed private file.
///
/// Data-dir private files are restricted to the same explicit manifest used
/// by export. This deliberately rejects nested paths such as
/// `nested/evil.conf`, database files such as `microwave.db`, and any other
/// file that would otherwise land inside the data directory.
fn data_dir_private_relative_path(value: &str) -> Option<&'static str> {
    DATA_DIR_PRIVATE_FILES
        .iter()
        .copied()
        .find(|name| *name == value)
}

fn safe_single_name(name: &str) -> Option<&str> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        None
    } else {
        Some(name)
    }
}

fn plan_private_files(
    extracted: &[ExtractedPrivateFile],
    fallback_data_dir: &Path,
    options: &PrivateKeyImportOptions,
) -> Result<Vec<PlannedPrivateFile>, MigrateError> {
    let data_dir = options.data_dir.as_deref().unwrap_or(fallback_data_dir);
    let mut destinations = HashSet::new();
    let mut plan = Vec::new();

    for private in extracted {
        let destination = if let Some(secrets_dir) = &options.secrets_dir {
            if let Ok(metadata) = fs::symlink_metadata(secrets_dir) {
                if metadata.file_type().is_symlink() {
                    return Err(MigrateError::Archive(format!(
                        "--secrets-dir {} must not be a symlink",
                        secrets_dir.display()
                    )));
                }
                if !metadata.is_dir() {
                    return Err(MigrateError::Archive(format!(
                        "--secrets-dir {} is not a directory",
                        secrets_dir.display()
                    )));
                }
            }
            let name = safe_single_name(&private.entry.suggested_name).ok_or_else(|| {
                MigrateError::Archive("secrets manifest has an unsafe suggested name".into())
            })?;
            secrets_dir.join(name)
        } else {
            match private.entry.kind {
                PrivateFileKind::DataDir => {
                    let relative = private.entry.relative_path.as_deref().ok_or_else(|| {
                        MigrateError::Archive(format!(
                            "private data-dir entry {:?} has no relative_path",
                            private.entry.archive_path
                        ))
                    })?;
                    let fixed_name = data_dir_private_relative_path(relative).ok_or_else(|| {
                        MigrateError::Archive(format!(
                            "private data-dir path {relative:?} is not an allowed private filename"
                        ))
                    })?;
                    if private.entry.suggested_name != fixed_name {
                        return Err(MigrateError::Archive(format!(
                            "private data-dir entry {:?} has an inconsistent suggested_name",
                            private.entry.archive_path
                        )));
                    }
                    data_dir.join(fixed_name)
                }
                PrivateFileKind::TlsCertificate => {
                    options.tls_cert_path.clone().ok_or_else(|| {
                        MigrateError::Archive(
                            "archive contains a TLS certificate but no target server.tls.cert_path \
                             was configured; pass --config or use --secrets-dir"
                                .into(),
                        )
                    })?
                }
                PrivateFileKind::TlsPrivateKey => {
                    options.tls_key_path.clone().ok_or_else(|| {
                        MigrateError::Archive(
                            "archive contains a TLS private key but no target server.tls.key_path \
                             was configured; pass --config or use --secrets-dir"
                                .into(),
                        )
                    })?
                }
            }
        };

        if !destinations.insert(destination.clone()) {
            return Err(MigrateError::Archive(format!(
                "archive maps more than one private file to {}",
                destination.display()
            )));
        }
        validate_private_destination_parent(&destination)?;
        match fs::symlink_metadata(&destination) {
            Ok(metadata) => {
                if metadata.is_dir() {
                    return Err(MigrateError::Archive(format!(
                        "refusing to replace directory {} with a private file",
                        destination.display()
                    )));
                }
                if !options.force {
                    return Err(MigrateError::Archive(format!(
                        "refusing to overwrite existing private file {}; pass --force to replace \
                         it (this can invalidate existing clients)",
                        destination.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(MigrateError::Read {
                    path: destination,
                    source: error,
                })
            }
        }
        plan.push(PlannedPrivateFile {
            source: private.staging_path.clone(),
            destination,
        });
    }
    Ok(plan)
}

fn validate_private_destination_parent(destination: &Path) -> Result<(), MigrateError> {
    let Some(parent) = destination.parent() else {
        return Ok(());
    };
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::metadata(&current) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(MigrateError::Archive(format!(
                    "private key parent {} is not a directory",
                    current.display()
                )))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(MigrateError::Read {
                    path: current,
                    source: error,
                })
            }
        }
    }
    Ok(())
}

fn stage_private_files(
    plan: &[PlannedPrivateFile],
) -> Result<Vec<StagedPrivateFile>, MigrateError> {
    let mut staged = Vec::with_capacity(plan.len());
    for private in plan {
        let result = (|| -> Result<StagedPrivateFile, MigrateError> {
            if let Some(parent) = private.destination.parent() {
                if !parent.as_os_str().is_empty() {
                    create_private_parents(parent)?;
                }
            }
            let data = fs::read(&private.source).map_err(|source| MigrateError::Read {
                path: private.source.clone(),
                source,
            })?;
            let temporary = private.destination.with_file_name(format!(
                ".{}.tmp-{}-{}",
                private
                    .destination
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("private"),
                std::process::id(),
                now_nanos()
            ));
            write_private_file(&temporary, &data)?;
            Ok(StagedPrivateFile {
                temporary,
                destination: private.destination.clone(),
                backup: None,
            })
        })();
        match result {
            Ok(file) => staged.push(file),
            Err(error) => {
                cleanup_staged_private_files(&staged);
                return Err(error);
            }
        }
    }
    Ok(staged)
}

fn cleanup_staged_private_files(staged: &[StagedPrivateFile]) {
    for private in staged {
        let _ = fs::remove_file(&private.temporary);
    }
}

fn commit_staged_private_files(
    staged: Vec<StagedPrivateFile>,
    force: bool,
) -> Result<(), MigrateError> {
    let mut committed: Vec<StagedPrivateFile> = Vec::new();
    let mut pending = staged.into_iter();
    while let Some(mut private) = pending.next() {
        match fs::symlink_metadata(&private.destination) {
            Ok(metadata) => {
                if metadata.is_dir() {
                    cleanup_staged_private_files(&[private.clone()]);
                    cleanup_staged_private_files(&committed);
                    cleanup_staged_private_files(&pending.collect::<Vec<_>>());
                    rollback_committed_private_files(&mut committed);
                    return Err(MigrateError::Archive(format!(
                        "refusing to replace directory {} with a private file",
                        private.destination.display()
                    )));
                }
                if !force {
                    cleanup_staged_private_files(&[private.clone()]);
                    cleanup_staged_private_files(&committed);
                    cleanup_staged_private_files(&pending.collect::<Vec<_>>());
                    rollback_committed_private_files(&mut committed);
                    return Err(MigrateError::Archive(format!(
                        "refusing to overwrite existing private file {}; pass --force to replace \
                         it (this can invalidate existing clients)",
                        private.destination.display()
                    )));
                }
                let backup = private.destination.with_file_name(format!(
                    ".{}.backup-{}-{}",
                    private
                        .destination
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("private"),
                    std::process::id(),
                    now_nanos()
                ));
                if let Err(source) = fs::rename(&private.destination, &backup) {
                    cleanup_staged_private_files(&[private.clone()]);
                    cleanup_staged_private_files(&pending.collect::<Vec<_>>());
                    rollback_committed_private_files(&mut committed);
                    return Err(MigrateError::Write {
                        path: private.destination.clone(),
                        source,
                    });
                }
                private.backup = Some(backup);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                cleanup_staged_private_files(&[private.clone()]);
                cleanup_staged_private_files(&pending.collect::<Vec<_>>());
                rollback_committed_private_files(&mut committed);
                return Err(MigrateError::Read {
                    path: private.destination,
                    source,
                });
            }
        }

        if let Err(source) = fs::rename(&private.temporary, &private.destination) {
            if let Some(backup) = &private.backup {
                let _ = fs::rename(backup, &private.destination);
            }
            cleanup_staged_private_files(&[private.clone()]);
            cleanup_staged_private_files(&pending.collect::<Vec<_>>());
            rollback_committed_private_files(&mut committed);
            return Err(MigrateError::Write {
                path: private.destination,
                source,
            });
        }
        committed.push(private);
    }
    for private in &committed {
        if let Some(backup) = &private.backup {
            remove_path_if_exists(backup);
        }
    }
    Ok(())
}

fn rollback_committed_private_files(committed: &mut Vec<StagedPrivateFile>) {
    for private in committed.drain(..).rev() {
        remove_path_if_exists(&private.destination);
        if let Some(backup) = &private.backup {
            let _ = fs::rename(backup, &private.destination);
        }
    }
}

fn remove_path_if_exists(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            let _ = fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
}

fn write_private_file(path: &Path, data: &[u8]) -> Result<(), MigrateError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|source| MigrateError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(data).map_err(|source| MigrateError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    file.flush().map_err(|source| MigrateError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            MigrateError::Write {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

fn create_private_parents(parent: &Path) -> Result<(), MigrateError> {
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(MigrateError::Archive(format!(
                        "private key parent {} is not a directory",
                        current.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|source| MigrateError::Write {
                    path: current.clone(),
                    source,
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&current, fs::Permissions::from_mode(0o700)).map_err(
                        |source| MigrateError::Write {
                            path: current.clone(),
                            source,
                        },
                    )?;
                }
            }
            Err(error) => {
                return Err(MigrateError::Read {
                    path: current,
                    source: error,
                })
            }
        }
    }
    Ok(())
}

fn check_entry_count(count: usize) -> Result<(), MigrateError> {
    if count > MAX_ARCHIVE_ENTRIES {
        return Err(MigrateError::Archive(format!(
            "archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
        )));
    }
    Ok(())
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
    private_files: &[PrivateFile],
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

        if !private_files.is_empty() {
            append_directory(&mut builder, Path::new(ARCHIVE_SECRETS_PREFIX))?;
            append_directory(&mut builder, Path::new(ARCHIVE_SECRETS_FILES_PREFIX))?;
            let mut entries = Vec::with_capacity(private_files.len());
            for (index, private) in private_files.iter().enumerate() {
                let archive_path = format!(
                    "{ARCHIVE_SECRETS_FILES_PREFIX}/{:04}-{}",
                    index + 1,
                    private.suggested_name
                );
                append_bytes(&mut builder, Path::new(&archive_path), &private.data, 0o600)?;
                entries.push(SecretsManifestEntry {
                    kind: private.kind,
                    archive_path,
                    original_path: private.original_path.clone(),
                    relative_path: private.relative_path.clone(),
                    suggested_name: private.suggested_name.clone(),
                    mode: private.mode,
                });
            }
            let manifest = SecretsManifest {
                format: SECRETS_MANIFEST_FORMAT.to_string(),
                version: SECRETS_MANIFEST_VERSION,
                entries,
            };
            let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
                MigrateError::Archive(format!("failed to serialize secrets manifest: {error}"))
            })?;
            append_bytes(
                &mut builder,
                Path::new(ARCHIVE_SECRETS_MANIFEST),
                &manifest_bytes,
                0o600,
            )?;
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

fn preflight_bookshelf(
    staged: Option<&Path>,
    target: &Path,
    force: bool,
) -> Result<(), MigrateError> {
    let Some(staged) = staged else {
        return Ok(());
    };
    if !directory_is_empty(staged)? {
        return Ok(());
    }
    let target_has_content = match fs::read_dir(target) {
        Ok(mut entries) => entries.next().is_some(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => true,
        Err(error) => {
            return Err(MigrateError::Read {
                path: target.to_path_buf(),
                source: error,
            })
        }
    };
    if target_has_content && !force {
        return Err(MigrateError::Archive(format!(
            "archive bookshelf is empty but target {} is not; pass --force to replace a populated \
             bookshelf with an empty archive",
            target.display()
        )));
    }
    Ok(())
}

fn directory_is_empty(path: &Path) -> Result<bool, MigrateError> {
    let mut entries = fs::read_dir(path).map_err(|source| MigrateError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(entries.next().is_none())
}

fn swap_bookshelf(staged: &Path, target: &Path) -> Result<BookshelfSwap, MigrateError> {
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
    Ok(BookshelfSwap {
        target: target.to_path_buf(),
        backup: had_target.then_some(backup),
    })
}

fn finalize_bookshelf_swap(swap: BookshelfSwap) {
    if let Some(backup) = swap.backup {
        remove_path_if_exists(&backup);
    }
}

fn rollback_bookshelf_swap(swap: BookshelfSwap) {
    // Never follow a symlink at the target: remove the restored directory and
    // put the previous entry (which may itself be a symlink) back.
    remove_path_if_exists(&swap.target);
    if let Some(backup) = swap.backup {
        let _ = fs::rename(backup, &swap.target);
    }
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

#[cfg(test)]
mod tests {
    use super::{check_entry_count, MAX_ARCHIVE_ENTRIES};

    #[test]
    fn archive_entry_cap_is_enforced() {
        assert!(check_entry_count(MAX_ARCHIVE_ENTRIES).is_ok());
        assert!(check_entry_count(MAX_ARCHIVE_ENTRIES + 1).is_err());
    }
}
