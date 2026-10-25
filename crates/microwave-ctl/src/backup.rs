use crate::pg::{normalize_database_name, PgTarget};
use crate::{context::Context, BackupFormat, CtlError};
use microwave_config::DatabaseDriver;
use microwave_store::SqliteStore;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{IsTerminal, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const DATABASE_FILE: &str = "microwave.db";
const POSTGRES_DUMP_FILE: &str = "microwave.dump";
const BOOKSHELF_DIR: &str = "bookshelf";
const PRIVATE_DIR: &str = "private";
const PRIVATE_FILES_DIR: &str = "private/files";
const PRIVATE_MANIFEST: &str = "private/manifest.json";
const PRIVATE_MANIFEST_FORMAT: &str = "microwave-ctl-private-files";
const PRIVATE_MANIFEST_VERSION: u32 = 1;
const PRIVATE_MODE: u32 = 0o600;
const PUBLIC_MODE: u32 = 0o644;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const MAX_PRIVATE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PRIVATE_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const TLS_CERT_TARGET: &str = "tls-cert";
const TLS_KEY_TARGET: &str = "tls-key";

/// Explicit data-directory private-file allowlist. This is a fixed manifest,
/// never a glob: unknown files are never read or archived.
const DATA_DIR_PRIVATE_FILES: [&str; 13] = [
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

/// Create a backup containing a consistent database dump and a copy of the
/// local bookshelf, when one exists.
///
/// SQLite uses `VACUUM INTO`; PostgreSQL uses a custom-format `pg_dump`
/// archive. Both include committed WAL/transaction contents and are safe to
/// take while a server is running. The directory format writes the files
/// as-is; the tar format writes the same layout into one uncompressed archive.
///
/// With `include_private_keys`, an explicit `private/` subtree is added for the
/// fixed server key/certificate allowlist. File contents are never printed.
pub fn backup(
    context: &Context,
    path: &Path,
    format: BackupFormat,
    include_private_keys: bool,
) -> Result<(), CtlError> {
    let private_files = if include_private_keys {
        Some(collect_private_backup_files(context)?)
    } else {
        None
    };

    match format {
        BackupFormat::Directory => backup_directory(context, path, private_files.as_deref())?,
        BackupFormat::Tar => backup_tar(context, path, private_files.as_deref())?,
    }

    println!("Backup written to {}", path.display());
    println!(
        "note: the bookshelf copy is not atomic with the database dump; drain writes for a strict backup."
    );
    match &private_files {
        Some(files) if files.is_empty() => println!(
            "no allowlisted private key/certificate payload files were present; an empty private manifest was written."
        ),
        Some(files) => {
            let private = files.iter().filter(|file| file.private).count();
            println!(
                "included {} private payload file(s) under {PRIVATE_DIR}/ ({private} private, {} public); the backup contains unencrypted key material.",
                files.len(),
                files.len().saturating_sub(private)
            );
        }
        None => println!(
            "private key/certificate files were not included; create a NEW backup in a NEW destination with --include-private-keys. An existing backup destination cannot be extended with keys."
        ),
    }
    Ok(())
}

fn backup_directory(
    context: &Context,
    path: &Path,
    private_files: Option<&[PrivateBackupFile]>,
) -> Result<(), CtlError> {
    if path.exists() && !path.is_dir() {
        return Err(CtlError::message(format!(
            "backup destination {} is not a directory; use --format tar for a file destination",
            path.display()
        )));
    }
    std::fs::create_dir_all(path)?;
    write_backup_payload(context, path, private_files)
}

fn backup_tar(
    context: &Context,
    path: &Path,
    private_files: Option<&[PrivateBackupFile]>,
) -> Result<(), CtlError> {
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
    write_backup_payload(context, staging.path(), private_files)?;

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

    match context.config.database.driver {
        DatabaseDriver::Sqlite => {
            builder.append_path_with_name(staging.path().join(DATABASE_FILE), DATABASE_FILE)?;
        }
        DatabaseDriver::Postgres => {
            builder.append_path_with_name(
                staging.path().join(POSTGRES_DUMP_FILE),
                POSTGRES_DUMP_FILE,
            )?;
        }
    }

    let bookshelf = staging.path().join(BOOKSHELF_DIR);
    if bookshelf.is_dir() {
        builder.append_dir_all(BOOKSHELF_DIR, &bookshelf)?;
    }
    if private_files.is_some() {
        builder.append_dir_all(PRIVATE_DIR, staging.path().join(PRIVATE_DIR))?;
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
    private_files: Option<&[PrivateBackupFile]>,
) -> Result<(), CtlError> {
    // Resolve any existing private/ tree before writing the database. A
    // destination is either new, contains a tree this tool wrote, or is
    // rejected; never recursively delete an unrelated user directory.
    prepare_private_backup_destination(path)?;

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

    if let Some(files) = private_files {
        write_private_backup(path, files)?;
    }
    Ok(())
}

/// Remove a previous private/ tree from a backup destination only when it is
/// recognizably the tree this command writes. An unrelated directory is
/// refused rather than deleted.
fn prepare_private_backup_destination(path: &Path) -> Result<(), CtlError> {
    let private_root = path.join(PRIVATE_DIR);
    match std::fs::symlink_metadata(&private_root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(CtlError::message(format!(
                "backup destination contains {PRIVATE_DIR}/, but it is not a directory this command can replace; choose a new destination"
            )))
        }
        Ok(_) => {
            if is_recognized_private_backup_tree(&private_root)? {
                remove_path(&private_root)
            } else {
                Err(CtlError::message(format!(
                    "backup destination contains {PRIVATE_DIR}/ that is not a recognizable microwave-ctl private backup tree; refusing to delete it. Choose a new backup destination."
                )))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn is_recognized_private_backup_tree(private_root: &Path) -> Result<bool, CtlError> {
    let manifest_path = private_root.join("manifest.json");
    let files_root = private_root.join("files");
    if !is_regular_non_symlink(&manifest_path)? || !is_directory_non_symlink(&files_root)? {
        return Ok(false);
    }
    let bytes = match read_private_bytes(&manifest_path, false) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(false),
    };
    let Ok(manifest) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(false);
    };
    Ok(
        manifest.get("format").and_then(Value::as_str) == Some(PRIVATE_MANIFEST_FORMAT)
            && manifest.get("version").and_then(Value::as_u64)
                == Some(u64::from(PRIVATE_MANIFEST_VERSION)),
    )
}

fn is_regular_non_symlink(path: &Path) -> Result<bool, CtlError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn is_directory_non_symlink(path: &Path) -> Result<bool, CtlError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrivateKind {
    DataDir,
    TlsCertificate,
    TlsPrivateKey,
}

impl PrivateKind {
    fn manifest_name(self) -> &'static str {
        match self {
            Self::DataDir => "data-dir",
            Self::TlsCertificate => "tls-cert",
            Self::TlsPrivateKey => "tls-key",
        }
    }

    /// Configured TLS paths are operator-controlled and commonly traverse
    /// managed symlinks (for example Let's Encrypt's `live/current`).
    fn allows_parent_symlinks(self) -> bool {
        !matches!(self, Self::DataDir)
    }
}

/// One validated private file ready to be written into a backup.
struct PrivateBackupFile {
    kind: PrivateKind,
    /// Logical destination: a data-relative file name, `tls-cert`, or
    /// `tls-key`.
    target: String,
    /// Fixed name inside `private/files/`.
    archive_name: String,
    mode: u32,
    private: bool,
    data: Vec<u8>,
}

impl PrivateBackupFile {
    fn archive_path(&self) -> String {
        format!("{PRIVATE_FILES_DIR}/{}", self.archive_name)
    }
}

fn data_dir_file_is_private(name: &str) -> bool {
    !matches!(name, "webui_pub.pem" | "oc_id_pub.pem" | "ocid_pub.pem")
}

fn collect_private_backup_files(context: &Context) -> Result<Vec<PrivateBackupFile>, CtlError> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut total = 0_u64;

    for name in DATA_DIR_PRIVATE_FILES {
        let source = context.data_dir.join(name);
        match std::fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CtlError::message(format!(
                    "refusing to back up symlinked private file {}; use a regular file",
                    source.display()
                )));
            }
            Ok(metadata) if metadata.is_file() => {
                let private = data_dir_file_is_private(name);
                add_private_backup_file(
                    &mut files,
                    &mut seen,
                    &mut total,
                    PrivateKind::DataDir,
                    name,
                    name,
                    private,
                    &source,
                    false,
                )?;
            }
            Ok(_) => {
                return Err(CtlError::message(format!(
                    "private file {} is not a regular file",
                    source.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
    }

    for (kind, target, archive_name, private, configured) in [
        (
            PrivateKind::TlsCertificate,
            TLS_CERT_TARGET,
            "tls-cert.pem",
            false,
            context.config.server.tls.cert_path.as_deref(),
        ),
        (
            PrivateKind::TlsPrivateKey,
            TLS_KEY_TARGET,
            "tls-key.pem",
            true,
            context.config.server.tls.key_path.as_deref(),
        ),
    ] {
        let Some(configured) = configured.map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };
        let source = PathBuf::from(configured);
        let metadata = match std::fs::metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CtlError::message(format!(
                    "configured {target} file {} does not exist",
                    source.display()
                )));
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() {
            return Err(CtlError::message(format!(
                "configured {target} file {} is not a regular file",
                source.display()
            )));
        }
        // TLS paths are explicit configuration and may be managed symlinks.
        add_private_backup_file(
            &mut files,
            &mut seen,
            &mut total,
            kind,
            target,
            archive_name,
            private,
            &source,
            true,
        )?;
    }

    Ok(files)
}

#[allow(clippy::too_many_arguments)]
fn add_private_backup_file(
    files: &mut Vec<PrivateBackupFile>,
    seen: &mut HashSet<PathBuf>,
    total: &mut u64,
    kind: PrivateKind,
    target: &str,
    archive_name: &str,
    private: bool,
    source: &Path,
    follow_symlinks: bool,
) -> Result<(), CtlError> {
    if !follow_symlinks {
        let metadata = std::fs::symlink_metadata(source)?;
        if metadata.file_type().is_symlink() {
            return Err(CtlError::message(format!(
                "refusing to back up symlinked private file {}",
                source.display()
            )));
        }
    }
    let canonical = std::fs::canonicalize(source)?;
    if !seen.insert(canonical) {
        return Ok(());
    }

    let metadata = std::fs::metadata(source)?;
    if !metadata.is_file() {
        return Err(CtlError::message(format!(
            "private file {} is not a regular file",
            source.display()
        )));
    }
    if metadata.len() > MAX_PRIVATE_FILE_BYTES {
        return Err(CtlError::message(format!(
            "private file {} exceeds the {MAX_PRIVATE_FILE_BYTES}-byte file limit",
            source.display()
        )));
    }
    if total.saturating_add(metadata.len()) > MAX_PRIVATE_TOTAL_BYTES {
        return Err(CtlError::message(format!(
            "private files exceed the {MAX_PRIVATE_TOTAL_BYTES}-byte total limit"
        )));
    }
    let data = read_private_bytes(source, follow_symlinks)?;
    *total = total.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));

    files.push(PrivateBackupFile {
        kind,
        target: target.to_string(),
        archive_name: archive_name.to_string(),
        mode: if private { PRIVATE_MODE } else { PUBLIC_MODE },
        private,
        data,
    });
    Ok(())
}

