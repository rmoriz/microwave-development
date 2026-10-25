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
pub fn export_postgres(database_url: &str, output: &Path) -> Result<(), MigrateError> {
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
pub fn import_postgres(database_url: &str, input: &Path) -> Result<(), MigrateError> {
    let snapshot = read_snapshot(input)?;
    let runtime = runtime()?;
    let store = runtime.block_on(PostgresStore::connect(database_url))?;
    runtime.block_on(store.import_snapshot(&snapshot.data))?;
    Ok(())
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
