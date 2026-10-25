//! Import server-side private material that a `knife ec backup` repository
//! cannot contain.
//!
//! Chef Infra Server keeps client and user private keys on the client side;
//! the server only stores public keys. A legacy server installation does have
//! server-owned files such as `pivotal.pem`, `pivotal.password`,
//! `webui_priv.pem`, `objectstore.secret`, `private-chef-secrets.json`, and
//! TLS certificate/key material. This module copies those files from an
//! explicitly supplied directory or `.tar`/`.tar.gz` archive.
//!
//! Nothing here runs unless a secrets source is supplied. File contents are
//! never written to logs or error messages; plans expose only target paths and
//! modes.

use crate::MigrateError;
use flate2::read::MultiGzDecoder;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use tar::Archive;

const PRIVATE_MODE: u32 = 0o600;
const PUBLIC_MODE: u32 = 0o644;
const DIRECTORY_MODE: u32 = 0o700;
const MAX_SECRET_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SECRET_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SECRET_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SECRET_ENTRIES: usize = 4096;

const BASE_FILES: [&str; 6] = [
    "pivotal.pem",
    "pivotal.password",
    "webui_priv.pem",
    "webui_pub.pem",
    "objectstore.secret",
    "private-chef-secrets.json",
];
const TLS_CERT_CANDIDATES: [&str; 2] = ["ssl/cert.pem", "cert.pem"];
const TLS_KEY_CANDIDATES: [&str; 2] = ["ssl/key.pem", "key.pem"];

/// Options for [`prepare_server_secrets`].
#[derive(Debug, Clone)]
pub struct ServerSecretsOptions {
    /// Source directory. Mutually exclusive with `source_archive`.
    pub source_dir: Option<PathBuf>,
    /// Source `.tar`/`.tar.gz` archive. Mutually exclusive with `source_dir`.
    pub source_archive: Option<PathBuf>,
    /// Target data directory for default server-file paths.
    pub target_data_dir: PathBuf,
    /// Optional explicit TLS certificate target (overrides configuration).
    pub tls_cert_target: Option<PathBuf>,
    /// Optional explicit TLS private-key target (overrides configuration).
    pub tls_key_target: Option<PathBuf>,
    /// Permit replacing existing regular target files.
    pub force: bool,
}

/// One planned server-secret write. `data` is intentionally private so a
/// `Debug`/log statement cannot accidentally print secret bytes.
#[derive(Clone)]
pub struct ServerSecretWrite {
    pub target: PathBuf,
    pub mode: u32,
    pub private: bool,
    data: Vec<u8>,
}

impl ServerSecretWrite {
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl std::fmt::Debug for ServerSecretWrite {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerSecretWrite")
            .field("target", &self.target)
            .field("mode", &format_args!("{:04o}", self.mode))
            .field("private", &self.private)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// A validated set of server-secret writes.
pub struct ServerSecretsPlan {
    pub writes: Vec<ServerSecretWrite>,
    pub warnings: Vec<String>,
    force: bool,
}

impl std::fmt::Debug for ServerSecretsPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerSecretsPlan")
            .field("writes", &self.writes)
            .field("warnings", &self.warnings)
            .field("force", &self.force)
            .finish()
    }
}

impl ServerSecretsPlan {
    pub fn force(&self) -> bool {
        self.force
    }

    pub fn private_count(&self) -> usize {
        self.writes.iter().filter(|write| write.private).count()
    }

