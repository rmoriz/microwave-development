//! Timing decorator for [`SearchBackend`].
//!
//! Wraps a backend so search, index, and health calls land in the search
//! histogram without every call site knowing about metrics.

use super::{IndexOutcome, SearchBackend, SearchDocument, SearchError, SearchHits, SearchRequest};
use async_trait::async_trait;
use microwave_metrics::Metrics;
use std::sync::Arc;
use std::time::Instant;

/// Records every backend call against the search histogram.
pub struct TimedSearch {
    inner: Arc<dyn SearchBackend>,
    metrics: Arc<Metrics>,
    /// Cached from the inner backend, which reports the configured kind.
    backend: &'static str,
}

impl TimedSearch {
    pub fn new(inner: Arc<dyn SearchBackend>, metrics: Arc<Metrics>) -> Self {
        let backend = inner.name();
        Self {
            inner,
            metrics,
            backend,
        }
    }

    fn record(&self, started: Instant, okay: bool) {
        self.metrics.record_search_request(
            self.backend,
            if okay { "ok" } else { "error" },
            started.elapsed(),
        );
    }
}

#[async_trait]
impl SearchBackend for TimedSearch {
    /// The real backend name, so `/_status` keeps naming the configured one.
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    async fn health_check(&self) -> Result<(), SearchError> {
        let started = Instant::now();
        let result = self.inner.health_check().await;
        self.record(started, result.is_ok());
        result
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchHits, SearchError> {
        let started = Instant::now();
        let result = self.inner.search(request).await;
        self.record(started, result.is_ok());
        result
    }

    async fn index(&self, document: &SearchDocument) -> Result<IndexOutcome, SearchError> {
        let started = Instant::now();
        let result = self.inner.index(document).await;
        self.record(started, result.is_ok());
        result
    }

    async fn index_many(&self, documents: &[SearchDocument]) -> Result<usize, SearchError> {
        let started = Instant::now();
        let result = self.inner.index_many(documents).await;
        self.record(started, result.is_ok());
        result
    }

    async fn refresh(&self) -> Result<(), SearchError> {
        let started = Instant::now();
        let result = self.inner.refresh().await;
        self.record(started, result.is_ok());
        result
    }

    fn supports_purge(&self) -> bool {
        self.inner.supports_purge()
    }

    async fn delete(&self, org_id: &str, id: &str) -> Result<(), SearchError> {
        let started = Instant::now();
        let result = self.inner.delete(org_id, id).await;
        self.record(started, result.is_ok());
        result
    }

    async fn delete_index(&self, org_id: &str) -> Result<(), SearchError> {
        let started = Instant::now();
        let result = self.inner.delete_index(org_id).await;
        self.record(started, result.is_ok());
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{EmbeddedSearch, IndexKind, SearchQuery};
    use microwave_metrics::Metrics;
    use std::sync::Arc;

    /// An empty store is enough: the embedded backend answers from the store.
    fn store() -> Arc<dyn microwave_store::MetadataStore> {
        let dir = tempfile::tempdir().unwrap();
        Arc::new(microwave_store::SqliteStore::open(&dir.path().join("metrics.db")).unwrap())
    }

    fn request() -> SearchRequest {
        SearchRequest {
            org_id: "acme".into(),
            kind: IndexKind::Node,
            query: SearchQuery::MatchAll,
            start: 0,
            rows: 10,
            actor_authz_id: None,
        }
    }

    /// A backend whose only interesting answer is an error.
    struct FailingSearch;

    #[async_trait::async_trait]
    impl SearchBackend for FailingSearch {
        fn name(&self) -> &'static str {
            "failing"
        }

        async fn search(&self, _request: &SearchRequest) -> Result<SearchHits, SearchError> {
            Err(SearchError::BadRequest)
        }

        async fn index(&self, _document: &SearchDocument) -> Result<IndexOutcome, SearchError> {
            Err(SearchError::BadRequest)
        }

        async fn delete(&self, _org_id: &str, _id: &str) -> Result<(), SearchError> {
            Err(SearchError::BadRequest)
        }

        async fn delete_index(&self, _org_id: &str) -> Result<(), SearchError> {
            Err(SearchError::BadRequest)
        }
    }

    #[tokio::test]
    async fn searches_record_backend_and_outcome() {
        let metrics = Arc::new(Metrics::new());
        let backend = TimedSearch::new(Arc::new(EmbeddedSearch::new(store())), metrics.clone());
        let _ = backend.search(&request()).await;
        let entry = &metrics.search_series()[0];
        assert_eq!(entry.backend, "embedded");
        assert_eq!(entry.outcome, "ok");
    }

    #[tokio::test]
    async fn a_failed_search_records_an_error_outcome() {
        let metrics = Arc::new(Metrics::new());
        let backend = TimedSearch::new(Arc::new(FailingSearch), metrics.clone());
        let _ = backend.search(&request()).await;
        assert_eq!(metrics.search_series()[0].outcome, "error");
    }

    #[tokio::test]
    async fn health_check_reaches_the_inner_default() {
        let metrics = Arc::new(Metrics::new());
        let backend = TimedSearch::new(Arc::new(EmbeddedSearch::new(store())), metrics.clone());
        backend.health_check().await.unwrap();
        assert_eq!(metrics.search_series()[0].outcome, "ok");
    }

    #[test]
    fn name_reports_the_inner_backend() {
        let metrics = Arc::new(Metrics::new());
        let backend = TimedSearch::new(Arc::new(EmbeddedSearch::new(store())), metrics);
        assert_eq!(backend.name(), "embedded");
        assert!(!backend.supports_purge());
    }
}
