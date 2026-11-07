//! Reload the tenant routing snapshot from the configuration file.
//!
//! A reload re-runs the startup loader (config file, bookshelf secrets, the
//! local object-store secret, the tenant cross-checks) and probes the buckets
//! before the new snapshot goes live. Any rejection keeps the previous
//! snapshot. The outcome lands in a small status file so `microwave-ctl reload`
//! can wait for it.

use crate::bootstrap;
use crate::object_store::assemble;
use microwave_config::{Config, ObjectStoreBackend};
use microwave_objectstore::{verify_buckets, RoutingObjectStore};
use microwave_store::MetadataStore;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What asked for the reload. Only used for logging and the status line.
///
/// `Interval` is wired by the periodic timer; `Manual` is used by tests and
/// administrative callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Trigger {
    Sighup,
    Interval,
    Manual,
}

impl fmt::Display for Trigger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Sighup => "sighup",
            Self::Interval => "interval",
            Self::Manual => "manual",
        })
    }
}

/// A rejected reload. The message is written to the status file and returned;
/// it never carries secret material.
#[derive(Debug)]
pub struct ReloadError(String);

impl fmt::Display for ReloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ReloadError {}

fn rejected(message: impl ToString) -> ReloadError {
    ReloadError(message.to_string())
}

/// Status file written next to the data directory. The token is copied from
/// the request file so one caller can tell its own outcome from a concurrent
/// one.
const STATUS_FILE: &str = "objectstore.reload";
/// Request file `microwave-ctl reload` writes the token to.
const REQUEST_FILE: &str = "objectstore.reload.request";

/// Rebuilds and proves the routing snapshot, then swaps it in.
pub struct Reloader {
    config_path: Option<PathBuf>,
    store: Arc<dyn MetadataStore>,
    router: Arc<RoutingObjectStore>,
    data_dir: PathBuf,
    boot_id: String,
    /// Serializes reloads so a SIGHUP waits for a running one instead of
    /// racing the file reads and the snapshot swap.
    lock: tokio::sync::Mutex<()>,
}

impl Reloader {
    pub fn new(
        config_path: Option<PathBuf>,
        store: Arc<dyn MetadataStore>,
        router: Arc<RoutingObjectStore>,
        data_dir: PathBuf,
        boot_id: String,
    ) -> Self {
        Self {
            config_path,
            store,
            router,
            data_dir,
            boot_id,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Write the initial `boot` status. Failures are the caller's to log.
    pub async fn write_boot_status(&self) -> std::io::Result<()> {
        self.write_status(None, "boot", "ok", None)
    }

    /// Reload the snapshot. On any error the old snapshot stays installed and
    /// the rejection is recorded in the status file.
    pub async fn reload(&self, trigger: Trigger) -> Result<(), ReloadError> {
        let _guard = self.lock.lock().await;
        let token = self.read_token();
        let result = self.run(trigger).await;

        let (outcome, message) = match &result {
            Ok(()) => ("ok", None),
            Err(error) => ("rejected", Some(error.to_string())),
        };
        if let Err(error) = self.write_status(
            token.as_deref(),
            &trigger.to_string(),
            outcome,
            message.as_deref(),
        ) {
            tracing::error!(%error, "failed to write the object store reload status");
        }
        // The token is consumed even on rejection so the ctl never reads a
        // stale answer from a later request.
        if let Err(error) = std::fs::remove_file(self.data_dir.join(REQUEST_FILE)) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, "failed to remove the object store reload request");
            }
        }