fn write_private_backup(path: &Path, files: &[PrivateBackupFile]) -> Result<(), CtlError> {
    let private_root = path.join(PRIVATE_DIR);
    let files_root = path.join(PRIVATE_FILES_DIR);
    std::fs::create_dir_all(&files_root)?;
    set_mode(&private_root, PRIVATE_DIRECTORY_MODE)?;
    set_mode(&files_root, PRIVATE_DIRECTORY_MODE)?;

    for file in files {
        write_new_file_mode(&files_root.join(&file.archive_name), &file.data, file.mode)?;
    }

    let entries = files
        .iter()
        .map(|file| {
            json!({
                "kind": file.kind.manifest_name(),
                "path": file.archive_path(),
                "target": file.target,
                "mode": file.mode,
            })
        })
        .collect::<Vec<_>>();
    let manifest = json!({
        "format": PRIVATE_MANIFEST_FORMAT,
        "version": PRIVATE_MANIFEST_VERSION,
        "entries": entries,
    });
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        CtlError::message(format!("failed to encode private manifest: {error}"))
    })?;
    write_new_file_mode(&path.join(PRIVATE_MANIFEST), &bytes, PRIVATE_MODE)?;
    Ok(())
}

fn write_new_file_mode(path: &Path, data: &[u8], mode: u32) -> Result<(), CtlError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let mut file = options.open(path).map_err(|error| {
        CtlError::message(format!("failed to create private file {path:?}: {error}"))
    })?;
    file.write_all(data).map_err(|error| {
        CtlError::message(format!("failed to write private file {path:?}: {error}"))
    })?;
    file.sync_all().map_err(|error| {
        CtlError::message(format!("failed to sync private file {path:?}: {error}"))
    })?;
    set_mode(path, mode).map_err(|error| {
        CtlError::message(format!(
            "failed to set mode on private file {path:?}: {error}"
        ))
    })?;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), CtlError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
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
    restore_private_keys: bool,
    private_only: bool,
) -> Result<(), CtlError> {
    if private_only && !restore_private_keys {
        return Err(CtlError::message(
            "--private-only requires --restore-private-keys",
        ));
    }

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

    match format {
        BackupFormat::Directory => {
            if !path.is_dir() {
                return Err(CtlError::message(format!(
                    "--format directory requires a backup directory, but {} is not a directory",
                    path.display()
                )));
            }
            if private_only {
                restore_private_only(context, path, yes)?;
            } else {
                restore_directory(context, path, yes, restore_private_keys)?;
            }
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
            if private_only {
                restore_private_only(context, staging.path(), yes)?;
            } else {
                restore_directory(context, staging.path(), yes, restore_private_keys)?;
            }
        }
    }

    if private_only {
        println!("Restored private files from {}", path.display());
    } else {
        println!("Restored from {}", path.display());
    }
    Ok(())
}

