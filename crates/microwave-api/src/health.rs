//! Dependency probes rendered by `GET /_status`.
//!
//! Upstream `chef_wm_status` reports one `pong`/`fail` entry per dependency.
//! This module keeps that wire shape while adding a small, bounded probe of the
//! metadata database, the search backend, and the object store. Results are
//! cached briefly so a busy health-check loop does not hit every dependency on
//! every request.

use crate::search::{SearchBackend, SearchError};
use microwave_objectstore::ObjectStore;
use microwave_store::{HealthRepository, MetadataStore};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Freshness window for a completed probe snapshot.
const DEFAULT_TTL: Duration = Duration::from_secs(1);
/// Maximum time any single dependency may spend answering a probe.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1_500);
/// Never serve a cached snapshot older than this, even while refreshing.
const MAX_STALE_AGE: Duration = Duration::from_secs(5);
/// Keep diagnostics short and single-line.
const MAX_DETAIL_LEN: usize = 240;

#[derive(Debug, Clone)]
pub struct DependencyProbe {
    pub name: &'static str,
    pub status: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct DependencySnapshot {
    pub probes: Vec<DependencyProbe>,
}

impl DependencySnapshot {
    pub fn healthy(&self) -> bool {
        self.probes.iter().all(|probe| probe.status == "pong")
    }

    /// Upstream-compatible `upstreams` object: dependency name to `pong`/`fail`.
    pub fn upstreams(&self) -> Map<String, Value> {
        self.probes
            .iter()
            .map(|probe| (probe.name.to_string(), json!(probe.status)))
            .collect()
    }

    /// Per-dependency diagnostics that do not fit upstream's string map.
    pub fn details(&self) -> Map<String, Value> {
        self.probes
            .iter()
            .map(|probe| {
                (
                    probe.name.to_string(),
                    json!({
                        "status": probe.status,
                        "detail": probe.detail,
                    }),
                )
            })
            .collect()
    }
}

/// Short-lived cache shared by every clone of [`crate::AppState`].
///
/// A cold cache is refreshed by exactly one caller; concurrent callers wait for
/// that probe and reuse its result. Once a snapshot exists it is served
/// immediately while an expired snapshot is refreshed in the background, so an
/// unauthenticated `/_status` burst cannot multiply dependency probes.
#[derive(Clone)]
pub struct HealthCache {
    inner: Arc<HealthCacheInner>,
}

struct HealthCacheInner {
    ttl: Duration,
    cached: Mutex<Option<(Instant, Arc<DependencySnapshot>)>>,
    refresh: tokio::sync::Mutex<()>,
    refreshing: AtomicBool,
}

/// Resets the in-flight flag even if the background refresh panics.
struct RefreshGuard(Arc<HealthCacheInner>);

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        self.0.refreshing.store(false, Ordering::Release);
    }
}

impl Default for HealthCache {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthCache {
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_TTL)
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(HealthCacheInner {
                ttl,
                cached: Mutex::new(None),
                refresh: tokio::sync::Mutex::new(()),
                refreshing: AtomicBool::new(false),
            }),
        }
    }

    fn fresh(&self) -> Option<Arc<DependencySnapshot>> {
        let guard = self
            .inner
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.as_ref().and_then(|(taken_at, snapshot)| {
            (taken_at.elapsed() < self.inner.ttl).then(|| snapshot.clone())
        })
    }

    #[cfg(test)]
    fn latest(&self) -> Option<Arc<DependencySnapshot>> {
        self.inner
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|(_, snapshot)| snapshot.clone())
    }

    /// Return the previous snapshot only while it is younger than the absolute
    /// staleness ceiling. Beyond this a request waits for a real probe instead
    /// of serving arbitrarily old health data.
    fn stale_within(&self, max_age: Duration) -> Option<Arc<DependencySnapshot>> {
        let guard = self
            .inner
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.as_ref().and_then(|(taken_at, snapshot)| {
            (taken_at.elapsed() <= max_age).then(|| snapshot.clone())
        })
    }

    fn store(&self, snapshot: Arc<DependencySnapshot>) {
        let mut guard = self
            .inner
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some((Instant::now(), snapshot));
    }

    pub async fn snapshot(
        &self,
        store: &Arc<dyn MetadataStore>,
        search: &Arc<dyn SearchBackend>,
        object_store: &Arc<dyn ObjectStore + Send + Sync>,
    ) -> Arc<DependencySnapshot> {
        if let Some(snapshot) = self.fresh() {
            return snapshot;
        }

        // Serve the previous result immediately and refresh it in the
        // background. The `refreshing` compare-and-swap ensures only one
        // request creates a refresh task; every other stale request keeps the
        // stale snapshot. The mutex remains the inner single-flight guard for
        // cold waiters.
        if let Some(stale) = self.stale_within(MAX_STALE_AGE) {
            if !self.inner.refreshing.swap(true, Ordering::AcqRel) {
                let cache = self.clone();
                let store = Arc::clone(store);
                let search = Arc::clone(search);
                let object_store = Arc::clone(object_store);
                // Build the guard before spawning so an aborted-but-never-polled
                // task still clears the in-flight flag when its future drops.
                let inflight = RefreshGuard(Arc::clone(&self.inner));
                tokio::spawn(async move {
                    let _inflight = inflight;
                    let _refresh = cache.inner.refresh.lock().await;
                    if cache.fresh().is_some() {
                        return;
                    }
                    let snapshot = Arc::new(collect(&store, &search, &object_store).await);
                    cache.store(snapshot);
                });
            }
            return stale;
        }

        // Cold cache: one caller probes, the rest wait and reuse the snapshot.
        let _refresh = self.inner.refresh.lock().await;
        if let Some(snapshot) = self.fresh() {
            return snapshot;
        }
        let snapshot = Arc::new(collect(store, search, object_store).await);
        self.store(Arc::clone(&snapshot));
        snapshot
    }
}