        match result {
            Ok(()) => {
                tracing::info!(trigger = %trigger, "reloaded the object store tenant routes");
                Ok(())
            }
            Err(error) => {
                tracing::error!(trigger = %trigger, %error, "object store reload rejected; keeping the previous snapshot");
                Err(error)
            }
        }
    }

    /// The body of a reload, run under the lock.
    async fn run(&self, _trigger: Trigger) -> Result<(), ReloadError> {
        let mut config = Config::load(self.config_path.as_deref()).map_err(rejected)?;
        bootstrap::apply_bookshelf_secrets(&mut config, self.store.as_ref())
            .await
            .map_err(rejected)?;
        let secret = match config.object_store.backend {
            ObjectStoreBackend::Local => {
                let secret = bootstrap::ensure_object_store_secret(&config, self.store.as_ref())
                    .await
                    .map_err(rejected)?;
                config.object_store.secret = Some(secret.clone());
                secret
            }
            _ => config.object_store.secret.clone().unwrap_or_default(),
        };

        let snapshot = assemble(&config, self.store.as_ref(), &secret)
            .await
            .map_err(rejected)?;
        verify_buckets(&config.object_store, &snapshot)
            .await
            .map_err(rejected)?;
        self.router.install(snapshot);
        Ok(())
    }

    fn read_token(&self) -> Option<String> {
        let token = std::fs::read_to_string(self.data_dir.join(REQUEST_FILE)).ok()?;
        let token = token.trim();
        (!token.is_empty()).then(|| token.to_string())
    }

    /// Write the status through a temp file and a rename, so a poller never
    /// reads half a document.
    fn write_status(
        &self,
        token: Option<&str>,
        trigger: &str,
        outcome: &str,
        message: Option<&str>,
    ) -> std::io::Result<()> {
        let status = serde_json::json!({
            "boot_id": self.boot_id,
            "token": token,
            "trigger": trigger,
            "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "outcome": outcome,
            "message": message,
        });
        write_atomic(
            &self.data_dir.join(STATUS_FILE),
            status.to_string().as_bytes(),
        )
    }
}

