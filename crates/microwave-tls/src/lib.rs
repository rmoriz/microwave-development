//! TLS configuration loading and validation.
//!
//! The listener stays on plain HTTP unless `server.tls` names a certificate
//! and key. Both files are read and parsed here, and rustls verifies that the
//! key matches the certificate before the server binds. Errors name the
//! offending path but never include file contents, so private key material is
//! never printed by `check-config`, `serve --check`, or the server logs.

use microwave_config::TlsConfig;
use rustls::pki_types::CertificateDer;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

/// A validated certificate/key pair plus the rustls server configuration
/// derived from it.
pub struct LoadedTls {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub config: Arc<rustls::ServerConfig>,
}

impl std::fmt::Debug for LoadedTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omit the rustls config; it contains key material.
        f.debug_struct("LoadedTls")
            .field("cert_path", &self.cert_path)
            .field("key_path", &self.key_path)
            .finish_non_exhaustive()
    }
}

/// Validate and load the configured TLS certificate and private key.
///
/// This checks that both paths are set, that the files exist and are readable
/// regular files, that they contain PEM certificates and a PEM private key,
/// and that the key belongs to the certificate. It never returns or logs the
/// key bytes.
pub fn load(tls: &TlsConfig) -> Result<LoadedTls, TlsError> {
    let cert_path = required_path(tls.cert_path.as_deref(), Setting::CertPath)?;
    let key_path = required_path(tls.key_path.as_deref(), Setting::KeyPath)?;

    let cert_pem = read_file(&cert_path, "certificate")?;
    let key_pem = read_file(&key_path, "private key")?;

    let mut cert_reader = cert_pem.as_slice();
    let certs = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<CertificateDer<'static>>, _>>()
        .map_err(|error| TlsError::Parse {
            kind: "certificate",
            path: cert_path.display().to_string(),
            reason: error.to_string(),
        })?;
    if certs.is_empty() {
        return Err(TlsError::NoCertificates {
            path: cert_path.display().to_string(),
        });
    }

    let mut key_reader = key_pem.as_slice();
    let key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|error| TlsError::Parse {
            kind: "private key",
            path: key_path.display().to_string(),
            reason: error.to_string(),
        })?
        .ok_or_else(|| TlsError::NoPrivateKey {
            path: key_path.display().to_string(),
        })?;

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| TlsError::Invalid {
            reason: error.to_string(),
        })?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(map_pair_error)?;

    Ok(LoadedTls {
        cert_path,
        key_path,
        config: Arc::new(config),
    })
}

/// Validate the pair without producing a server configuration. Used by
/// `check-config` to report a concise line.
pub fn validate(tls: &TlsConfig) -> Result<(), TlsError> {
    load(tls).map(|_loaded| ())
}

#[derive(Debug, Clone, Copy)]
enum Setting {
    CertPath,
    KeyPath,
}

impl Setting {
    fn key(self) -> &'static str {
        match self {
            Setting::CertPath => "server.tls.cert_path",
            Setting::KeyPath => "server.tls.key_path",
        }
    }
}

fn required_path(value: Option<&str>, setting: Setting) -> Result<PathBuf, TlsError> {
    match value.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => Err(TlsError::MissingSetting { key: setting.key() }),
    }
}

