//! Offline migration/export-import support for Microwave Server.
//!
//! Migration moves data between local `microwave-store` SQLite databases or
//! PostgreSQL metadata stores and a live legacy Chef Infra Server through a
//! versioned JSON snapshot:
//!
//! ```text
//! microwave-migrate export --data-dir ./data --output snapshot.json
//! microwave-migrate export --database-url postgres://db/microwave --output snapshot.json
//! microwave-migrate import --data-dir ./new-data --input snapshot.json
//! microwave-migrate import --database-url postgres://db/microwave --input snapshot.json
//! microwave-migrate from-api --url https://chef.example.com \
//!     --client-name migrator --key migrator.pem --output snapshot.json
//! ```
//!
//! The live crawler signs every request with `microwave-crypto::sign_request`
//! and writes the same snapshot envelope as a local export.

use chrono::{SecondsFormat, Utc};
use microwave_store::{SqliteStore, StoreError, StoreSnapshot};
use microwave_store_postgres::PostgresStore;
use serde::{Deserialize, Serialize};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use url::Url;

mod from_api;

pub use from_api::{crawl_from_api, from_api_with_options, read_private_key, FromApiOptions};

/// Value of the snapshot envelope's `format` field.
pub const SNAPSHOT_FORMAT: &str = "microwave-server-snapshot";
/// Snapshot schema version supported by this build.
pub const SNAPSHOT_VERSION: u32 = 1;
/// Database file name used inside a data directory.
pub const DATABASE_FILE: &str = "microwave.db";

/// A versioned, full-fidelity migration snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: String,
    pub version: u32,
    pub generated_at: String,
    pub source: SnapshotSource,
    pub data: StoreSnapshot,
}

/// Where a snapshot came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotSource {
    pub kind: SnapshotSourceKind,
    pub url: Option<String>,
    pub data_dir: Option<String>,
}

