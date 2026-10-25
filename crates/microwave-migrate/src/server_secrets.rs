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
    if let Some(directory) = &options.source_dir {
        read_directory(directory, &mut sources)?;
    }
    if let Some(archive) = &options.source_archive {
        read_archive(archive, &mut sources)?;
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

    add_tls_plan(
        &sources,
        &TLS_CERT_CANDIDATES,
        options.tls_cert_target.as_deref(),
        &options.target_data_dir,
        false,
        &mut plans,
    );
    add_tls_plan(
        &sources,
        &TLS_KEY_CANDIDATES,
        options.tls_key_target.as_deref(),
        &options.target_data_dir,
        true,
        &mut plans,
    );

    for source in sources.values() {
        if source.kind != SecretKind::Ca {
            continue;
        }
        let relative = safe_relative_path(Path::new(&source.relative)).map_err(|message| {
            MigrateError::ServerSecrets(format!(
                "unsafe CA certificate path {:?}: {message}",
                source.relative
            ))
        })?;
        plans.push((
            options.target_data_dir.join(relative),
            PUBLIC_MODE,
            false,
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
    for write in &plan.writes {
        write_secret(write, plan.force)?;
    }
    Ok(plan.writes.len())
}

fn add_tls_plan(
    sources: &BTreeMap<String, SourceSecret>,
    candidates: &[&str],
    explicit_target: Option<&Path>,
    target_data_dir: &Path,
    private: bool,
    plans: &mut Vec<(PathBuf, u32, bool, String)>,
) {
    let Some(relative) = candidates
        .iter()
        .copied()
        .find(|candidate| sources.contains_key(*candidate))
    else {
        return;
    };
    let target = explicit_target
        .map(Path::to_path_buf)
        .unwrap_or_else(|| target_data_dir.join(relative));
    plans.push((
        target,
        if private { PRIVATE_MODE } else { PUBLIC_MODE },
        private,
        relative.to_string(),
    ));
}

fn read_directory(
    directory: &Path,
    sources: &mut BTreeMap<String, SourceSecret>,
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
            )? {
                continue;
            }
            add_root_file(
                directory,
                "private_chef_secrets.json",
                SecretKind::Base,
                sources,
            )?;
            continue;
        }
        add_root_file(directory, name, SecretKind::Base, sources)?;
    }
    for candidate in TLS_CERT_CANDIDATES {
        if add_root_file(directory, candidate, SecretKind::TlsCert, sources)? {
            break;
        }
    }
    for candidate in TLS_KEY_CANDIDATES {
        if add_root_file(directory, candidate, SecretKind::TlsKey, sources)? {
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
                scan_ca_directory(&path, ca_dir, sources)?;
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

/// Return true when the file exists and was added.
fn add_root_file(
    directory: &Path,
    relative: &str,
    kind: SecretKind,
    sources: &mut BTreeMap<String, SourceSecret>,
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
            let data = std::fs::read(&path).map_err(|source| MigrateError::Read {
                path: path.clone(),
                source,
            })?;
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
            scan_ca_directory(&path, &format!("{relative_prefix}/{name}"), sources)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(MigrateError::ServerSecrets(format!(
                "CA source {} is not a regular file",
                path.display()
            )));
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let relative = format!("{relative_prefix}/{name}");
        let data = std::fs::read(&path).map_err(|source| MigrateError::Read {
            path: path.clone(),
            source,
        })?;
        sources.insert(
            relative.clone(),
            SourceSecret {
                relative,
                kind: SecretKind::Ca,
                data,
            },
        );
    }
    Ok(())
}

fn read_archive(
    archive: &Path,
    sources: &mut BTreeMap<String, SourceSecret>,
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
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|source| MigrateError::Read {
                path: archive.to_path_buf(),
                source,
            })?;
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
    let ca_relative = relative
        .strip_prefix("ca/")
        .or_else(|| relative.strip_prefix("nginx/ca/"));
    if ca_relative.is_some_and(|rest| !rest.is_empty()) {
        return Some(SecretKind::Ca);
    }
    None
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

fn write_secret(write: &ServerSecretWrite, force: bool) -> Result<(), MigrateError> {
    match std::fs::symlink_metadata(&write.target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(MigrateError::ServerSecrets(format!(
                "secret target {} is a symlink; refusing to replace it",
                write.target.display()
            )))
        }
        Ok(metadata) if metadata.is_file() && !force => {
            return Err(MigrateError::ServerSecrets(format!(
                "secret target {} already exists; re-run with --force to overwrite",
                write.target.display()
            )))
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(MigrateError::ServerSecrets(format!(
                "secret target {} exists and is not a regular file",
                write.target.display()
            )))
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(MigrateError::Read {
                path: write.target.clone(),
                source,
            })
        }
    }

    let parent = write
        .target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_private_directory(parent)?;
    let temporary = temporary_path(&write.target);
    let result = (|| -> Result<(), MigrateError> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(write.mode);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|source| MigrateError::Write {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(&write.data)
            .map_err(|source| MigrateError::Write {
                path: temporary.clone(),
                source,
            })?;
        file.sync_all().map_err(|source| MigrateError::Write {
            path: temporary.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(write.mode))
                .map_err(|source| MigrateError::Write {
                    path: temporary.clone(),
                    source,
                })?;
        }
        #[cfg(windows)]
        {
            if write.target.exists() {
                let _ = std::fs::remove_file(&write.target);
            }
        }
        std::fs::rename(&temporary, &write.target).map_err(|source| MigrateError::Write {
            path: write.target.clone(),
            source,
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
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

fn temporary_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "secret".into());
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    target.with_file_name(format!(".{name}.tmp-{}-{suffix}", std::process::id()))
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
    fn directory_allowlist_is_narrow() {
        for allowed in ["ca", "nginx", "nginx/ca", "ssl", "ca/nested"] {
            assert!(allowed_archive_directory(allowed), "{allowed}");
        }
        for rejected in ["", "etc", "nginx/ca2", "pivotal"] {
            assert!(!allowed_archive_directory(rejected), "{rejected}");
        }
    }
}