/// Restore only the `private/` subtree. The database and bookshelf are not
/// touched, so an operator can retry a failed private apply without replaying
/// metadata. The manifest and destinations are validated and staged first.
fn restore_private_only(context: &Context, path: &Path, yes: bool) -> Result<(), CtlError> {
    let staged = stage_private_restore(context, path)?;
    let total = staged.entries.len();
    if total > 0 {
        eprintln!(
            "warning: restoring private keys/certificates can invalidate existing clients and break a running server; {total} file(s) will be applied. The database and bookshelf will NOT be restored."
        );
    }
    confirm_destructive("private key restore", yes)?;

    match apply_private_restore(staged) {
        Ok(count) => {
            println!(
                "restored {count} private payload file(s); restart the server to load the restored key/certificate material."
            );
            Ok(())
        }
        Err(error) => Err(CtlError::message(format!(
            "private key restore failed: {error}. The database and bookshelf were not touched; fix the reported problem and retry with --private-only."
        ))),
    }
}

fn restore_directory(
    context: &Context,
    path: &Path,
    yes: bool,
    restore_private_keys: bool,
) -> Result<(), CtlError> {
    // Validate the private manifest and stage its payload before the database
    // or live bookshelf is touched. A malformed manifest therefore fails while
    // the live installation is still intact, even when tar extraction already
    // succeeded.
    let staged_private = if restore_private_keys {
        Some(stage_private_restore(context, path)?)
    } else {
        if private_material_present(path) {
            eprintln!(
                "warning: backup contains private key/certificate material under {PRIVATE_DIR}/, but --restore-private-keys was not given; skipping it. Existing target key files are untouched."
            );
        }
        None
    };

    // Validate and copy the backup bookshelf before the database or live
    // bookshelf is touched. A symlink/FIFO (or any read error) in the backup
    // must fail here, while the live installation is still intact.
    let staged_bookshelf = stage_bookshelf(context, &path.join(BOOKSHELF_DIR))?;

    if let Some(private) = &staged_private {
        if !private.entries.is_empty() {
            eprintln!(
                "warning: restoring private keys/certificates can invalidate existing clients and break a running server; {} file(s) will be applied after the database and bookshelf restore.",
                private.entries.len()
            );
        }
    }

    match context.config.database.driver {
        DatabaseDriver::Sqlite => restore_sqlite(context, path, yes)?,
        DatabaseDriver::Postgres => restore_postgres(context, path, yes)?,
    }

    swap_bookshelf(context, staged_bookshelf)?;

    if let Some(private) = staged_private {
        match apply_private_restore(private) {
            Ok(count) => {
                if count > 0 {
                    println!(
                        "restored {count} private payload file(s); restart the server to load the restored key/certificate material."
                    );
                }
            }
            Err(error) => {
                return Err(CtlError::message(format!(
                    "private key restore failed: {error}. The database and bookshelf were restored and remain intact; no database/bookshelf replay is needed. Fix the reported problem and retry with --restore-private-keys --private-only."
                )));
            }
        }
    }
    Ok(())
}