fn read_file(path: &Path, kind: &'static str) -> Result<Vec<u8>, TlsError> {
    let metadata = std::fs::metadata(path).map_err(|source| TlsError::Read {
        kind,
        path: path.display().to_string(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(TlsError::NotAFile {
            kind,
            path: path.display().to_string(),
        });
    }
    std::fs::read(path).map_err(|source| TlsError::Read {
        kind,
        path: path.display().to_string(),
        source,
    })
}

fn map_pair_error(error: rustls::Error) -> TlsError {
    match error {
        rustls::Error::InconsistentKeys(_) => TlsError::MismatchedKeyPair,
        other => TlsError::Invalid {
            reason: other.to_string(),
        },
    }
}

#[derive(Debug, Error)]
pub enum TlsError {
    #[error("{key} must be set when TLS is enabled")]
    MissingSetting { key: &'static str },
    #[error("failed to read TLS {kind} file {path}: {source}")]
    Read {
        kind: &'static str,
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TLS {kind} path {path} is not a regular file")]
    NotAFile { kind: &'static str, path: String },
    #[error("failed to parse TLS {kind} file {path}: {reason}")]
    Parse {
        kind: &'static str,
        path: String,
        reason: String,
    },
    #[error("no PEM certificates found in TLS certificate file {path}")]
    NoCertificates { path: String },
    #[error("no PEM private key found in TLS key file {path}")]
    NoPrivateKey { path: String },
    #[error("the TLS certificate and private key do not form a matching pair")]
    MismatchedKeyPair,
    #[error("invalid TLS configuration: {reason}")]
    Invalid { reason: String },
}

#[cfg(test)]
mod tests {
    use super::{load, validate};
    use microwave_config::TlsConfig;

    struct Pair {
        _dir: tempfile::TempDir,
        cert: std::path::PathBuf,
        key: std::path::PathBuf,
    }

    fn write_pair() -> Pair {
        let dir = tempfile::tempdir().unwrap();
        let cert = rcgen::generate_simple_self_signed(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ])
        .unwrap();
        let cert_path = dir.path().join("tls.crt");
        let key_path = dir.path().join("tls.key");
        std::fs::write(&cert_path, cert.cert.pem()).unwrap();
        std::fs::write(&key_path, cert.signing_key.serialize_pem()).unwrap();
        Pair {
            _dir: dir,
            cert: cert_path,
            key: key_path,
        }
    }

    fn config(cert: &std::path::Path, key: &std::path::Path) -> TlsConfig {
        TlsConfig {
            enabled: Some(true),
            cert_path: Some(cert.display().to_string()),
            key_path: Some(key.display().to_string()),
        }
    }

    #[test]
    fn loads_a_matching_self_signed_pair() {
        let pair = write_pair();
        let loaded = load(&config(&pair.cert, &pair.key)).unwrap();
        assert_eq!(loaded.cert_path, pair.cert);
        assert_eq!(loaded.key_path, pair.key);
    }

    #[test]
    fn rejects_missing_settings() {
        let error = load(&TlsConfig {
            enabled: Some(true),
            cert_path: None,
            key_path: Some("/tmp/key.pem".into()),
        })
        .unwrap_err();
        assert!(error.to_string().contains("server.tls.cert_path"));

        let error = load(&TlsConfig {
            enabled: Some(true),
            cert_path: Some("/tmp/cert.pem".into()),
            key_path: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("server.tls.key_path"));
    }

    #[test]
    fn rejects_missing_files_without_reading_them() {
        let error = validate(&TlsConfig {
            enabled: Some(true),
            cert_path: Some("/nonexistent/microwave/cert.pem".into()),
            key_path: Some("/nonexistent/microwave/key.pem".into()),
        })
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("failed to read TLS certificate file"));
        assert!(text.contains("/nonexistent/microwave/cert.pem"));
    }

    #[test]
    fn rejects_a_directory_instead_of_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let error = validate(&config(dir.path(), dir.path())).unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
    }

    #[test]
    fn rejects_garbage_pem() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("tls.crt");
        let key = dir.path().join("tls.key");
        std::fs::write(&cert, "not a certificate").unwrap();
        std::fs::write(&key, "not a key").unwrap();
        let error = validate(&config(&cert, &key)).unwrap_err();
        assert!(error.to_string().contains("no PEM certificates"));
    }

    #[test]
    fn rejects_a_key_that_does_not_match_the_certificate() {
        let first = write_pair();
        let second = write_pair();
        let error = validate(&config(&first.cert, &second.key)).unwrap_err();
        assert!(
            error.to_string().contains("matching pair"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn never_includes_key_material_in_errors() {
        let first = write_pair();
        let second = write_pair();
        let key_pem = std::fs::read_to_string(&second.key).unwrap();
        let secret_line = key_pem
            .lines()
            .find(|line| !line.starts_with("-----"))
            .unwrap();
        let error = validate(&config(&first.cert, &second.key)).unwrap_err();
        assert!(!error.to_string().contains(secret_line));
    }
}
