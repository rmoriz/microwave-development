//! Timing decorator for [`ObjectStore`].
//!
//! Wraps a store so blob operations land in the object-store histogram without
//! every caller knowing about metrics.

use super::{ObjectMetadata, ObjectStore, ObjectStoreError};
use microwave_metrics::Metrics;
use std::sync::Arc;
use std::time::Instant;

/// Records a timed call for every blob operation that performs backend I/O;
/// `exists` and `presigned_url` delegate untimed.
pub struct TimedObjectStore {
    inner: Arc<dyn ObjectStore + Send + Sync>,
    metrics: Arc<Metrics>,
    /// Store kind named by the caller, never a path, bucket or host.
    backend: &'static str,
}

impl TimedObjectStore {
    pub fn new(
        inner: Arc<dyn ObjectStore + Send + Sync>,
        metrics: Arc<Metrics>,
        backend: &'static str,
    ) -> Self {
        Self {
            inner,
            metrics,
            backend,
        }
    }

    /// Record one finished call. `operation` is the method name.
    fn record(&self, operation: &'static str, started: Instant, okay: bool) {
        self.metrics.record_object_store_request(
            self.backend,
            operation,
            if okay { "ok" } else { "error" },
            started.elapsed(),
        );
    }
}

impl ObjectStore for TimedObjectStore {
    fn put(
        &self,
        organization_id: &str,
        checksum: &str,
        data: &[u8],
    ) -> Result<(), ObjectStoreError> {
        let started = Instant::now();
        let result = self.inner.put(organization_id, checksum, data);
        self.record("put", started, result.is_ok());
        result
    }

    fn get(&self, organization_id: &str, checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        let started = Instant::now();
        let result = self.inner.get(organization_id, checksum);
        self.record("get", started, result.is_ok());
        result
    }

    fn exists(&self, organization_id: &str, checksum: &str) -> bool {
        self.inner.exists(organization_id, checksum)
    }

    fn delete(&self, organization_id: &str, checksum: &str) -> Result<bool, ObjectStoreError> {
        let started = Instant::now();
        let result = self.inner.delete(organization_id, checksum);
        self.record("delete", started, result.is_ok());
        result
    }

    fn metadata(
        &self,
        organization_id: &str,
        checksum: &str,
    ) -> Result<Option<ObjectMetadata>, ObjectStoreError> {
        let started = Instant::now();
        let result = self.inner.metadata(organization_id, checksum);
        self.record("metadata", started, result.is_ok());
        result
    }

    fn presigned_url(&self, method: &str, organization_id: &str, checksum: &str) -> String {
        self.inner.presigned_url(method, organization_id, checksum)
    }

    fn health_check(&self) -> Result<(), ObjectStoreError> {
        let started = Instant::now();
        let result = self.inner.health_check();
        self.record("health_check", started, result.is_ok());
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store whose only interesting answer is an error.
    struct FailingStore;

    impl ObjectStore for FailingStore {
        fn put(
            &self,
            _organization_id: &str,
            _checksum: &str,
            _data: &[u8],
        ) -> Result<(), ObjectStoreError> {
            Ok(())
        }

        fn get(
            &self,
            _organization_id: &str,
            _checksum: &str,
        ) -> Result<Vec<u8>, ObjectStoreError> {
            Err(ObjectStoreError::NotFound)
        }

        fn exists(&self, _organization_id: &str, _checksum: &str) -> bool {
            false
        }

        fn delete(
            &self,
            _organization_id: &str,
            _checksum: &str,
        ) -> Result<bool, ObjectStoreError> {
            Ok(false)
        }

        fn presigned_url(&self, _method: &str, _organization_id: &str, _checksum: &str) -> String {
            "http://example.test/blob".into()
        }
    }

    #[test]
    fn object_store_calls_record_operation_and_outcome() {
        let metrics = Arc::new(Metrics::new());
        let store = TimedObjectStore::new(Arc::new(FailingStore), metrics.clone(), "local");
        let _ = store.get("org", "checksum");
        let entry = &metrics.object_store_series()[0];
        assert_eq!(entry.backend, "local");
        assert_eq!(entry.operation, "get");
        assert_eq!(entry.outcome, "error");
    }

    #[test]
    fn a_successful_call_records_ok() {
        let metrics = Arc::new(Metrics::new());
        let store = TimedObjectStore::new(Arc::new(FailingStore), metrics.clone(), "s3");
        store.put("org", "checksum", b"data").unwrap();
        let entry = &metrics.object_store_series()[0];
        assert_eq!(entry.backend, "s3");
        assert_eq!(entry.operation, "put");
        assert_eq!(entry.outcome, "ok");
    }

    #[test]
    fn metadata_reaches_the_inner_default() {
        let metrics = Arc::new(Metrics::new());
        let store = TimedObjectStore::new(Arc::new(FailingStore), metrics.clone(), "local");
        assert!(store.metadata("org", "checksum").unwrap().is_none());
        let entry = &metrics.object_store_series()[0];
        assert_eq!(entry.backend, "local");
        assert_eq!(entry.operation, "metadata");
        assert_eq!(entry.outcome, "ok");
    }

    #[test]
    fn health_check_reaches_the_inner_default() {
        let metrics = Arc::new(Metrics::new());
        let store = TimedObjectStore::new(Arc::new(FailingStore), metrics.clone(), "local");
        store.health_check().unwrap();
        let entry = &metrics.object_store_series()[0];
        assert_eq!(entry.backend, "local");
        assert_eq!(entry.operation, "health_check");
        assert_eq!(entry.outcome, "ok");
    }

    #[test]
    fn cheap_calls_do_not_record() {
        let metrics = Arc::new(Metrics::new());
        let store = TimedObjectStore::new(Arc::new(FailingStore), metrics.clone(), "local");
        assert!(!store.exists("org", "checksum"));
        assert_eq!(
            store.presigned_url("GET", "org", "checksum"),
            "http://example.test/blob"
        );
        assert!(metrics.object_store_series().is_empty());
    }
}
