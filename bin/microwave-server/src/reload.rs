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
use std::time::Duration;

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
    /// Handle the API verifier reads; swapped together with the local store so
    /// a rotated local secret takes effect without a restart.
    local_secret: Arc<arc_swap::ArcSwap<String>>,
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
        local_secret: Arc<arc_swap::ArcSwap<String>>,
    ) -> Self {
        Self {
            config_path,
            store,
            router,
            data_dir,
            boot_id,
            local_secret,
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
        // Only the token this reload actually read may be consumed. A token
        // written while this reload runs belongs to the queued reload.
        let taken = take_token(&self.request_path());
        // A queued reload must not erase an unanswered answer with
        // `token: null`: when this reload read no request, echo the token of
        // the previous same-boot status so the ctl still matches. The boot id
        // keeps a status from an older process out.
        let echo = taken
            .clone()
            .or_else(|| answered_token(&self.status_path(), &self.boot_id));
        let result = self.run(trigger).await;

        let (outcome, message) = match &result {
            Ok(()) => ("ok", None),
            Err(error) => ("rejected", Some(error.to_string())),
        };
        if let Err(error) = self.write_status(
            echo.as_deref(),
            &trigger.to_string(),
            outcome,
            message.as_deref(),
        ) {
            tracing::error!(%error, "failed to write the object store reload status");
        }
        // Compare-and-delete: a request file rewritten meanwhile stays for the
        // queued reload. A read token is consumed even on rejection so the ctl
        // never reads a stale answer from a later request.
        if let Some(token) = taken {
            consume_token(&self.request_path(), &token);
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
        // Only meaningful for the local backend, where the verifier shares the
        // key with the store we just installed.
        self.local_secret.store(Arc::new(secret));
        Ok(())
    }

    fn request_path(&self) -> PathBuf {
        self.data_dir.join(REQUEST_FILE)
    }

    fn status_path(&self) -> PathBuf {
        self.data_dir.join(STATUS_FILE)
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

/// The inputs the periodic timer watches: the config file, the stored
/// `object_store:` secrets and the org table. It never carries secret values.
///
/// `org_count` is deliberately extra: `updated_at` has second precision, so two
/// org creations inside one second leave the newest timestamp unchanged while
/// the count still moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    config_mtime: Option<std::time::SystemTime>,
    config_len: u64,
    secrets: Option<String>,
    orgs: Option<String>,
    org_count: usize,
}

/// Collect a stamp. A missing config file is not an error: the timer keeps
/// running and reloads on the next change.
async fn stamp(reloader: &Reloader, config_path: Option<&Path>) -> Result<Stamp, std::io::Error> {
    let (config_mtime, config_len) = match config_path {
        None => (None, 0),
        Some(path) => match std::fs::metadata(path) {
            Ok(metadata) => (metadata.modified().ok(), metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, 0),
            Err(error) => return Err(error),
        },
    };
    let orgs = reloader
        .store
        .list_orgs()
        .await
        .map_err(std::io::Error::other)?;
    Ok(Stamp {
        config_mtime,
        config_len,
        secrets: reloader
            .store
            .secret_stamp("object_store:")
            .await
            .map_err(std::io::Error::other)?,
        orgs: orgs
            .iter()
            .map(|org| org.updated_at.as_str())
            .max()
            .map(str::to_string),
        org_count: orgs.len(),
    })
}

/// Poll the stamp and reload only when it changed. A zero interval disables the
/// timer.
///
/// The first stamp is taken before the first tick, so startup never triggers an
/// unconditional reload. A failed first read is simply compared on the next
/// tick. Reload errors are the reloader's to log and record in the status.
pub fn spawn_periodic_reloader(
    reloader: Arc<Reloader>,
    interval_seconds: u64,
) -> Option<tokio::task::JoinHandle<()>> {
    if interval_seconds == 0 {
        return None;
    }
    Some(tokio::spawn(async move {
        let config_path = reloader.config_path.clone();
        let mut last_seen = stamp(&reloader, config_path.as_deref()).await.ok();
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_seconds));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Ok(current) = stamp(&reloader, config_path.as_deref()).await else {
                continue;
            };
            if last_seen.as_ref() == Some(&current) {
                continue;
            }
            // Remember the stamp only once the reload went through: a rejected
            // snapshot must be retried on the next tick instead of sleeping
            // until an unrelated write moves the stamp.
            if reloader.reload(Trigger::Interval).await.is_ok() {
                last_seen = Some(current);
            }
        }
    }))
}

