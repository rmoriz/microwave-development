//! Content-addressed object storage for cookbook files.

use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use sha2::Sha256;
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error)]
pub enum ObjectStoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("checksum mismatch")]
    ChecksumMismatch,
    #[error("invalid token")]
    InvalidToken,
}

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

    pub fn put(
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

    pub fn get(&self, organization_id: &str, checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        Ok(std::fs::read(self.path(organization_id, checksum))?)
    }

    pub fn exists(&self, organization_id: &str, checksum: &str) -> bool {
        self.path(organization_id, checksum).is_file()
    }

    pub fn delete(&self, organization_id: &str, checksum: &str) -> Result<bool, ObjectStoreError> {
        let path = self.path(organization_id, checksum);
        if path.is_file() {
            std::fs::remove_file(path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn presigned_url(&self, method: &str, organization_id: &str, checksum: &str) -> String {
        let token = self.token(method, organization_id, checksum);
        format!(
            "{}/bookshelf/organizations/{organization_id}/checksums/{checksum}?token={token}",
            self.base_url
        )
    }

    pub fn token(&self, method: &str, organization_id: &str, checksum: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key");
        mac.update(format!("{method}:{organization_id}:{checksum}").as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    pub fn verify(&self, method: &str, organization_id: &str, checksum: &str, token: &str) -> bool {
        let expected = self.token(method, organization_id, checksum);
        expected.as_bytes().ct_eq(token.as_bytes()).into()
    }

    fn path(&self, organization_id: &str, checksum: &str) -> PathBuf {
        self.root
            .join(format!("organization-{organization_id}"))
            .join(format!("checksum-{checksum}"))
    }
}

pub fn md5_hex(data: &[u8]) -> String {
    hex::encode(Md5::digest(data))
}

pub fn md5_base64(data: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD.encode(Md5::digest(data))
}

pub fn object_path(root: &Path, organization_id: &str, checksum: &str) -> PathBuf {
    root.join(format!("organization-{organization_id}"))
        .join(format!("checksum-{checksum}"))
}
