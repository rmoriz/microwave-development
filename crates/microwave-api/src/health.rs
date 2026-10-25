//! Dependency probes rendered by `GET /_status`.
//!
//! Upstream `chef_wm_status` reports one `pong`/`fail` entry per dependency.
//! This module keeps that wire shape while adding a small, bounded probe of the
//! metadata database, the search backend, and the object store. Results are
//! cached briefly so a busy health-check loop does not hit every dependency on
//! every request.

use crate::search::{SearchBackend, SearchError};
use microwave_objectstore::ObjectStore;
use microwave_store::MetadataStore;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Freshness window for a completed probe snapshot.
const DEFAULT_TTL: Duration = Duration::from_secs(1);
/// Maximum time any single dependency may spend answering a probe.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1_500);
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
pub struct HealthCache {
    ttl: Duration,
    cached: Mutex<Option<(Instant, Arc<DependencySnapshot>)>>,
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
            ttl,
            cached: Mutex::new(None),
        }
    }

    fn fresh(&self) -> Option<Arc<DependencySnapshot>> {
        let guard = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.as_ref().and_then(|(taken_at, snapshot)| {
            (taken_at.elapsed() < self.ttl).then(|| snapshot.clone())
        })
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

        let snapshot = Arc::new(collect(store, search, object_store).await);
        let mut guard = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some((Instant::now(), snapshot.clone()));
        snapshot
    }
}

async fn collect(
    store: &Arc<dyn MetadataStore>,
    search: &Arc<dyn SearchBackend>,
    object_store: &Arc<dyn ObjectStore + Send + Sync>,
) -> DependencySnapshot {
    let metadata = probe_metadata(store);
    let search_probe = probe_search(search);
    let object = object_store.clone();
    let object_probe = async move {
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

async fn probe_metadata(store: &Arc<dyn MetadataStore>) -> DependencyProbe {
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
    use super::{redact_credentials, truncate_detail, MAX_DETAIL_LEN};

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
