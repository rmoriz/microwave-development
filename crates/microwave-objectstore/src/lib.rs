//! Content-addressed object storage for cookbook files.
//!
//! Two backends implement [`ObjectStore`]:
//!
//! * [`LocalObjectStore`] stores blobs on the local filesystem and serves them
//!   through the in-process `/bookshelf` routes using HMAC-signed tokens.
//! * [`S3ObjectStore`] talks to any S3-compatible service (AWS S3, Garage,
//!   MinIO) using SigV4 request signing and presigned URLs.

use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use sha2::Sha256;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use thiserror::Error;

mod s3;

pub use s3::{S3Config, S3ObjectStore};

type HmacSha256 = Hmac<Sha256>;

/// Monotonic suffix that keeps concurrent health probes from sharing a path.
static HEALTH_PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Default lifetime for presigned S3 URLs, matching the upstream bookshelf default.
pub const DEFAULT_PRESIGN_EXPIRY_SECONDS: u64 = 900;

#[derive(Debug, Error)]
pub enum ObjectStoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("checksum mismatch")]
    ChecksumMismatch,
    #[error("invalid token")]
    InvalidToken,
    #[error("object not found")]
    NotFound,
    #[error("invalid object store configuration: {0}")]
    InvalidConfig(String),
    #[error("object store request failed: {0}")]
    Http(String),
    #[error("object store returned status {0}")]
    HttpStatus(u16),
}

/// Content-addressed blob store keyed by `(organization_id, checksum)`.
///
/// Implementations must be safe to share across request handlers.
pub trait ObjectStore: Send + Sync {
    /// Store `data` under the key for `checksum`, verifying the MD5 first.
    fn put(
        &self,
        organization_id: &str,
        checksum: &str,
        data: &[u8],
    ) -> Result<(), ObjectStoreError>;

    /// Read the blob for `checksum`.
    fn get(&self, organization_id: &str, checksum: &str) -> Result<Vec<u8>, ObjectStoreError>;

    /// Return whether a blob exists. Backend errors are reported as `false`.
    fn exists(&self, organization_id: &str, checksum: &str) -> bool;

    /// Delete the blob, returning whether it existed.
    fn delete(&self, organization_id: &str, checksum: &str) -> Result<bool, ObjectStoreError>;

    /// Build a URL a client can use to `method` the blob directly.
    fn presigned_url(&self, method: &str, organization_id: &str, checksum: &str) -> String;