fn private_material_present(path: &Path) -> bool {
    std::fs::symlink_metadata(path.join(PRIVATE_DIR)).is_ok()
        || std::fs::symlink_metadata(path.join(PRIVATE_MANIFEST)).is_ok()
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

/// A validated private destination with its new contents staged beside the
/// canonical target.
struct PrivateRestoreEntry {
    target: PathBuf,
    mode: u32,
    temporary: PathBuf,
    backup: Option<PathBuf>,
    old_mode: Option<u32>,
}

struct StagedPrivateRestore {
    entries: Vec<PrivateRestoreEntry>,
}

impl Drop for StagedPrivateRestore {
    fn drop(&mut self) {
        for entry in &self.entries {
            let _ = std::fs::remove_file(&entry.temporary);
            if let Some(backup) = &entry.backup {
                let _ = std::fs::remove_file(backup);
            }
        }
    }
}

fn stage_private_restore(
    context: &Context,
    backup: &Path,
) -> Result<StagedPrivateRestore, CtlError> {
    let private_root = backup.join(PRIVATE_DIR);
    match std::fs::symlink_metadata(&private_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(CtlError::message(format!(
                "backup private directory {private_root:?} is a symlink; refusing to restore it"
            )));
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(CtlError::message(format!(
                "backup private path {private_root:?} is not a directory"
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CtlError::message(format!(
                "backup {backup:?} does not contain {PRIVATE_MANIFEST}; it was probably created without --include-private-keys"
            )));
        }
        Err(error) => return Err(error.into()),
    }

    for entry in std::fs::read_dir(&private_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            CtlError::message("backup private directory contains a non-UTF-8 name")
        })?;
        if name != "manifest.json" && name != "files" {
            return Err(CtlError::message(format!(
                "backup private directory contains unexpected entry {name:?}; expected manifest.json and files/"
            )));
        }
    }

    let manifest_path = backup.join(PRIVATE_MANIFEST);
    match std::fs::symlink_metadata(&manifest_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(CtlError::message(format!(
                "backup {PRIVATE_MANIFEST} is a symlink; refusing to restore it"
            )));
        }
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(CtlError::message(format!(
                "backup {PRIVATE_MANIFEST} is not a regular file"
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CtlError::message(format!(
                "backup {backup:?} does not contain {PRIVATE_MANIFEST}; it was probably created without --include-private-keys"
            )));
        }
        Err(error) => return Err(error.into()),
    }
    let bytes = read_regular_private_file(&manifest_path)?;
    let manifest: Value = serde_json::from_slice(&bytes)
        .map_err(|error| CtlError::message(format!("invalid {PRIVATE_MANIFEST}: {error}")))?;
    let format = manifest
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| CtlError::message(format!("{PRIVATE_MANIFEST} has no format")))?;
    if format != PRIVATE_MANIFEST_FORMAT {
        return Err(CtlError::message(format!(
            "unsupported private manifest format {format:?}; expected {PRIVATE_MANIFEST_FORMAT:?}"
        )));
    }
    let version = manifest
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| CtlError::message(format!("{PRIVATE_MANIFEST} has no version")))?;
    if version != u64::from(PRIVATE_MANIFEST_VERSION) {
        return Err(CtlError::message(format!(
            "unsupported private manifest version {version}; this build supports {PRIVATE_MANIFEST_VERSION}"
        )));
    }
    let raw_entries = manifest
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| CtlError::message(format!("{PRIVATE_MANIFEST} has no entries array")))?;

    // Resolve the configured data directory before constructing destinations.
    // A data_dir that is itself a symlink is valid and is resolved to its
    // canonical directory, matching server startup.
    let data_dir = canonical_data_dir(context)?;

    let mut entries = Vec::with_capacity(raw_entries.len());
    let mut seen_paths = HashSet::new();
    let mut seen_targets = HashSet::new();
    let mut total = 0_u64;

    for raw_entry in raw_entries {
        let kind_name = manifest_field(raw_entry, "kind")?;
        let kind = match kind_name {
            "data-dir" => PrivateKind::DataDir,
            "tls-cert" => PrivateKind::TlsCertificate,
            "tls-key" => PrivateKind::TlsPrivateKey,
            other => {
                return Err(CtlError::message(format!(
                    "private manifest contains unknown kind {other:?}"
                )));
            }
        };
        let archive_path = manifest_field(raw_entry, "path")?.to_string();
        private_manifest_file_name(&archive_path).ok_or_else(|| {
            CtlError::message(format!(
                "private manifest path {archive_path:?} is not {PRIVATE_FILES_DIR}/<file>"
            ))
        })?;
        if !seen_paths.insert(archive_path.clone()) {
            return Err(CtlError::message(format!(
                "private manifest repeats path {archive_path:?}"
            )));
        }
        let target = manifest_field(raw_entry, "target")?.to_string();
        let mode = raw_entry
            .get("mode")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                CtlError::message(format!(
                    "private manifest entry {archive_path:?} has no numeric mode"
                ))
            })?;

        let (destination, expected_mode) = match kind {
            PrivateKind::DataDir => {
                let fixed = DATA_DIR_PRIVATE_FILES
                    .iter()
                    .copied()
                    .find(|name| *name == target)
                    .ok_or_else(|| {
                        CtlError::message(format!(
                            "private manifest target {target:?} is not an allowlisted data-directory file"
                        ))
                    })?;
                let private = data_dir_file_is_private(fixed);
                (
                    data_dir.join(fixed),
                    if private { PRIVATE_MODE } else { PUBLIC_MODE },
                )
            }
            PrivateKind::TlsCertificate => {
                if target != TLS_CERT_TARGET {
                    return Err(CtlError::message(format!(
                        "private manifest tls-cert target must be {TLS_CERT_TARGET:?}, got {target:?}"
                    )));
                }
                let configured = resolve_tls_target(context, kind)?;
                (configured, PUBLIC_MODE)
            }
            PrivateKind::TlsPrivateKey => {
                if target != TLS_KEY_TARGET {
                    return Err(CtlError::message(format!(
                        "private manifest tls-key target must be {TLS_KEY_TARGET:?}, got {target:?}"
                    )));
                }
                let configured = resolve_tls_target(context, kind)?;
                (configured, PRIVATE_MODE)
            }
        };
        if mode != u64::from(expected_mode) {
            return Err(CtlError::message(format!(
                "private manifest entry {archive_path:?} has mode {mode:o}; expected {expected_mode:o}"
            )));
        }
        if !seen_targets.insert(destination.clone()) {
            return Err(CtlError::message(format!(
                "private manifest maps more than one file to {destination:?}"
            )));
        }

        let source = backup.join(&archive_path);
        let data = read_regular_private_file(&source)?;
        if total.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX))
            > MAX_PRIVATE_TOTAL_BYTES
        {
            return Err(CtlError::message(format!(
                "private files exceed the {MAX_PRIVATE_TOTAL_BYTES}-byte total limit"
            )));
        }
        total = total.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));

        entries.push((
            PrivateRestoreEntry {
                target: destination,
                mode: expected_mode,
                temporary: PathBuf::new(),
                backup: None,
                old_mode: None,
            },
            data,
            kind,
        ));
    }

    validate_private_files_directory(backup, &seen_paths)?;

    stage_private_destinations(entries)
}