    pub fn public_count(&self) -> usize {
        self.writes.iter().filter(|write| !write.private).count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretKind {
    Base,
    TlsCert,
    TlsKey,
    Ca,
}

struct SourceSecret {
    relative: String,
    kind: SecretKind,
    data: Vec<u8>,
}

/// Read and validate a server-secrets source, plan target paths, and refuse to
/// overwrite existing files unless `force` is set. This performs reads only;
/// it never writes to the target.
pub fn prepare_server_secrets(
    options: &ServerSecretsOptions,
) -> Result<ServerSecretsPlan, MigrateError> {
    match (&options.source_dir, &options.source_archive) {
        (Some(_), Some(_)) => {
            return Err(MigrateError::ServerSecrets(
                "choose either a secrets directory or a secrets archive, not both".into(),
            ))
        }
        (None, None) => {
            return Err(MigrateError::ServerSecrets(
                "a secrets directory or archive is required".into(),
            ))
        }
        _ => {}
    }

    let mut sources = BTreeMap::new();
    let mut total = 0u64;
    if let Some(directory) = &options.source_dir {
        read_directory(directory, &mut sources, &mut total)?;
    }
    if let Some(archive) = &options.source_archive {
        read_archive(archive, &mut sources, &mut total)?;
    }
    if sources.len() > MAX_SECRET_ENTRIES {
        return Err(MigrateError::ServerSecrets(format!(
            "server secrets contain more than {MAX_SECRET_ENTRIES} entries"
        )));
    }

    let mut target_sources: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut plans: Vec<(PathBuf, u32, bool, String)> = Vec::new();

    for name in BASE_FILES {
        let relative = if name == "private-chef-secrets.json" {
            sources
                .contains_key("private-chef-secrets.json")
                .then_some("private-chef-secrets.json")
                .or_else(|| {
                    sources
                        .contains_key("private_chef_secrets.json")
                        .then_some("private_chef_secrets.json")
                })
        } else {
            sources.contains_key(name).then_some(name)
        };
        let Some(relative) = relative else {
            continue;
        };
        let target = options.target_data_dir.join(name);
        let public = name == "webui_pub.pem";
        plans.push((
            target,
            if public { PUBLIC_MODE } else { PRIVATE_MODE },
            !public,
            relative.to_string(),
        ));
    }

    let selected_cert = add_tls_plan(
        &sources,
        SecretKind::TlsCert,
        &TLS_CERT_CANDIDATES,
        options.tls_cert_target.as_deref(),
        &options.target_data_dir,
        false,
        &mut plans,
    );
    let selected_key = add_tls_plan(
        &sources,
        SecretKind::TlsKey,
        &TLS_KEY_CANDIDATES,
        options.tls_key_target.as_deref(),
        &options.target_data_dir,
        true,
        &mut plans,
    );

    for source in sources.values() {
        let extra_nginx_candidate = source.relative.starts_with("nginx/ca/")
            && match source.kind {
                SecretKind::TlsCert => selected_cert.as_deref() != Some(&source.relative),
                SecretKind::TlsKey => selected_key.as_deref() != Some(&source.relative),
                _ => false,
            };
        if source.kind != SecretKind::Ca && !extra_nginx_candidate {
            continue;
        }
        let relative = safe_relative_path(Path::new(&source.relative)).map_err(|message| {
            MigrateError::ServerSecrets(format!(
                "unsafe CA certificate path {:?}: {message}",
                source.relative
            ))
        })?;
        let private =
            source.kind == SecretKind::TlsKey || ca_file_is_private(&source.relative, &source.data);
        plans.push((
            options.target_data_dir.join(relative),
            if private { PRIVATE_MODE } else { PUBLIC_MODE },
            private,
            source.relative.clone(),
        ));
    }

    let mut writes = Vec::new();
    let mut conflicts = Vec::new();
    for (target, mode, private, relative) in plans {
        if target_sources.insert(target.clone(), relative).is_some() {
            return Err(MigrateError::ServerSecrets(format!(
                "multiple secret sources map to {}",
                target.display()
            )));
        }
        validate_target(&target, options.force, &mut conflicts)?;
        let data = sources
            .get(
                target_sources
                    .get(&target)
                    .expect("target source was just inserted")
                    .as_str(),
            )
            .map(|source| source.data.clone())
            .ok_or_else(|| {
                MigrateError::ServerSecrets(format!(
                    "internal error resolving source for {}",
                    target.display()
                ))
            })?;
        writes.push(ServerSecretWrite {
            target,
            mode,
            private,
            data,
        });
    }
    if !conflicts.is_empty() {
        return Err(MigrateError::ServerSecrets(format!(
            "target files already exist; re-run with --force to overwrite: {}",
            conflicts
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if writes.is_empty() {
        return Err(MigrateError::ServerSecrets(
            "no allowlisted server secret files were found".into(),
        ));
    }

    writes.sort_by(|left, right| left.target.cmp(&right.target));
    Ok(ServerSecretsPlan {
        writes,
        warnings: Vec::new(),
        force: options.force,
    })
}

/// Apply a prepared plan atomically per file. Parent directories are created
/// with mode 0700; private files are 0600 and public material is 0644.
pub fn apply_server_secrets(plan: &ServerSecretsPlan) -> Result<usize, MigrateError> {
    let mut staged = Vec::with_capacity(plan.writes.len());
    for write in &plan.writes {
        match stage_secret(write, plan.force) {
            Ok(staged_write) => staged.push(staged_write),
            Err(error) => {
                cleanup_staged(&staged);
                return Err(error);
            }
        }
    }

    let mut committed = Vec::with_capacity(staged.len());
    for staged_write in staged {
        match commit_staged(&staged_write) {
            Ok(()) => committed.push(staged_write),
            Err(error) => {
                let rollback = rollback_committed(&committed);
                cleanup_failed_stage(&staged_write);
                if let Err(rollback_error) = rollback {
                    return Err(MigrateError::ServerSecrets(format!(
                        "secret commit failed: {error}; rollback also failed: {rollback_error}"
                    )));
                }
                return Err(error);
            }
        }
    }

    for staged_write in &committed {
        if let Some(backup) = &staged_write.backup {
            let _ = std::fs::remove_file(backup);
        }
    }
    Ok(committed.len())
}

struct StagedSecret {
    target: PathBuf,
    temporary: PathBuf,
    backup: Option<PathBuf>,
}

fn stage_secret(write: &ServerSecretWrite, force: bool) -> Result<StagedSecret, MigrateError> {
    validate_secret_target(&write.target, force)?;
    let parent = write
        .target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_private_directory(parent)?;
    let temporary = temporary_path(&write.target, "tmp");
    if let Err(error) = write_staged_contents(&temporary, write.mode, &write.data) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    let backup = if write.target.is_file() {
        let backup = temporary_path(&write.target, "bak");
        match std::fs::copy(&write.target, &backup) {
            Ok(_) => Some(backup),
            Err(source) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(MigrateError::Write {
                    path: backup,
                    source,
                });
            }
        }
    } else {
        None
    };
    Ok(StagedSecret {
        target: write.target.clone(),
        temporary,
        backup,
    })
}

fn write_staged_contents(path: &Path, mode: u32, data: &[u8]) -> Result<(), MigrateError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let mut file = options.open(path).map_err(|source| MigrateError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(data).map_err(|source| MigrateError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| MigrateError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(
            |source| MigrateError::Write {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

fn commit_staged(staged: &StagedSecret) -> Result<(), MigrateError> {
    #[cfg(windows)]
    {
        if staged.target.exists() {
            std::fs::remove_file(&staged.target).map_err(|source| MigrateError::Write {
                path: staged.target.clone(),
                source,
            })?;
        }
        if let Err(source) = std::fs::rename(&staged.temporary, &staged.target) {
            if let Some(backup) = &staged.backup {
                let _ = std::fs::rename(backup, &staged.target);
            }
            return Err(MigrateError::Write {
                path: staged.target.clone(),
                source,
            });
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(&staged.temporary, &staged.target).map_err(|source| MigrateError::Write {
            path: staged.target.clone(),
            source,
        })
    }
}

fn rollback_committed(committed: &[StagedSecret]) -> Result<(), MigrateError> {
    for staged in committed.iter().rev() {
        match &staged.backup {
            Some(backup) => {
                let _ = std::fs::remove_file(&staged.target);
                std::fs::rename(backup, &staged.target).map_err(|source| MigrateError::Write {
                    path: staged.target.clone(),
                    source,
                })?;
            }
            None => {
                let _ = std::fs::remove_file(&staged.target);
            }
        }
    }
    Ok(())
}

fn cleanup_failed_stage(staged: &StagedSecret) {
    let _ = std::fs::remove_file(&staged.temporary);
    // If the target is missing, a Windows commit failed before restoring its
    // backup. Keep that backup as the only old copy for operator recovery.
    if staged.target.exists() {
        if let Some(backup) = &staged.backup {
            let _ = std::fs::remove_file(backup);
        }
    }
}

fn cleanup_staged(staged: &[StagedSecret]) {
    for staged in staged {
        let _ = std::fs::remove_file(&staged.temporary);
        if let Some(backup) = &staged.backup {
            let _ = std::fs::remove_file(backup);
        }
    }
}

fn validate_secret_target(target: &Path, force: bool) -> Result<(), MigrateError> {
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(MigrateError::ServerSecrets(format!(
                "secret target {} is a symlink; refusing to replace it",
                target.display()
            )))
        }
        Ok(metadata) if metadata.is_file() && !force => Err(MigrateError::ServerSecrets(format!(
            "secret target {} already exists; re-run with --force to overwrite",
            target.display()
        ))),
        Ok(metadata) if !metadata.is_file() => Err(MigrateError::ServerSecrets(format!(
            "secret target {} exists and is not a regular file",
            target.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MigrateError::Read {
            path: target.to_path_buf(),
            source,
        }),
    }
}

fn add_tls_plan(
    sources: &BTreeMap<String, SourceSecret>,
    kind: SecretKind,
    preferred: &[&str],
    explicit_target: Option<&Path>,
    target_data_dir: &Path,
    private: bool,
    plans: &mut Vec<(PathBuf, u32, bool, String)>,
) -> Option<String> {
    let relative = preferred
        .iter()
        .copied()
        .find(|candidate| {
            sources
                .get(*candidate)
                .is_some_and(|source| source.kind == kind)
        })
        .or_else(|| {
            sources
                .values()
                .find(|source| source.kind == kind)
                .map(|source| source.relative.as_str())
        });
    let relative = relative?;
    let target = explicit_target
        .map(Path::to_path_buf)
        .unwrap_or_else(|| target_data_dir.join(relative));
    plans.push((
        target,
        if private { PRIVATE_MODE } else { PUBLIC_MODE },
        private,
        relative.to_string(),
    ));
    Some(relative.to_string())
}

fn read_directory(
    directory: &Path,
    sources: &mut BTreeMap<String, SourceSecret>,
    total: &mut u64,
) -> Result<(), MigrateError> {
    let metadata = std::fs::symlink_metadata(directory).map_err(|source| MigrateError::Read {
        path: directory.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(MigrateError::ServerSecrets(format!(
            "secrets directory {} is a symlink; refusing to follow it",
            directory.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(MigrateError::ServerSecrets(format!(
            "secrets source {} is not a directory",
            directory.display()
        )));
    }

    for name in BASE_FILES {
        if name == "private-chef-secrets.json" {
            if add_root_file(
                directory,
                "private-chef-secrets.json",
                SecretKind::Base,
                sources,
                total,
            )? {
                continue;
            }
            add_root_file(
                directory,
                "private_chef_secrets.json",
                SecretKind::Base,
                sources,
                total,
            )?;
            continue;
        }
        add_root_file(directory, name, SecretKind::Base, sources, total)?;
    }
    for candidate in TLS_CERT_CANDIDATES {
        if add_root_file(directory, candidate, SecretKind::TlsCert, sources, total)? {
            break;
        }
    }
    for candidate in TLS_KEY_CANDIDATES {
        if add_root_file(directory, candidate, SecretKind::TlsKey, sources, total)? {
            break;
        }
    }

    for ca_dir in ["ca", "nginx/ca"] {
        let path = directory.join(ca_dir);
        validate_source_ancestors(directory, &path)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MigrateError::ServerSecrets(format!(
                    "CA directory {} is a symlink; refusing to follow it",
                    path.display()
                )))
            }
            Ok(metadata) if metadata.is_dir() => {
                scan_ca_directory(&path, ca_dir, sources, total)?;
            }
            Ok(_) => {
                return Err(MigrateError::ServerSecrets(format!(
                    "CA path {} is not a directory",
                    path.display()
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(MigrateError::Read {
                    path,
                    source: error,
                })
            }
        }
    }
    Ok(())
}

fn check_secret_size(path: &Path, size: u64, total: u64) -> Result<(), MigrateError> {
    if size > MAX_SECRET_FILE_BYTES {
        return Err(MigrateError::ServerSecrets(format!(
            "secret source {} exceeds the {MAX_SECRET_FILE_BYTES}-byte file limit",
            path.display()
        )));
    }
    if total.saturating_add(size) > MAX_SECRET_TOTAL_BYTES {
        return Err(MigrateError::ServerSecrets(format!(
            "server secrets exceed the {MAX_SECRET_TOTAL_BYTES}-byte total limit"
        )));
    }
    Ok(())
}

/// Return true when the file exists and was added.
fn add_root_file(
    directory: &Path,
    relative: &str,
    kind: SecretKind,
    sources: &mut BTreeMap<String, SourceSecret>,
    total: &mut u64,
) -> Result<bool, MigrateError> {
    let path = directory.join(relative);
    validate_source_ancestors(directory, &path)?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(MigrateError::ServerSecrets(format!(
                "secret source {} is a symlink; refusing to follow it",
                path.display()
            )))
        }
        Ok(metadata) if metadata.is_file() => {
            if sources.len() >= MAX_SECRET_ENTRIES {
                return Err(MigrateError::ServerSecrets(format!(
                    "server secrets contain more than {MAX_SECRET_ENTRIES} entries"
                )));
            }
            check_secret_size(&path, metadata.len(), *total)?;
            let data = std::fs::read(&path).map_err(|source| MigrateError::Read {
                path: path.clone(),
                source,
            })?;
            *total = total.saturating_add(metadata.len());
            sources.insert(
                relative.to_string(),
                SourceSecret {
                    relative: relative.to_string(),
                    kind,
                    data,
                },
            );
            Ok(true)
        }
        Ok(_) => Err(MigrateError::ServerSecrets(format!(
            "secret source {} is not a regular file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(MigrateError::Read { path, source }),
    }
}

fn scan_ca_directory(
    root: &Path,
    relative_prefix: &str,
    sources: &mut BTreeMap<String, SourceSecret>,
    total: &mut u64,
) -> Result<(), MigrateError> {
    let mut entries = std::fs::read_dir(root)
        .map_err(|source| MigrateError::Read {
            path: root.to_path_buf(),
            source,
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|source| MigrateError::Read {
                    path: root.to_path_buf(),
                    source,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();

    for path in entries {
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| MigrateError::Read {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(MigrateError::ServerSecrets(format!(
                "CA source {} is a symlink; refusing to follow it",
                path.display()
            )));
        }
        if metadata.is_dir() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            scan_ca_directory(&path, &format!("{relative_prefix}/{name}"), sources, total)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(MigrateError::ServerSecrets(format!(
                "CA source {} is not a regular file",
                path.display()
            )));
        }
        if sources.len() >= MAX_SECRET_ENTRIES {
            return Err(MigrateError::ServerSecrets(format!(
                "server secrets contain more than {MAX_SECRET_ENTRIES} entries"
            )));
        }
        check_secret_size(&path, metadata.len(), *total)?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let relative = format!("{relative_prefix}/{name}");
        let data = std::fs::read(&path).map_err(|source| MigrateError::Read {
            path: path.clone(),
            source,
        })?;
        *total = total.saturating_add(metadata.len());
        let kind = classify_ca_file(relative_prefix, &name);
        sources.insert(
            relative.clone(),
            SourceSecret {
                relative,
                kind,
                data,
            },
        );
    }
    Ok(())
}

fn read_archive(
    archive: &Path,
    sources: &mut BTreeMap<String, SourceSecret>,
    total: &mut u64,
) -> Result<(), MigrateError> {
    let metadata = std::fs::symlink_metadata(archive).map_err(|source| MigrateError::Read {
        path: archive.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(MigrateError::ServerSecrets(format!(
            "secrets archive {} is a symlink; refusing to follow it",
            archive.display()
        )));
    }
    if !metadata.is_file() {
        return Err(MigrateError::ServerSecrets(format!(
            "secrets archive {} is not a regular file",
            archive.display()
        )));
    }
    if metadata.len() > MAX_SECRET_ARCHIVE_BYTES {
        return Err(MigrateError::ServerSecrets(format!(
            "secrets archive {} exceeds the {MAX_SECRET_ARCHIVE_BYTES}-byte archive limit",
            archive.display()
        )));
    }
    let file = std::fs::File::open(archive).map_err(|source| MigrateError::Read {
        path: archive.to_path_buf(),
        source,
    })?;
    let gzip = archive
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|name| name.ends_with(".tar.gz") || name.ends_with(".tgz"));
    let reader: Box<dyn Read> = if gzip {
        Box::new(MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut tar_archive = Archive::new(reader);
    let mut seen = HashSet::new();
    let entries = tar_archive.entries().map_err(|error| {
        MigrateError::ServerSecrets(format!(
            "cannot read secrets archive {}: {error}",
            archive.display()
        ))
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|error| {
            MigrateError::ServerSecrets(format!("invalid secrets archive entry: {error}"))
        })?;
        let path = entry.path().map_err(|error| {
            MigrateError::ServerSecrets(format!("invalid secrets archive path: {error}"))
        })?;
        let relative = safe_relative_path(&path).map_err(|message| {
            MigrateError::ServerSecrets(format!(
                "unsafe secrets archive path {:?}: {message}",
                path.display().to_string()
            ))
        })?;
        let entry_type = entry.header().entry_type();
        let relative = strip_legacy_archive_prefix(&relative, entry_type.is_dir());
        if relative.is_empty() {
            if entry_type.is_dir() {
                // Archives created with `tar -C DIR .` commonly contain a
                // single root directory entry; it has no target of its own.
                continue;
            }
            return Err(MigrateError::ServerSecrets(
                "secrets archive contains an empty path".into(),
            ));
        }
        if !seen.insert(relative.clone()) {
            return Err(MigrateError::ServerSecrets(format!(
                "secrets archive contains duplicate entry {relative:?}"
            )));
        }
        if entry_type.is_dir() {
            if !allowed_archive_directory(&relative) {
                return Err(MigrateError::ServerSecrets(format!(
                    "secrets archive contains a directory outside the allowlist: {relative:?}"
                )));
            }
            continue;
        }
        if !entry_type.is_file() {
            return Err(MigrateError::ServerSecrets(format!(
                "secrets archive contains a link, device, or FIFO entry: {relative:?}"
            )));
        }
        let Some(kind) = classify_archive_file(&relative) else {
            return Err(MigrateError::ServerSecrets(format!(
                "secrets archive contains an entry outside the allowlist: {relative:?}"
            )));
        };
        if sources.len() >= MAX_SECRET_ENTRIES {
            return Err(MigrateError::ServerSecrets(format!(
                "server secrets contain more than {MAX_SECRET_ENTRIES} entries"
            )));
        }
        check_secret_size(archive, entry.size(), *total)?;
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|source| MigrateError::Read {
                path: archive.to_path_buf(),
                source,
            })?;
        *total = total.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        sources.insert(
            relative.clone(),
            SourceSecret {
                relative,
                kind,
                data,
            },
        );
    }
    Ok(())
}

fn safe_relative_path(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_string_lossy();
                if value.is_empty() {
                    return Err("empty path component".into());
                }
                parts.push(value.into_owned());
            }
            Component::CurDir => {}
            Component::ParentDir => return Err("contains '..'".into()),
            Component::RootDir => return Err("is absolute".into()),
            Component::Prefix(_) => return Err("has a platform prefix".into()),
        }
    }
    Ok(parts.join("/"))
}

fn strip_legacy_archive_prefix(relative: &str, is_dir: bool) -> String {
    if archive_root_allowed(relative) {
        return relative.to_string();
    }
    if let Some((_prefix, rest)) = relative.split_once('/') {
        if archive_root_allowed(rest) {
            return rest.to_string();
        }
        return relative.to_string();
    }
    // A single non-allowlisted component is a legacy top-level directory
    // entry. Its files are stripped when they are visited.
    if is_dir {
        String::new()
    } else {
        relative.to_string()
    }
}

fn archive_root_allowed(relative: &str) -> bool {
    if BASE_FILES.contains(&relative)
        || relative == "private_chef_secrets.json"
        || TLS_CERT_CANDIDATES.contains(&relative)
        || TLS_KEY_CANDIDATES.contains(&relative)
    {
        return true;
    }
    matches!(
        relative.split('/').next().unwrap_or_default(),
        "ca" | "nginx" | "ssl"
    )
}

fn allowed_archive_directory(relative: &str) -> bool {
    matches!(relative, "ca" | "nginx" | "nginx/ca" | "ssl")
        || relative.starts_with("ca/")
        || relative.starts_with("nginx/ca/")
}

fn classify_archive_file(relative: &str) -> Option<SecretKind> {
    if BASE_FILES.contains(&relative) || relative == "private_chef_secrets.json" {
        return Some(SecretKind::Base);
    }
    if TLS_CERT_CANDIDATES.contains(&relative) {
        return Some(SecretKind::TlsCert);
    }
    if TLS_KEY_CANDIDATES.contains(&relative) {
        return Some(SecretKind::TlsKey);
    }
    if let Some(rest) = relative.strip_prefix("nginx/ca/") {
        if !rest.contains('/') {
            let name = rest;
            if has_extension(name, "crt") {
                return Some(SecretKind::TlsCert);
            }
            if has_extension(name, "key") {
                return Some(SecretKind::TlsKey);
            }
        }
    }
    let ca_relative = relative
        .strip_prefix("ca/")
        .or_else(|| relative.strip_prefix("nginx/ca/"));
    if ca_relative.is_some_and(|rest| !rest.is_empty()) {
        return Some(SecretKind::Ca);
    }
    None
}

fn classify_ca_file(relative_prefix: &str, name: &str) -> SecretKind {
    if relative_prefix == "nginx/ca" {
        if has_extension(name, "crt") {
            return SecretKind::TlsCert;
        }
        if has_extension(name, "key") {
            return SecretKind::TlsKey;
        }
    }
    SecretKind::Ca
}

fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

fn ca_file_is_private(relative: &str, data: &[u8]) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    if has_extension(name, "key") {
        return true;
    }
    let text = String::from_utf8_lossy(data).to_ascii_uppercase();
    text.contains("PRIVATE KEY") || text.contains("BEGIN RSA PRIVATE KEY")
}

fn validate_source_ancestors(source_root: &Path, path: &Path) -> Result<(), MigrateError> {
    let Some(mut parent) = path.parent() else {
        return Ok(());
    };
    loop {
        if parent == source_root {
            return Ok(());
        }
        match std::fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MigrateError::ServerSecrets(format!(
                    "secret source directory {} is a symlink; refusing to follow it",
                    parent.display()
                )))
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(MigrateError::ServerSecrets(format!(
                    "secret source parent {} is not a directory",
                    parent.display()
                )))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(source) => {
                return Err(MigrateError::Read {
                    path: parent.to_path_buf(),
                    source,
                })
            }
        }
        let Some(next) = parent.parent() else {
            return Ok(());
        };
        if next.as_os_str().is_empty() {
            return Ok(());
        }
        parent = next;
    }
}

fn validate_target(
    target: &Path,
    force: bool,
    conflicts: &mut Vec<PathBuf>,
) -> Result<(), MigrateError> {
    validate_parent_chain(target)?;
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(MigrateError::ServerSecrets(format!(
                "secret target {} is a symlink; refusing to replace it",
                target.display()
            )))
        }
        Ok(metadata) if metadata.is_file() => {
            if !force {
                conflicts.push(target.to_path_buf());
            }
            Ok(())
        }
        Ok(_) => Err(MigrateError::ServerSecrets(format!(
            "secret target {} exists and is not a regular file",
            target.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MigrateError::Read {
            path: target.to_path_buf(),
            source,
        }),
    }
}

fn validate_parent_chain(target: &Path) -> Result<(), MigrateError> {
    let Some(mut parent) = target.parent() else {
        return Ok(());
    };
    while !parent.as_os_str().is_empty() {
        match std::fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MigrateError::ServerSecrets(format!(
                    "secret target parent {} is a symlink; refusing to follow it",
                    parent.display()
                )))
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(MigrateError::ServerSecrets(format!(
                    "secret target parent {} is not a directory",
                    parent.display()
                )))
            }
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(MigrateError::Read {
                    path: parent.to_path_buf(),
                    source,
                })
            }
        }
        let Some(next) = parent.parent() else {
            break;
        };
        parent = next;
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), MigrateError> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(MigrateError::ServerSecrets(format!(
                "secret target directory {} is a symlink; refusing to follow it",
                path.display()
            )))
        }
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(MigrateError::ServerSecrets(format!(
            "secret target directory {} is not a directory",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    ensure_private_directory(parent)?;
                }
            }
            std::fs::create_dir(path).map_err(|source| MigrateError::Write {
                path: path.to_path_buf(),
                source,
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(DIRECTORY_MODE))
                    .map_err(|source| MigrateError::Write {
                        path: path.to_path_buf(),
                        source,
                    })?;
            }
            Ok(())
        }
        Err(source) => Err(MigrateError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn temporary_path(target: &Path, kind: &str) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "secret".into());
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    target.with_file_name(format!(".{name}.{kind}-{}-{suffix}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::{allowed_archive_directory, safe_relative_path};
    use std::path::Path;

    #[test]
    fn normalizes_safe_archive_paths_and_rejects_escapes() {
        assert_eq!(
            safe_relative_path(Path::new("./ssl/cert.pem")).unwrap(),
            "ssl/cert.pem"
        );
        assert_eq!(
            safe_relative_path(Path::new("nginx/ca/root.pem")).unwrap(),
            "nginx/ca/root.pem"
        );
        assert!(safe_relative_path(Path::new("../escape")).is_err());
        assert!(safe_relative_path(Path::new("ca/../../escape")).is_err());
        assert!(safe_relative_path(Path::new("/etc/passwd")).is_err());
        assert_eq!(safe_relative_path(Path::new("")).unwrap(), "");
        assert_eq!(safe_relative_path(Path::new(".")).unwrap(), "");
    }

    #[test]
    fn rollback_restores_overwritten_files_and_removes_new_ones() {
        use super::{rollback_committed, StagedSecret};

        let directory = tempfile::tempdir().unwrap();
        let overwritten = directory.path().join("overwritten.pem");
        let backup = directory.path().join("overwritten.bak");
        std::fs::write(&overwritten, b"new").unwrap();
        std::fs::write(&backup, b"old").unwrap();
        let created = directory.path().join("created.pem");
        std::fs::write(&created, b"created").unwrap();
        let staged = vec![
            StagedSecret {
                target: overwritten.clone(),
                temporary: directory.path().join("ignored.tmp"),
                backup: Some(backup),
            },
            StagedSecret {
                target: created.clone(),
                temporary: directory.path().join("ignored2.tmp"),
                backup: None,
            },
        ];
        rollback_committed(&staged).unwrap();
        assert_eq!(std::fs::read(&overwritten).unwrap(), b"old");
        assert!(!created.exists());
    }

    #[test]
    fn directory_allowlist_is_narrow() {
        for allowed in ["ca", "nginx", "nginx/ca", "ssl", "ca/nested"] {
            assert!(allowed_archive_directory(allowed), "{allowed}");
        }
        for rejected in ["", "etc", "nginx/ca2", "pivotal"] {
            assert!(!allowed_archive_directory(rejected), "{rejected}");
        }
    }
}