async fn collect(
    store: &Arc<dyn MetadataStore>,
    search: &Arc<dyn SearchBackend>,
    object_store: &Arc<dyn ObjectStore + Send + Sync>,
) -> DependencySnapshot {
    let metadata = probe_metadata(store.as_ref());
    let search_probe = probe_search(search);
    let object = object_store.clone();
    let object_probe = async move {
        // `spawn_blocking` tasks cannot be cancelled. The built-in local and
        // S3 stores bound their own I/O; a custom store that hangs beyond the
        // deadline leaves one detached blocking task alive, while the handler
        // still returns at PROBE_TIMEOUT.
        match tokio::time::timeout(
            PROBE_TIMEOUT,
            tokio::task::spawn_blocking(move || object.health_check()),
        )
        .await
        {
            Ok(Ok(Ok(()))) => healthy("bookshelf", "ok"),
            Ok(Ok(Err(error))) => failed("bookshelf", error.to_string()),
            Ok(Err(_)) => failed("bookshelf", "probe task failed"),
            Err(_) => failed("bookshelf", "probe timed out"),
        }
    };

    let (metadata, search, object_store) = tokio::join!(metadata, search_probe, object_probe);
    DependencySnapshot {
        probes: vec![metadata, search, object_store],
    }
}

async fn probe_metadata(store: &dyn HealthRepository) -> DependencyProbe {
    match tokio::time::timeout(PROBE_TIMEOUT, store.health_check()).await {
        Ok(Ok(())) => healthy("chef_sql", "ok"),
        Ok(Err(error)) => failed("chef_sql", error.to_string()),
        Err(_) => failed("chef_sql", "probe timed out"),
    }
}

async fn probe_search(search: &Arc<dyn SearchBackend>) -> DependencyProbe {
    let name = match search.name() {
        "opensearch" => "chef_opensearch",
        "elasticsearch" => "chef_elasticsearch",
        _ => "chef_index",
    };
    match tokio::time::timeout(PROBE_TIMEOUT, search.health_check()).await {
        Ok(Ok(())) => healthy(name, search.name()),
        Ok(Err(error)) => failed(name, search_error_detail(&error)),
        Err(_) => failed(name, "probe timed out"),
    }
}

fn healthy(name: &'static str, detail: impl Into<String>) -> DependencyProbe {
    DependencyProbe {
        name,
        status: "pong",
        detail: truncate_detail(&detail.into()),
    }
}

fn failed(name: &'static str, detail: impl Into<String>) -> DependencyProbe {
    DependencyProbe {
        name,
        status: "fail",
        detail: truncate_detail(&redact_credentials(&detail.into())),
    }
}

fn truncate_detail(detail: &str) -> String {
    let single_line = detail.replace(['\r', '\n'], " ");
    if single_line.chars().count() <= MAX_DETAIL_LEN {
        single_line
    } else {
        let mut truncated: String = single_line.chars().take(MAX_DETAIL_LEN).collect();
        truncated.push('…');
        truncated
    }
}