/// Preflight and stage every private destination before the database or
/// bookshelf is touched. This creates missing parent directories, validates
/// final target types, resolves parent symlinks for TLS paths, and writes the
/// new payload plus a temporary copy of any previous target beside it.
fn stage_private_destinations(
    entries: Vec<(PrivateRestoreEntry, Vec<u8>, PrivateKind)>,
) -> Result<StagedPrivateRestore, CtlError> {
    let mut staged = StagedPrivateRestore {
        entries: Vec::with_capacity(entries.len()),
    };
    let mut seen_targets = HashSet::new();

    for (mut entry, data, kind) in entries {
        let parent = entry
            .target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        let Some(parent) = parent else {
            return Err(CtlError::message(format!(
                "private target {:?} has no parent directory",
                entry.target
            )));
        };
        ensure_private_directory(parent, kind.allows_parent_symlinks())?;
        entry.target = finalize_private_target(&entry.target, kind)?;

        if !seen_targets.insert(entry.target.clone()) {
            return Err(CtlError::message(format!(
                "private manifest maps more than one file to {:?}",
                entry.target
            )));
        }

        stage_one_private_destination(&mut entry, &data)?;
        staged.entries.push(entry);
    }

    Ok(staged)
}

/// Resolve the final target path. The data-directory root has already been
/// canonicalized; data-dir payload targets must not be symlinks. Configured
/// TLS final-target symlinks are resolved by [`resolve_tls_target`] before
/// this point, and TLS intermediate parent symlinks are handled by
/// [`ensure_private_directory`].
fn finalize_private_target(target: &Path, kind: PrivateKind) -> Result<PathBuf, CtlError> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        CtlError::message(format!(
            "failed to resolve private target parent {:?}: {error}",
            parent
        ))
    })?;
    let file_name = target.file_name().ok_or_else(|| {
        CtlError::message(format!("private target {:?} has no file name", target))
    })?;
    let candidate = canonical_parent.join(file_name);

    match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if matches!(kind, PrivateKind::DataDir) {
                Err(CtlError::message(format!(
                    "private data-directory target {:?} is a symlink; refusing to replace it",
                    candidate
                )))
            } else {
                let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
                    CtlError::message(format!(
                        "configured TLS target symlink {:?} does not resolve: {error}",
                        candidate
                    ))
                })?;
                let resolved_metadata = std::fs::metadata(&resolved).map_err(|error| {
                    CtlError::message(format!(
                        "failed to inspect configured TLS target {:?}: {error}",
                        resolved
                    ))
                })?;
                if !resolved_metadata.is_file() {
                    return Err(CtlError::message(format!(
                        "configured TLS target {:?} resolves to a non-file",
                        candidate
                    )));
                }
                Ok(resolved)
            }
        }
        Ok(metadata) if metadata.is_file() => {
            Ok(std::fs::canonicalize(&candidate).map_err(|error| {
                CtlError::message(format!(
                    "failed to resolve private target {:?}: {error}",
                    candidate
                ))
            })?)
        }
        Ok(metadata) if metadata.is_dir() => Err(CtlError::message(format!(
            "private target {:?} is a directory; refusing to replace it",
            candidate
        ))),
        Ok(_) => Err(CtlError::message(format!(
            "private target {:?} is not a regular file",
            candidate
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(candidate),
        Err(error) => Err(CtlError::message(format!(
            "failed to inspect private target {:?}: {error}",
            candidate
        ))),
    }
}

fn stage_one_private_destination(
    entry: &mut PrivateRestoreEntry,
    data: &[u8],
) -> Result<(), CtlError> {
    let temporary = sidecar(
        &entry.target,
        &format!(".restore-private-{}", Uuid::new_v4().simple()),
    );
    if let Err(error) = write_new_file_mode(&temporary, data, entry.mode) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }

    match std::fs::symlink_metadata(&entry.target) {
        Ok(metadata) if metadata.is_file() => {
            let old_data = match read_private_bytes(&entry.target, false) {
                Ok(data) => data,
                Err(error) => {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error);
                }
            };
            let backup = sidecar(
                &entry.target,
                &format!(".restore-private-backup-{}", Uuid::new_v4().simple()),
            );
            if let Err(error) = write_new_file_mode(&backup, &old_data, PRIVATE_MODE) {
                let _ = std::fs::remove_file(&temporary);
                let _ = std::fs::remove_file(&backup);
                return Err(error);
            }
            entry.old_mode = Some(file_mode(&metadata));
            entry.backup = Some(backup);
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let _ = std::fs::remove_file(&temporary);
            return Err(CtlError::message(format!(
                "private target {:?} is a symlink; refusing to replace it",
                entry.target
            )));
        }
        Ok(metadata) if metadata.is_dir() => {
            let _ = std::fs::remove_file(&temporary);
            return Err(CtlError::message(format!(
                "private target {:?} is a directory; refusing to replace it",
                entry.target
            )));
        }
        Ok(_) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(CtlError::message(format!(
                "private target {:?} is not a regular file",
                entry.target
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(CtlError::message(format!(
                "failed to inspect private target {:?}: {error}",
                entry.target
            )));
        }
    }

    entry.temporary = temporary;
    Ok(())
}

fn file_mode(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        PRIVATE_MODE
    }
}