/// Publish `contents` with a rename. Unix callers get mode `0640`.
fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
    })?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        name.to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o640);
    }
    let mut file = options.open(&temp)?;
    use std::io::Write;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    // The creation mode is masked by the umask; pin the exact documented mode
    // before publishing.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o640))?;
    }
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::{Reloader, Trigger};
    use crate::object_store::assemble;
    use microwave_config::Config;
    use microwave_objectstore::{ObjectStore, RoutingObjectStore};
    use microwave_store::{MetadataStore, SecretsRepository, SqliteStore};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    const CHECKSUM: &str = "0123456789abcdef0123456789abcdef";

    struct Harness {
        _dir: tempfile::TempDir,
        reloader: Reloader,
        router: Arc<RoutingObjectStore>,
        org_id: String,
        data_dir: PathBuf,
        config_path: PathBuf,
    }

    fn base_config(bucket: Option<&str>) -> String {
        let bucket = bucket
            .map(|bucket| format!("bucket = \"{bucket}\"\n"))
            .unwrap_or_default();
        format!(
            r#"
[object_store]
backend = "s3"
{bucket}endpoint = "http://127.0.0.1:3900"
region = "garage"
access_key = "shared-access"
secret_key = "shared-secret"
path_style = true
verify_buckets = false
"#
        )
    }

    fn write_config(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    async fn harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let config_path = dir.path().join("microwave.toml");
        write_config(&config_path, &base_config(Some("mw-old")));

        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let org = store.create_org("acme", "Acme").unwrap();
        let config = Config::load(Some(&config_path)).unwrap();
        let snapshot = assemble(&config, store.as_ref(), "secret").await.unwrap();
        let router = Arc::new(RoutingObjectStore::new(snapshot));
        let reloader = Reloader::new(
            Some(config_path.clone()),
            store.clone(),
            router.clone(),
            data_dir.clone(),
            "boot-1".into(),
        );

        Harness {
            _dir: dir,
            reloader,
            router,
            org_id: org.id,
            data_dir,
            config_path,
        }
    }

    fn url(harness: &Harness) -> String {
        harness
            .router
            .presigned_url("GET", &harness.org_id, CHECKSUM)
            .unwrap()
    }

    fn status(data_dir: &Path) -> serde_json::Value {
        let contents = std::fs::read_to_string(data_dir.join("objectstore.reload")).unwrap();
        serde_json::from_str(&contents).unwrap()
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn an_accepted_reload_swaps_the_routing() {
        let harness = harness().await;
        assert!(url(&harness).contains("mw-old"));

        write_config(&harness.config_path, &base_config(Some("mw-new")));
        harness.reloader.reload(Trigger::Manual).await.unwrap();

        assert!(url(&harness).contains("mw-new"), "{}", url(&harness));
        assert!(!url(&harness).contains("mw-old"));

        let status = status(&harness.data_dir);
        assert_eq!(status["outcome"], "ok");
        assert_eq!(status["trigger"], "manual");
        assert_eq!(status["boot_id"], "boot-1");
        assert!(status["token"].is_null());
        assert!(status["message"].is_null());
        assert!(status["at"].is_string());
    }

    #[tokio::test]
    async fn a_rejected_reload_keeps_the_previous_routing() {
        let harness = harness().await;
        let before = url(&harness);
        assert!(before.contains("mw-old"));

        // A non-strict S3 config without a shared bucket cannot build a
        // default store.
        write_config(&harness.config_path, &base_config(None));
        harness.reloader.reload(Trigger::Manual).await.unwrap_err();

        assert_eq!(url(&harness), before);
        let status = status(&harness.data_dir);
        assert_eq!(status["outcome"], "rejected");
        let message = status["message"].as_str().expect("rejection message");
        assert!(message.contains("object_store.bucket"), "{message}");
        assert!(!message.contains("shared-secret"), "{message}");
    }

    #[tokio::test]
    async fn an_unknown_organization_only_warns() {
        let harness = harness().await;
        write_config(
            &harness.config_path,
            &format!(
                "{}[object_store.organizations.ghost]\nbucket = \"mw-ghost\"\n",
                base_config(Some("mw-new"))
            ),
        );

        let capture = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || CaptureWriter(writer.clone()))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);

        harness.reloader.reload(Trigger::Manual).await.unwrap();
        drop(guard);

        let logs = String::from_utf8(capture.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("ghost"), "logs: {logs}");
        assert_eq!(status(&harness.data_dir)["outcome"], "ok");
        assert!(url(&harness).contains("mw-new"));
    }

    #[tokio::test]
    async fn the_status_file_is_published_atomically_with_the_request_token() {
        let harness = harness().await;
        let request = harness.data_dir.join("objectstore.reload.request");
        std::fs::write(&request, "token-123\n").unwrap();
        write_config(&harness.config_path, &base_config(Some("mw-new")));

        harness.reloader.reload(Trigger::Manual).await.unwrap();

        let status = status(&harness.data_dir);
        assert_eq!(status["token"], "token-123");
        assert_eq!(status["outcome"], "ok");
        assert!(!request.exists(), "the request file is consumed");
        #[cfg(unix)]
        assert_eq!(mode(&harness.data_dir.join("objectstore.reload")), 0o640);
        // No temp file survives the rename.
        let leftovers: Vec<_> = std::fs::read_dir(&harness.data_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".objectstore.reload.tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[tokio::test]
    async fn the_boot_status_is_ok_and_token_free() {
        let harness = harness().await;

        harness.reloader.write_boot_status().await.unwrap();

        let status = status(&harness.data_dir);
        assert_eq!(status["trigger"], "boot");
        assert_eq!(status["outcome"], "ok");
        assert!(status["token"].is_null());
        assert!(!harness.data_dir.join("objectstore.reload.request").exists());
    }

    #[tokio::test]
    async fn secret_stamp_returns_the_newest_matching_timestamp() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(
            SecretsRepository::secret_stamp(&store, "object_store:")
                .await
                .unwrap(),
            None
        );

        store
            .set_secret("object_store", "acme.access_key", "a")
            .unwrap();
        let first = SecretsRepository::secret_stamp(&store, "object_store:")
            .await
            .unwrap()
            .expect("one stamped row");
        // `updated_at` has second resolution.
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        store
            .set_secret("object_store", "acme.secret_key", "b")
            .unwrap();
        let newest = SecretsRepository::secret_stamp(&store, "object_store:")
            .await
            .unwrap()
            .expect("two stamped rows");

        assert!(newest > first, "{newest} must be newer than {first}");
        assert_eq!(
            SecretsRepository::secret_stamp(&store, "bookshelf:")
                .await
                .unwrap(),
            None
        );
        // The inherent method stays available for synchronous callers.
        assert_eq!(store.secret_stamp("object_store:").unwrap(), Some(newest));
    }

    struct CaptureWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[allow(dead_code)]
    fn assert_store(_store: &dyn MetadataStore) {}
}