fn search_error_detail(error: &SearchError) -> String {
    match error {
        // The backend error embeds the request URL, which may contain userinfo.
        SearchError::Backend { status, .. } => format!("backend returned HTTP {status}"),
        SearchError::Transport(_) => "transport error".to_string(),
        SearchError::BadRequest => "invalid health request".to_string(),
        SearchError::Response(message) => format!("invalid response: {message}"),
        SearchError::Configuration(message) => format!("configuration: {message}"),
    }
}

/// Remove `user:password@` from any URL-shaped text before exposing it.
fn redact_credentials(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find("://") {
        let (head, tail) = rest.split_at(index + 3);
        output.push_str(head);
        let authority_end = tail
            .find(|character: char| {
                matches!(character, '/' | '?' | '#' | ')') || character.is_whitespace()
            })
            .unwrap_or(tail.len());
        let authority = &tail[..authority_end];
        if let Some(at) = authority.rfind('@') {
            output.push_str("***");
            output.push_str(&authority[at..]);
        } else {
            output.push_str(authority);
        }
        rest = &tail[authority_end..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::{
        probe_metadata, redact_credentials, truncate_detail, DependencyProbe, DependencySnapshot,
        HealthCache, MAX_DETAIL_LEN, MAX_STALE_AGE,
    };
    use microwave_store::{HealthRepository, StoreError};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn snapshot() -> Arc<DependencySnapshot> {
        Arc::new(DependencySnapshot {
            probes: vec![DependencyProbe {
                name: "chef_sql",
                status: "pong",
                detail: "ok".to_string(),
            }],
        })
    }

    #[test]
    fn cache_hits_within_ttl_and_expires_afterwards() {
        let cache = HealthCache::with_ttl(Duration::from_millis(50));
        cache.store(snapshot());

        let clone = cache.clone();
        assert!(clone.fresh().is_some(), "clones must share the snapshot");
        assert!(cache.latest().is_some());

        std::thread::sleep(Duration::from_millis(60));
        assert!(cache.fresh().is_none(), "the snapshot must expire");
        assert!(
            cache.latest().is_some(),
            "the stale snapshot stays available for stale-while-refresh"
        );
    }

    #[test]
    fn stale_snapshot_has_an_absolute_max_age() {
        let cache = HealthCache::with_ttl(Duration::from_millis(1));
        {
            let mut guard = cache
                .inner
                .cached
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some((
                Instant::now() - MAX_STALE_AGE - Duration::from_secs(1),
                snapshot(),
            ));
        }

        assert!(cache.fresh().is_none());
        assert!(
            cache.stale_within(MAX_STALE_AGE).is_none(),
            "a snapshot older than the ceiling must not be served stale"
        );
        assert!(cache.latest().is_some());
    }

    struct FailingMetadata;

    #[async_trait::async_trait]
    impl HealthRepository for FailingMetadata {
        async fn health_check(&self) -> Result<(), StoreError> {
            Err(StoreError::Invalid("metadata is unavailable".to_string()))
        }
    }

    struct HangingMetadata;

    #[async_trait::async_trait]
    impl HealthRepository for HangingMetadata {
        async fn health_check(&self) -> Result<(), StoreError> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn metadata_probe_reports_failures() {
        let probe = probe_metadata(&FailingMetadata).await;
        assert_eq!(probe.status, "fail");
        assert!(probe.detail.contains("metadata is unavailable"));
    }

    #[tokio::test]
    async fn metadata_probe_times_out() {
        let started = Instant::now();
        let probe = probe_metadata(&HangingMetadata).await;
        assert_eq!(probe.status, "fail");
        assert_eq!(probe.detail, "probe timed out");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "metadata probe exceeded its bound: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn redacts_url_userinfo() {
        let detail = "search backend returned status 401 for https://user:secret@example.test/chef";
        assert_eq!(
            redact_credentials(detail),
            "search backend returned status 401 for https://***@example.test/chef"
        );
        assert!(!redact_credentials(detail).contains("secret"));
    }

    #[test]
    fn leaves_unauthenticated_urls_and_plain_text_untouched() {
        assert_eq!(
            redact_credentials("connect http://127.0.0.1:9200 failed"),
            "connect http://127.0.0.1:9200 failed"
        );
        assert_eq!(
            redact_credentials("relation does not exist"),
            "relation does not exist"
        );
    }

    #[test]
    fn truncates_long_details_to_one_line() {
        let detail = format!("{}\nsecond line", "x".repeat(MAX_DETAIL_LEN + 10));
        let truncated = truncate_detail(&detail);
        assert!(!truncated.contains('\n'));
        assert!(truncated.ends_with('…'));
    }
}