fn canonical_data_dir(context: &Context) -> Result<PathBuf, CtlError> {
    context.prepare_data_dir()?;
    let canonical = std::fs::canonicalize(&context.data_dir).map_err(|error| {
        CtlError::message(format!(
            "failed to resolve data directory {:?}: {error}",
            context.data_dir
        ))
    })?;
    let metadata = std::fs::metadata(&canonical).map_err(|error| {
        CtlError::message(format!(
            "failed to inspect data directory {:?}: {error}",
            canonical
        ))
    })?;
    if !metadata.is_dir() {
        return Err(CtlError::message(format!(
            "data directory {:?} is not a directory",
            canonical
        )));
    }
    Ok(canonical)
}

/// Resolve a configured TLS target. A final-target symlink (for example a
/// Let's Encrypt `live/.../cert.pem` link into `archive/...`) is allowed and
/// replaced at its canonical regular-file target, preserving the symlink.
fn resolve_tls_target(context: &Context, kind: PrivateKind) -> Result<PathBuf, CtlError> {
    let configured = configured_tls_path(context, kind)?;
    match std::fs::symlink_metadata(&configured) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let resolved = std::fs::canonicalize(&configured).map_err(|error| {
                CtlError::message(format!(
                    "configured TLS target symlink {:?} does not resolve: {error}",
                    configured
                ))
            })?;
            let metadata = std::fs::metadata(&resolved).map_err(|error| {
                CtlError::message(format!(
                    "failed to inspect configured TLS target {:?}: {error}",
                    resolved
                ))
            })?;
            if !metadata.is_file() {
                return Err(CtlError::message(format!(
                    "configured TLS target {:?} resolves to a non-file",
                    configured
                )));
            }
            Ok(resolved)
        }
        Ok(metadata) if metadata.is_file() => Ok(configured),
        Ok(metadata) if metadata.is_dir() => Err(CtlError::message(format!(
            "configured TLS target {:?} is a directory",
            configured
        ))),
        Ok(_) => Err(CtlError::message(format!(
            "configured TLS target {:?} is not a regular file",
            configured
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(configured),
        Err(error) => Err(CtlError::message(format!(
            "failed to inspect configured TLS target {:?}: {error}",
            configured
        ))),
    }
}

fn manifest_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, CtlError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| CtlError::message(format!("private manifest entry has no string {key}")))
}

fn private_manifest_file_name(value: &str) -> Option<&str> {
    let path = Path::new(value);
    let components = path.components().collect::<Vec<_>>();
    if components.len() != 3
        || components[0].as_os_str() != "private"
        || components[1].as_os_str() != "files"
    {
        return None;
    }
    match components[2] {
        Component::Normal(name) => name.to_str(),
        _ => None,
    }
}

fn configured_tls_path(context: &Context, kind: PrivateKind) -> Result<PathBuf, CtlError> {
    let (label, value) = match kind {
        PrivateKind::TlsCertificate => {
            ("server.tls.cert_path", &context.config.server.tls.cert_path)
        }
        PrivateKind::TlsPrivateKey => ("server.tls.key_path", &context.config.server.tls.key_path),
        PrivateKind::DataDir => unreachable!("data-dir entries do not use configured TLS paths"),
    };
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            CtlError::message(format!(
                "backup contains private TLS material but {label} is not configured; set it in the configuration used for restore"
            ))
        })
}

fn validate_private_files_directory(
    backup: &Path,
    referenced: &HashSet<String>,
) -> Result<(), CtlError> {
    let files_root = backup.join(PRIVATE_FILES_DIR);
    match std::fs::symlink_metadata(&files_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(CtlError::message(format!(
                "backup {PRIVATE_FILES_DIR} is a symlink; refusing to restore it"
            )));
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(CtlError::message(format!(
                "backup {PRIVATE_FILES_DIR} is not a directory"
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if referenced.is_empty() {
                return Ok(());
            }
            return Err(CtlError::message(format!(
                "backup {PRIVATE_FILES_DIR} directory is missing"
            )));
        }
        Err(error) => return Err(error.into()),
    }

    let mut seen = HashSet::new();
    for entry in std::fs::read_dir(&files_root)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| {
                CtlError::message("backup private files directory contains a non-UTF-8 name")
            })?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(CtlError::message(format!(
                "backup private file {:?} is a symlink; refusing to restore it",
                entry.path()
            )));
        }
        if !metadata.is_file() {
            return Err(CtlError::message(format!(
                "backup private file {:?} is not a regular file",
                entry.path()
            )));
        }
        let path = format!("{PRIVATE_FILES_DIR}/{name}");
        if !referenced.contains(&path) {
            return Err(CtlError::message(format!(
                "backup contains unreferenced private file {path:?}"
            )));
        }
        seen.insert(path);
    }
    for path in referenced {
        if !seen.contains(path) {
            return Err(CtlError::message(format!(
                "private manifest references missing file {path:?}"
            )));
        }
    }
    Ok(())
}

fn read_regular_private_file(path: &Path) -> Result<Vec<u8>, CtlError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        CtlError::message(format!(
            "failed to inspect private payload {path:?}: {error}"
        ))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(CtlError::message(format!(
            "refusing to restore symlinked private file {path:?}"
        )));
    }
    if !metadata.is_file() {
        return Err(CtlError::message(format!(
            "private file {path:?} is not a regular file"
        )));
    }
    if metadata.len() > MAX_PRIVATE_FILE_BYTES {
        return Err(CtlError::message(format!(
            "private file {path:?} exceeds the {MAX_PRIVATE_FILE_BYTES}-byte file limit"
        )));
    }
    read_private_bytes(path, false)
}