/// Read the request token, if any.
fn take_token(path: &Path) -> Option<String> {
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Remove the request file only while it still holds `token`.
///
/// A token written while the reload was running stays in place for the queued
/// reload instead of being answered by the wrong status.
fn consume_token(path: &Path, token: &str) {
    let still = std::fs::read_to_string(path).ok();
    if still.as_deref().map(str::trim) != Some(token) {
        return;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, "failed to remove the object store reload request")
        }
    }
}

/// The token of the previous status, when it belongs to this boot.
///
/// A queued reload that read no request file echoes it instead of overwriting
/// the answer with `token: null`.
fn answered_token(path: &Path, boot_id: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let status: serde_json::Value = serde_json::from_str(&contents).ok()?;
    if status.get("boot_id")?.as_str()? != boot_id {
        return None;
    }
    let token = status.get("token")?.as_str()?.trim();
    (!token.is_empty()).then(|| token.to_string())
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
    use super::{consume_token, spawn_periodic_reloader, stamp, take_token, Reloader, Trigger};
    use crate::object_store::assemble;
    use microwave_config::Config;
    use microwave_objectstore::{ObjectStore, RoutingObjectStore};
    use microwave_store::{
        MetadataStore, SecretsRepository, SqliteStore, OBJECT_STORE_SECRET_GROUP,
        OBJECT_STORE_SECRET_NAME,
    };
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    const CHECKSUM: &str = "0123456789abcdef0123456789abcdef";

    struct Harness {
        _dir: tempfile::TempDir,
        reloader: Arc<Reloader>,
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
        let local_secret = Arc::new(arc_swap::ArcSwap::from_pointee("secret".to_string()));
        let reloader = Arc::new(Reloader::new(
            Some(config_path.clone()),
            store.clone(),
            router.clone(),
            data_dir.clone(),
            "boot-1".into(),
            local_secret.clone(),
        ));

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

    fn write_status_file(data_dir: &Path, boot_id: &str, token: &str) {
        let status = serde_json::json!({
            "boot_id": boot_id,
            "token": token,
            "trigger": "manual",
            "at": "2026-11-07T00:00:00Z",
            "outcome": "ok",
            "message": null,
        });
        std::fs::write(data_dir.join("objectstore.reload"), status.to_string()).unwrap();
    }

    fn base_config_at(bucket: &str, endpoint: &str, verify: bool) -> String {
        base_config(Some(bucket))
            .replace("http://127.0.0.1:3900", endpoint)
            .replace(
                "verify_buckets = false",
                &format!("verify_buckets = {verify}"),
            )
    }

    #[tokio::test]
    async fn consume_token_only_removes_the_token_it_answered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("request");
        std::fs::write(&path, "newer\n").unwrap();

        consume_token(&path, "older");
        assert_eq!(
            take_token(&path).as_deref(),
            Some("newer"),
            "a rewritten file survives"
        );

        consume_token(&path, "newer");
        assert_eq!(take_token(&path), None, "the answered token is removed");
    }

    #[tokio::test]
    async fn a_tokenless_reload_keeps_the_previous_answer() {
        let harness = harness().await;
        write_status_file(&harness.data_dir, "boot-1", "tok-previous");
        write_config(&harness.config_path, &base_config(Some("mw-new")));

        harness.reloader.reload(Trigger::Manual).await.unwrap();

        assert_eq!(status(&harness.data_dir)["token"], "tok-previous");
    }

    #[tokio::test]
    async fn a_status_from_another_boot_supplies_no_token() {
        let harness = harness().await;
        write_status_file(&harness.data_dir, "other-boot", "tok-stale");
        write_config(&harness.config_path, &base_config(Some("mw-new")));

        harness.reloader.reload(Trigger::Manual).await.unwrap();

        assert!(status(&harness.data_dir)["token"].is_null());
    }

    /// The first reload holds the lock while the token is written; its
    /// compare-and-delete must leave the file for the queued reload, which
    /// answers it. The listener accepts and never responds, so the bucket
    /// probe blocks until its 1.5 s timeout.
    #[tokio::test]
    async fn a_token_written_during_a_running_reload_is_answered_by_the_queued_one() {
        let harness = harness().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hold = tokio::spawn(async move {
            let mut sockets = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                sockets.push(socket);
            }
        });
        write_config(
            &harness.config_path,
            &base_config_at("mw-new", &format!("http://{addr}"), true),
        );

        let first = tokio::spawn({
            let reloader = harness.reloader.clone();
            async move { reloader.reload(Trigger::Manual).await }
        });
        // Let the first reload pass its token read before racing it.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(
            harness.data_dir.join("objectstore.reload.request"),
            "tok-queued\n",
        )
        .unwrap();
        let second = tokio::spawn({
            let reloader = harness.reloader.clone();
            async move { reloader.reload(Trigger::Manual).await }
        });

        first.await.unwrap().unwrap_err();
        second.await.unwrap().unwrap_err();
        hold.abort();

        let status = status(&harness.data_dir);
        assert_eq!(status["token"], "tok-queued");
        assert_eq!(status["outcome"], "rejected");
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

    #[tokio::test]
    async fn an_unchanged_stamp_compares_equal() {
        let harness = harness().await;
        let first = stamp(&harness.reloader, Some(&harness.config_path))
            .await
            .unwrap();
        let second = stamp(&harness.reloader, Some(&harness.config_path))
            .await
            .unwrap();

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn a_missing_config_file_is_not_a_stamp_error() {
        let harness = harness().await;

        let missing = harness.config_path.with_file_name("gone.toml");
        let missing_stamp = stamp(&harness.reloader, Some(&missing)).await.unwrap();
        let no_path = stamp(&harness.reloader, None).await.unwrap();

        assert_eq!(missing_stamp, no_path);
        assert!(missing_stamp.config_mtime.is_none());
        assert_eq!(missing_stamp.config_len, 0);
    }

    #[tokio::test]
    async fn a_new_object_store_secret_changes_the_stamp() {
        let harness = harness().await;
        let before = stamp(&harness.reloader, Some(&harness.config_path))
            .await
            .unwrap();

        harness
            .reloader
            .store
            .set_secret("object_store", "acme.access_key", "acme-access")
            .await
            .unwrap();
        let after = stamp(&harness.reloader, Some(&harness.config_path))
            .await
            .unwrap();

        assert_ne!(before.secrets, after.secrets);
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn a_new_organization_changes_the_stamp() {
        let harness = harness().await;
        let before = stamp(&harness.reloader, Some(&harness.config_path))
            .await
            .unwrap();

        harness
            .reloader
            .store
            .create_org("beta", "Beta")
            .await
            .unwrap();
        let after = stamp(&harness.reloader, Some(&harness.config_path))
            .await
            .unwrap();

        // `updated_at` is second precision; the count moves even when the
        // timestamp does not.
        assert_eq!(after.org_count, before.org_count + 1);
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn rewriting_the_config_file_changes_the_stamp() {
        let harness = harness().await;
        let before = stamp(&harness.reloader, Some(&harness.config_path))
            .await
            .unwrap();

        write_config(
            &harness.config_path,
            &base_config(Some("mw-a-longer-new-bucket")),
        );
        let after = stamp(&harness.reloader, Some(&harness.config_path))
            .await
            .unwrap();

        assert!(after.config_mtime.is_some());
        assert_ne!(before.config_len, after.config_len);
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn a_zero_interval_starts_no_timer() {
        let harness = harness().await;

        assert!(spawn_periodic_reloader(harness.reloader.clone(), 0).is_none());
    }

    #[tokio::test]
    async fn a_positive_interval_starts_a_timer() {
        let harness = harness().await;

        let handle = spawn_periodic_reloader(harness.reloader.clone(), 1).expect("a timer task");
        handle.abort();
        let _ = handle.await;
    }

    /// The real tick path: with a write in place the first interval tick must
    /// reload and record the `interval` trigger. It is only reached by a changed
    /// stamp, never by the priming read at spawn.
    #[tokio::test]
    async fn the_timer_reloads_a_changed_stamp_but_not_the_primed_one() {
        let harness = harness().await;
        let handle = spawn_periodic_reloader(harness.reloader.clone(), 1).expect("a timer task");
        // Let the task prime its first stamp before the write lands, so the
        // change is what triggers the reload.
        tokio::time::sleep(Duration::from_millis(200)).await;
        write_config(&harness.config_path, &base_config(Some("mw-new")));

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if harness.data_dir.join("objectstore.reload").exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the timer must reload the changed config");
        handle.abort();
        let _ = handle.await;

        let status = status(&harness.data_dir);
        assert_eq!(status["trigger"], "interval");
        assert_eq!(status["outcome"], "ok");
        assert!(url(&harness).contains("mw-new"));
    }

    /// A rejected snapshot must keep the old stamp, so the next tick retries
    /// the same, unchanged change instead of sleeping until an unrelated write.
    #[tokio::test]
    async fn a_rejected_reload_is_retried_on_the_next_tick() {
        let harness = harness().await;
        let handle = spawn_periodic_reloader(harness.reloader.clone(), 1).expect("a timer task");
        tokio::time::sleep(Duration::from_millis(200)).await;
        // `Bad` is structurally invalid, so every attempt is rejected.
        write_config(&harness.config_path, &base_config(Some("Bad")));

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if harness.data_dir.join("objectstore.reload").exists()
                    && status(&harness.data_dir)["outcome"] == "rejected"
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the first tick must reject the invalid config");
        let first = status(&harness.data_dir)["at"]
            .as_str()
            .unwrap()
            .to_string();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if status(&harness.data_dir)["at"] != first.as_str() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("an unchanged stamp must be retried after a rejection");
        handle.abort();
        let _ = handle.await;

        assert_eq!(status(&harness.data_dir)["outcome"], "rejected");
    }

    /// A rotated local secret must reach the verifier handle together with the
    /// local store, or every already-handed-out token would keep verifying
    /// against the old key.
    #[tokio::test]
    async fn a_reload_adopts_a_rotated_local_secret() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let config_path = dir.path().join("microwave.toml");
        write_config(
            &config_path,
            &format!(
                "[paths]\ndata_dir = {:?}\n[object_store]\nbackend = \"local\"\n",
                data_dir.display().to_string()
            ),
        );

        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let config = Config::load(Some(&config_path)).unwrap();
        let router = Arc::new(RoutingObjectStore::new(
            assemble(&config, store.as_ref(), "old-secret")
                .await
                .unwrap(),
        ));
        let secret = Arc::new(arc_swap::ArcSwap::from_pointee("old-secret".to_string()));
        let reloader = Reloader::new(
            Some(config_path),
            store.clone(),
            router,
            data_dir,
            "boot-1".into(),
            secret.clone(),
        );
        store
            .set_secret(
                OBJECT_STORE_SECRET_GROUP,
                OBJECT_STORE_SECRET_NAME,
                "rotated-secret",
            )
            .unwrap();

        reloader.reload(Trigger::Manual).await.unwrap();

        assert_eq!(secret.load().as_str(), "rotated-secret");
    }

    #[tokio::test]
    async fn an_interval_trigger_is_recorded_in_the_status() {
        let harness = harness().await;
        write_config(&harness.config_path, &base_config(Some("mw-new")));

        harness.reloader.reload(Trigger::Interval).await.unwrap();

        let status = status(&harness.data_dir);
        assert_eq!(status["trigger"], "interval");
        assert_eq!(status["outcome"], "ok");
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