impl Default for SnapshotSource {
    fn default() -> Self {
        Self {
            kind: SnapshotSourceKind::LocalStore,
            url: None,
            data_dir: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotSourceKind {
    #[default]
    LocalStore,
    LegacyApi,
}

#[derive(Debug, Error)]
pub enum MigrateError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database {} does not exist", .0.display())]
    MissingDatabase(PathBuf),
    #[error("invalid snapshot JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("unsupported snapshot format {0:?}; expected {SNAPSHOT_FORMAT:?}")]
    UnsupportedFormat(String),
    #[error("unsupported snapshot version {found}; this build supports {supported}")]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("storage error: {0}")]
    Store(#[from] StoreError),
    #[error("invalid legacy API URL {0:?}; expected an http:// or https:// URL")]
    InvalidUrl(String),
    #[error("legacy API request to {url} failed with HTTP {status}: {message}")]
    Api {
        url: String,
        status: u16,
        message: String,
    },
    #[error("legacy API request to {url} failed: {message}")]
    Transport { url: String, message: String },
    #[error("invalid response from {url}: {message}")]
    InvalidApiResponse { url: String, message: String },
    #[error("crypto error: {0}")]
    Crypto(#[from] microwave_crypto::CryptoError),
    #[error("failed to build tokio runtime: {0}")]
    Runtime(String),
    #[error("{0}")]
    Confirmation(String),
    #[error("invalid PostgreSQL database URL")]
    InvalidDatabaseUrl,
}

/// Export the store under `data_dir` to a JSON snapshot at `output`.
pub fn export(data_dir: &Path, output: &Path) -> Result<(), MigrateError> {
    export_database(&data_dir.join(DATABASE_FILE), output)
}

/// Export a specific SQLite database to a JSON snapshot.
pub fn export_database(database: &Path, output: &Path) -> Result<(), MigrateError> {
    if !database.is_file() {
        return Err(MigrateError::MissingDatabase(database.to_path_buf()));
    }

    let store = SqliteStore::open_read_only(database)?;
    let data = store.export_snapshot()?;
    let snapshot = Snapshot {
        format: SNAPSHOT_FORMAT.to_string(),
        version: SNAPSHOT_VERSION,
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        source: SnapshotSource {
            kind: SnapshotSourceKind::LocalStore,
            url: None,
            data_dir: Some(data_dir_of(database)),
        },
        data,
    };
    write_snapshot(&snapshot, output)
}

/// Import a JSON snapshot into the store under `data_dir`, creating the
/// database when needed.
pub fn import(data_dir: &Path, input: &Path) -> Result<(), MigrateError> {
    import_database(&data_dir.join(DATABASE_FILE), input)
}

/// Import a JSON snapshot into a specific SQLite database.
pub fn import_database(database: &Path, input: &Path) -> Result<(), MigrateError> {
    let snapshot = read_snapshot(input)?;
    if let Some(parent) = database.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| MigrateError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let store = SqliteStore::open(database)?;
    store.import_snapshot(&snapshot.data)?;
    Ok(())
}

/// Export a PostgreSQL metadata store to a JSON snapshot.
///
/// This connects without applying the embedded schema, so it works with a
/// read-only database role. The schema must already exist (the server creates
/// it on startup). The connection URL is never written to the snapshot.
///
/// The function is synchronous and owns a current-thread Tokio runtime. Do not
/// call it from inside an existing Tokio runtime; use the async
/// [`PostgresStore`] methods directly in that case.
pub fn export_postgres(database_url: &str, output: &Path) -> Result<(), MigrateError> {
    validate_postgres_url(database_url)?;
    let runtime = runtime()?;
    let store = runtime.block_on(PostgresStore::connect_existing(database_url))?;
    let data = runtime.block_on(store.export_snapshot())?;
    let snapshot = Snapshot {
        format: SNAPSHOT_FORMAT.to_string(),
        version: SNAPSHOT_VERSION,
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        source: SnapshotSource {
            kind: SnapshotSourceKind::LocalStore,
            url: None,
            data_dir: None,
        },
        data,
    };
    write_snapshot(&snapshot, output)
}

/// Import a JSON snapshot into a PostgreSQL metadata store.
///
/// The import runs in one database transaction and preserves source ids,
/// authz ids, timestamps, hashes, and keys. Importing the same snapshot twice
/// is idempotent.
///
/// The function is synchronous and owns a current-thread Tokio runtime. Do not
/// call it from inside an existing Tokio runtime; use the async
/// [`PostgresStore`] methods directly in that case.
pub fn import_postgres(database_url: &str, input: &Path) -> Result<(), MigrateError> {
    validate_postgres_url(database_url)?;
    let snapshot = read_snapshot(input)?;
    let runtime = runtime()?;
    let store = runtime.block_on(PostgresStore::connect(database_url))?;
    runtime.block_on(store.import_snapshot(&snapshot.data))?;
    Ok(())
}

/// Reject a malformed or non-PostgreSQL database URL without echoing it.
pub fn validate_postgres_url(database_url: &str) -> Result<(), MigrateError> {
    if !(database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")) {
        return Err(MigrateError::InvalidDatabaseUrl);
    }
    let parsed = Url::parse(database_url).map_err(|_| MigrateError::InvalidDatabaseUrl)?;
    if parsed.scheme() != "postgres" && parsed.scheme() != "postgresql" {
        return Err(MigrateError::InvalidDatabaseUrl);
    }
    Ok(())
}

/// Return a connection URL with authority and query-string passwords removed.
///
/// Used by command-line confirmation output so a `password`/`sslpassword`
/// query parameter is never echoed. Unparseable input is never echoed.
pub fn redacted_database_url(database_url: &str) -> String {
    redacted_url_with_label(database_url, "<invalid database URL>")
}

/// Return a credential-free PostgreSQL URL, or a fixed placeholder.
///
/// Use this before printing a `--database-url` target: anything that is not a
/// `postgres://`/`postgresql://` URL becomes `<invalid PostgreSQL URL>` rather
/// than being echoed.
pub fn redacted_postgres_url(database_url: &str) -> String {
    if !(database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")) {
        return "<invalid PostgreSQL URL>".to_string();
    }
    redacted_url_with_label(database_url, "<invalid PostgreSQL URL>")
}

fn redacted_url_with_label(database_url: &str, invalid: &str) -> String {
    let Ok(mut parsed) = Url::parse(database_url) else {
        return invalid.to_string();
    };
    let _ = parsed.set_password(None);
    if let Some(raw_query) = parsed.query() {
        let kept_query = strip_query_secrets(raw_query);
        if kept_query.is_empty() {
            parsed.set_query(None);
        } else {
            parsed.set_query(Some(&kept_query));
        }
    }
    parsed.to_string()
}

fn strip_query_secrets(raw_query: &str) -> String {
    raw_query
        .split('&')
        .filter(|pair| {
            let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
            let key = percent_decode_query(key);
            key != "password" && key != "sslpassword"
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_decode_query(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                decoded.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Run a PostgreSQL snapshot operation on a current-thread Tokio runtime.
///
/// The migration binary and the operational CLI stay synchronous; only these
/// backend calls cross into async.
fn runtime() -> Result<tokio::runtime::Runtime, MigrateError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MigrateError::Runtime(error.to_string()))
}

/// Crawl a live legacy Chef Infra Server into the snapshot format.
///
/// This is the compatibility entry point for callers that do not need an
/// organization filter or a non-default API version. Use
/// [`from_api_with_options`] for `--org` and `--api-version`.
pub fn from_api(
    url: &str,
    client_name: &str,
    key_path: &Path,
    output: &Path,
) -> Result<(), MigrateError> {
    from_api_with_options(&FromApiOptions::new(url, client_name, key_path, output))
}

/// Read and validate a snapshot file.
pub fn read_snapshot(input: &Path) -> Result<Snapshot, MigrateError> {
    let file = std::fs::File::open(input).map_err(|source| MigrateError::Read {
        path: input.to_path_buf(),
        source,
    })?;
    let snapshot: Snapshot =
        serde_json::from_reader(BufReader::new(file)).map_err(MigrateError::InvalidJson)?;
    validate(&snapshot)?;
    Ok(snapshot)
}

/// Validate the envelope format and version.
pub fn validate(snapshot: &Snapshot) -> Result<(), MigrateError> {
    if snapshot.format != SNAPSHOT_FORMAT {
        return Err(MigrateError::UnsupportedFormat(snapshot.format.clone()));
    }
    if snapshot.version != SNAPSHOT_VERSION {
        return Err(MigrateError::UnsupportedVersion {
            found: snapshot.version,
            supported: SNAPSHOT_VERSION,
        });
    }
    Ok(())
}

/// Write a snapshot atomically with owner-only permissions.
pub fn write_snapshot(snapshot: &Snapshot, output: &Path) -> Result<(), MigrateError> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| MigrateError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    let temporary = temporary_path(output);
    let result = (|| -> Result<(), MigrateError> {
        let file = std::fs::File::create(&temporary).map_err(|source| MigrateError::Write {
            path: temporary.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)).map_err(
                |source| MigrateError::Write {
                    path: temporary.clone(),
                    source,
                },
            )?;
        }
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, snapshot)?;
        writer
            .write_all(b"\n")
            .map_err(|source| MigrateError::Write {
                path: temporary.clone(),
                source,
            })?;
        writer.flush().map_err(|source| MigrateError::Write {
            path: temporary.clone(),
            source,
        })?;
        std::fs::rename(&temporary, output).map_err(|source| MigrateError::Write {
            path: output.to_path_buf(),
            source,
        })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "snapshot.json".into());
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    output.with_file_name(format!(".{name}.tmp-{}-{suffix}", std::process::id()))
}

fn data_dir_of(database: &Path) -> String {
    database
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{redacted_database_url, redacted_postgres_url, validate_postgres_url};

    #[test]
    fn removes_authority_and_query_passwords() {
        let redacted = redacted_database_url(
            "postgres://user:authority-secret@db/microwave?password=query-secret&sslmode=require",
        );
        assert!(!redacted.contains("authority-secret"));
        assert!(!redacted.contains("query-secret"));
        assert!(!redacted.contains("password="));
        assert!(redacted.contains("sslmode=require"));
        assert!(redacted.contains("db/microwave"));
    }

    #[test]
    fn removes_sslpassword_from_the_query() {
        let redacted = redacted_database_url(
            "postgres://host/microwave?sslpassword=ssl-secret&sslmode=verify-full",
        );
        assert!(!redacted.contains("ssl-secret"));
        assert!(!redacted.contains("sslpassword"));
        assert!(redacted.contains("sslmode=verify-full"));
    }

    #[test]
    fn preserves_raw_query_encoding() {
        let redacted = redacted_database_url(
            "postgres://host/db?options=-c%20statement_timeout%3D5s&password=secret&sslmode=require",
        );
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("password="));
        assert!(redacted.contains("options=-c%20statement_timeout%3D5s"));
        assert!(!redacted.contains('+'));
    }

    #[test]
    fn validates_postgres_urls_without_echoing_them() {
        assert!(validate_postgres_url("postgres://user:secret@db/microwave").is_ok());
        assert!(validate_postgres_url("postgresql://db/microwave").is_ok());
        assert!(validate_postgres_url("mysql://user:secret@db/microwave").is_err());
        let error = validate_postgres_url("postgres://user:secret@[::bad")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret"));
        assert!(error.contains("invalid PostgreSQL database URL"));
    }

    #[test]
    fn never_echoes_unparseable_or_non_postgres_urls() {
        let malformed = redacted_postgres_url("postgres://user:secret@[::bad");
        assert_eq!(malformed, "<invalid PostgreSQL URL>");
        assert!(!malformed.contains("secret"));

        let wrong_scheme = redacted_postgres_url("mysql://user:secret@db/microwave");
        assert_eq!(wrong_scheme, "<invalid PostgreSQL URL>");
        assert!(!wrong_scheme.contains("secret"));
    }
}