/// Read a private file. When `follow_symlinks` is false, `O_NOFOLLOW` closes
/// the check-then-open race for data-directory candidates and restore payloads;
/// `O_NONBLOCK` keeps a swapped-in FIFO from blocking before the metadata check
/// rejects it.
fn read_private_bytes(path: &Path, follow_symlinks: bool) -> Result<Vec<u8>, CtlError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if !follow_symlinks {
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
    }
    #[cfg(not(unix))]
    let _ = follow_symlinks;

    let mut file = options.open(path).map_err(|error| {
        CtlError::message(format!("failed to open private file {path:?}: {error}"))
    })?;
    let metadata = file.metadata().map_err(|error| {
        CtlError::message(format!(
            "failed to inspect open private file {path:?}: {error}"
        ))
    })?;
    if !metadata.is_file() {
        return Err(CtlError::message(format!(
            "private file {path:?} is not a regular file"
        )));
    }
    if metadata.len() > MAX_PRIVATE_FILE_BYTES {
        return Err(CtlError::message(format!(
            "private file {path:?} exceeds the {MAX_PRIVATE_FILE_BYTES}-byte file limit"
        )));
    }
    let mut data = Vec::new();
    file.read_to_end(&mut data).map_err(|error| {
        CtlError::message(format!("failed to read private file {path:?}: {error}"))
    })?;
    Ok(data)
}

/// Commit staged private files as a set. On an ordinary commit failure, files
/// already applied in this run are restored from their temporary backups.
/// The message reports how many of how many were applied and which target
/// failed; an unsafe rollback is reported explicitly rather than claimed.
fn apply_private_restore(staged: StagedPrivateRestore) -> Result<usize, CtlError> {
    let total = staged.entries.len();
    for (index, entry) in staged.entries.iter().enumerate() {
        #[cfg(windows)]
        if entry.target.exists() {
            if let Err(error) = std::fs::remove_file(&entry.target) {
                return Err(private_apply_error(
                    index,
                    total,
                    entry,
                    &error,
                    &staged.entries,
                ));
            }
        }
        if let Err(error) = std::fs::rename(&entry.temporary, &entry.target) {
            return Err(private_apply_error(
                index,
                total,
                entry,
                &error,
                &staged.entries,
            ));
        }
        if let Err(error) = set_mode(&entry.target, entry.mode) {
            let error = std::io::Error::other(error.to_string());
            return Err(private_apply_error(
                index,
                total,
                entry,
                &error,
                &staged.entries,
            ));
        }
        if let Some(backup) = &entry.backup {
            let _ = std::fs::remove_file(backup);
        }
    }
    Ok(total)
}

fn private_apply_error(
    index: usize,
    total: usize,
    entry: &PrivateRestoreEntry,
    error: &std::io::Error,
    entries: &[PrivateRestoreEntry],
) -> CtlError {
    let rollback_errors = rollback_private_entries(&entries[..=index]);
    let rollback = if rollback_errors.is_empty() {
        "rolled back the private files already applied in this run".to_string()
    } else {
        format!(
            "rollback failed for {}: {}; partial private state may remain",
            rollback_errors.len(),
            rollback_errors.join("; ")
        )
    };
    CtlError::message(format!(
        "applied {index} of {total} private payload file(s); failed while applying {:?}: {error}; {rollback}",
        entry.target
    ))
}

fn rollback_private_entries(entries: &[PrivateRestoreEntry]) -> Vec<String> {
    let mut errors = Vec::new();
    for entry in entries.iter().rev() {
        match &entry.backup {
            Some(backup) => {
                #[cfg(windows)]
                {
                    let _ = std::fs::remove_file(&entry.target);
                }
                if let Err(error) = std::fs::rename(backup, &entry.target) {
                    errors.push(format!("{:?}: {error}", entry.target));
                    continue;
                }
                if let Some(mode) = entry.old_mode {
                    if let Err(error) = set_mode(&entry.target, mode) {
                        errors.push(format!("{:?} mode: {error}", entry.target));
                    }
                }
            }
            None => {
                if let Err(error) = std::fs::remove_file(&entry.target) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        errors.push(format!("{:?}: {error}", entry.target));
                    }
                }
            }
        }
    }
    errors
}