    /// Cheap liveness probe used by `GET /_status`.
    ///
    /// Local storage checks that the data directory is readable and writable;
    /// S3-compatible storage performs a bounded bucket request. The default is
    /// a successful no-op so test doubles need only implement blob operations.
    fn health_check(&self) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

/// Key layout shared by both backends: `organization-{org}/checksum-{checksum}`.
pub fn object_key(organization_id: &str, checksum: &str) -> String {
    format!("organization-{organization_id}/checksum-{checksum}")
}

/// Filesystem path for the local backend.
pub fn object_path(root: &Path, organization_id: &str, checksum: &str) -> PathBuf {
    let key = object_key(organization_id, checksum);
    key.split('/')
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

/// Local filesystem backend used by the default single-binary deployment.
#[derive(Debug, Clone)]
pub struct LocalObjectStore {
    root: PathBuf,
    secret: Vec<u8>,
    base_url: String,
}

impl LocalObjectStore {
    pub fn new(root: impl Into<PathBuf>, secret: &str, base_url: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            secret: secret.as_bytes().to_vec(),
            base_url: base_url.into(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn token(&self, method: &str, organization_id: &str, checksum: &str) -> String {
        bookshelf_token(
            std::str::from_utf8(&self.secret).expect("secret was built from a string"),
            method,
            organization_id,
            checksum,
        )
    }

    pub fn verify(&self, method: &str, organization_id: &str, checksum: &str, token: &str) -> bool {
        bookshelf_token_matches(
            std::str::from_utf8(&self.secret).expect("secret was built from a string"),
            method,
            organization_id,
            checksum,
            token,
        )
    }

    fn path(&self, organization_id: &str, checksum: &str) -> PathBuf {
        object_path(&self.root, organization_id, checksum)
    }
}

impl ObjectStore for LocalObjectStore {
    fn put(
        &self,
        organization_id: &str,
        checksum: &str,
        data: &[u8],
    ) -> Result<(), ObjectStoreError> {
        if md5_hex(data) != checksum {
            return Err(ObjectStoreError::ChecksumMismatch);
        }

        let path = self.path(organization_id, checksum);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("part");
        std::fs::write(&temporary, data)?;
        std::fs::rename(&temporary, &path)?;
        Ok(())
    }

    fn get(&self, organization_id: &str, checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        let path = self.path(organization_id, checksum);
        if !path.is_file() {
            return Err(ObjectStoreError::NotFound);
        }
        Ok(std::fs::read(path)?)
    }

    fn exists(&self, organization_id: &str, checksum: &str) -> bool {
        self.path(organization_id, checksum).is_file()
    }

    fn delete(&self, organization_id: &str, checksum: &str) -> Result<bool, ObjectStoreError> {
        let path = self.path(organization_id, checksum);
        if path.is_file() {
            std::fs::remove_file(path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn presigned_url(&self, method: &str, organization_id: &str, checksum: &str) -> String {
        let token = self.token(method, organization_id, checksum);
        format!(
            "{}/bookshelf/organizations/{organization_id}/checksums/{checksum}?token={token}",
            self.base_url
        )
    }

    fn health_check(&self) -> Result<(), ObjectStoreError> {
        // Create the root if a fresh deployment has not written a blob yet, then
        // verify both read and write access. Each call uses a unique probe path
        // so concurrent health checks cannot delete each other's file.
        std::fs::create_dir_all(&self.root)?;
        std::fs::read_dir(&self.root)?;

        let nonce = HEALTH_PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let probe = self.root.join(format!(
            ".microwave-health-{}-{nonce}-{timestamp}",
            std::process::id()
        ));
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&probe)?;
            file.write_all(b"ok")?;
        }
        match std::fs::remove_file(&probe) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// HMAC token used by the local `/bookshelf` routes.
pub fn bookshelf_token(
    secret: &str,
    method: &str,
    organization_id: &str,
    checksum: &str,
) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(format!("{method}:{organization_id}:{checksum}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Constant-time verification of a local `/bookshelf` token.
///
/// An empty secret always fails so a missing configuration cannot become an
/// authentication bypass.
pub fn bookshelf_token_matches(
    secret: &str,
    method: &str,
    organization_id: &str,
    checksum: &str,
    token: &str,
) -> bool {
    if secret.is_empty() {
        return false;
    }
    let expected = bookshelf_token(secret, method, organization_id, checksum);
    expected.as_bytes().ct_eq(token.as_bytes()).into()
}

pub fn md5_hex(data: &[u8]) -> String {
    hex::encode(Md5::digest(data))
}

pub fn md5_base64(data: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD.encode(Md5::digest(data))
}

/// Base64-encode the raw MD5 digest represented by a hex checksum.
///
/// S3's `content-md5` header is the base64 form of the digest bytes, not of
/// the hex string. Returns `None` when the checksum is not valid hex.
pub fn checksum_md5_base64(checksum: &str) -> Option<String> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let digest = hex::decode(checksum).ok()?;
    Some(STANDARD.encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_use_the_upstream_layout() {
        assert_eq!(
            object_key("acme", "abc123"),
            "organization-acme/checksum-abc123"
        );
        assert_eq!(
            object_path(Path::new("/data"), "acme", "abc123"),
            PathBuf::from("/data/organization-acme/checksum-abc123")
        );
    }

    #[test]
    fn empty_secret_never_authenticates() {
        assert!(!bookshelf_token_matches(
            "", "GET", "acme", "abc", "anything"
        ));
    }

    #[test]
    fn local_health_check_verifies_read_and_write_access() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("bookshelf");
        let store = LocalObjectStore::new(&root, "secret", "http://localhost");

        store.health_check().unwrap();
        assert!(root.is_dir());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn local_health_check_fails_when_the_root_is_not_a_directory() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let store = LocalObjectStore::new(file.path(), "secret", "http://localhost");
        assert!(store.health_check().is_err());
    }

    #[test]
    fn concurrent_local_health_checks_use_distinct_probe_files() {
        use std::sync::{Arc, Barrier};

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("bookshelf");
        let store = LocalObjectStore::new(&root, "secret", "http://localhost");
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..10 {
                        store.health_check().unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
}