/// Create missing parent directories with mode 0700. Existing directories are
/// left alone. A symlink component is followed only for explicitly configured
/// TLS paths; data-directory private writes refuse it.
fn ensure_private_directory(path: &Path, allow_symlinks: bool) -> Result<(), CtlError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if allow_symlinks && std::fs::metadata(path).is_ok_and(|target| target.is_dir()) {
                return Ok(());
            }
            Err(CtlError::message(format!(
                "private target parent {} is a symlink; refusing to follow it",
                path.display()
            )))
        }
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(CtlError::message(format!(
            "private target parent {} is not a directory",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    ensure_private_directory(parent, allow_symlinks)?;
                }
            }
            std::fs::create_dir(path)?;
            set_mode(path, PRIVATE_DIRECTORY_MODE)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Extract a backup tar archive into a staging directory.
///
/// Only the files this command writes are accepted: the database snapshot or
/// dump, the `bookshelf/` tree, and the fixed `private/manifest.json` +
/// `private/files/<file>` layout. Absolute paths, `..`, symlinks, hard links,
/// devices, FIFOs, and unknown entries are rejected, so a malformed or hostile
/// archive cannot write outside the staging directory or plant a link that a
/// later live-data replacement would follow. Private entry sizes are checked
/// from the tar header before any payload is extracted.
fn extract_tar(path: &Path, destination: &Path) -> Result<(), CtlError> {
    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(file);
    let mut saw_payload = false;
    let mut private_total = 0_u64;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.into_owned();
        if !allowed_tar_entry(&entry_path) {
            return Err(CtlError::message(format!(
                "backup archive contains an unexpected entry {entry_path:?}; expected {}, {}, {}/, {PRIVATE_MANIFEST}, or {PRIVATE_FILES_DIR}/<file>",
                DATABASE_FILE,
                POSTGRES_DUMP_FILE,
                BOOKSHELF_DIR
            )));
        }

        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(CtlError::message(format!(
                "backup archive entry {entry_path:?} is not a regular file or directory"
            )));
        }

        if entry_type.is_file()
            && (is_private_tar_payload(&entry_path) || entry_path == Path::new(PRIVATE_MANIFEST))
        {
            let size = entry.header().size()?;
            if size > MAX_PRIVATE_FILE_BYTES {
                return Err(CtlError::message(format!(
                    "backup archive private entry {entry_path:?} exceeds the {MAX_PRIVATE_FILE_BYTES}-byte file limit"
                )));
            }
            private_total = private_total.saturating_add(size);
            if private_total > MAX_PRIVATE_TOTAL_BYTES {
                return Err(CtlError::message(format!(
                    "backup archive private entries exceed the {MAX_PRIVATE_TOTAL_BYTES}-byte total limit"
                )));
            }
        }

        if !entry.unpack_in(destination)? {
            return Err(CtlError::message(format!(
                "backup archive entry {entry_path:?} escapes the extraction directory"
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
    if path == Path::new(DATABASE_FILE)
        || path == Path::new(POSTGRES_DUMP_FILE)
        || path.starts_with(BOOKSHELF_DIR)
    {
        return true;
    }

    // The private subtree has a fixed layout: a manifest plus regular payload
    // files directly under private/files/. Nested private entries stay
    // rejected so extraction cannot create a tree a later write would follow.
    if path == Path::new(PRIVATE_DIR)
        || path == Path::new(PRIVATE_MANIFEST)
        || path == Path::new(PRIVATE_FILES_DIR)
    {
        return true;
    }
    is_private_tar_payload(path)
}

fn is_private_tar_payload(path: &Path) -> bool {
    match path.strip_prefix(PRIVATE_FILES_DIR) {
        Ok(relative) => {
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                && relative.components().count() == 1
        }
        Err(_) => false,
    }
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

#[cfg(test)]
mod tests {
    use super::{
        allowed_tar_entry, archive_database, data_dir_file_is_private, database_names_match,
        rollback_private_entries, PrivateRestoreEntry, DATA_DIR_PRIVATE_FILES,
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
        assert!(allowed_tar_entry(Path::new("bookshelf")));
        assert!(allowed_tar_entry(Path::new(
            "bookshelf/organization-abc/checksum-def"
        )));
        assert!(allowed_tar_entry(Path::new("private")));
        assert!(allowed_tar_entry(Path::new("private/manifest.json")));
        assert!(allowed_tar_entry(Path::new("private/files")));
        assert!(allowed_tar_entry(Path::new("private/files/pivotal.pem")));

        assert!(!allowed_tar_entry(Path::new("private/other.json")));
        assert!(!allowed_tar_entry(Path::new("private/files/nested/key")));
        assert!(!allowed_tar_entry(Path::new("../microwave.db")));
        assert!(!allowed_tar_entry(Path::new(
            "bookshelf/../../microwave.db"
        )));
        assert!(!allowed_tar_entry(Path::new("/etc/passwd")));
        assert!(!allowed_tar_entry(Path::new("pivotal.pem")));
        assert!(!allowed_tar_entry(Path::new("")));
    }

    #[test]
    fn private_allowlist_matches_the_migration_source_allowlist() {
        const MIGRATE_ARCHIVE: &str = include_str!("../../microwave-migrate/src/archive.rs");
        let start = MIGRATE_ARCHIVE
            .find("const DATA_DIR_PRIVATE_FILES")
            .expect("migrate data-dir allowlist");
        let block = &MIGRATE_ARCHIVE[start..];
        let block = &block[..block.find("];").expect("allowlist terminator")];
        let migrate = block.split('"').skip(1).step_by(2).collect::<Vec<_>>();
        assert_eq!(migrate, DATA_DIR_PRIVATE_FILES);
    }

    #[test]
    fn private_allowlist_matches_the_migration_data_dir_set() {
        let expected = [
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
        assert_eq!(DATA_DIR_PRIVATE_FILES, expected);
        for public in ["webui_pub.pem", "oc_id_pub.pem", "ocid_pub.pem"] {
            assert!(!data_dir_file_is_private(public), "{public}");
        }
        for private in expected {
            if !matches!(private, "webui_pub.pem" | "oc_id_pub.pem" | "ocid_pub.pem") {
                assert!(data_dir_file_is_private(private), "{private}");
            }
        }
    }

    #[test]
    fn private_rollback_restores_overwritten_files_and_removes_new_ones() {
        let directory = tempfile::tempdir().unwrap();
        let overwritten = directory.path().join("overwritten.pem");
        let backup = directory.path().join("overwritten.bak");
        std::fs::write(&overwritten, b"new").unwrap();
        std::fs::write(&backup, b"old").unwrap();
        let created = directory.path().join("created.pem");
        std::fs::write(&created, b"created").unwrap();
        let entries = vec![
            PrivateRestoreEntry {
                target: overwritten.clone(),
                mode: 0o600,
                temporary: directory.path().join("ignored.tmp"),
                backup: Some(backup),
                old_mode: Some(0o640),
            },
            PrivateRestoreEntry {
                target: created.clone(),
                mode: 0o600,
                temporary: directory.path().join("ignored2.tmp"),
                backup: None,
                old_mode: None,
            },
        ];
        let errors = rollback_private_entries(&entries);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(std::fs::read(&overwritten).unwrap(), b"old");
        assert!(!created.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&overwritten)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o640
            );
        }
    }
}
