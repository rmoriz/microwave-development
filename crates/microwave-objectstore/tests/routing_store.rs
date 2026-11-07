//! Tests for the per-organization routing decorator.

use microwave_objectstore::{
    ObjectMetadata, ObjectStore, ObjectStoreError, RoutingObjectStore, S3Config, S3ObjectStore,
    TenantSnapshot,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const CHEF_BLOB: &str = "5d41402abc4b2a76b9719d911017c592";
const OTHER_BLOB: &str = "7d793037a0760186574b0282f2f435e7";

/// Recording double. It records the bare operation name, answers with fixed
/// data and can be told to fail writes.
struct Double {
    bucket: String,
    failing: bool,
    calls: Mutex<Vec<String>>,
}

impl Double {
    fn new(bucket: &str) -> Arc<Self> {
        Arc::new(Self {
            bucket: bucket.to_string(),
            failing: false,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn failing(bucket: &str) -> Arc<Self> {
        Arc::new(Self {
            bucket: bucket.to_string(),
            failing: true,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, operation: &str) {
        self.calls.lock().unwrap().push(operation.to_string());
    }
}

impl ObjectStore for Double {
    fn put(
        &self,
        organization_id: &str,
        checksum: &str,
        _data: &[u8],
    ) -> Result<(), ObjectStoreError> {
        self.record("put");
        if self.failing {
            // Real backends quote the request URL, which carries the checksum.
            return Err(ObjectStoreError::Http(format!(
                "PUT https://{}.example.test/organization-{organization_id}/checksum-{checksum} failed: bucket is down",
                self.bucket
            )));
        }
        Ok(())
    }

    fn get(&self, _organization_id: &str, _checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        self.record("get");
        Ok(b"hello".to_vec())
    }

    fn exists(&self, _organization_id: &str, _checksum: &str) -> bool {
        self.record("exists");
        !self.failing
    }

    fn delete(&self, _organization_id: &str, _checksum: &str) -> Result<bool, ObjectStoreError> {
        self.record("delete");
        Ok(true)
    }

    fn metadata(
        &self,
        _organization_id: &str,
        _checksum: &str,
    ) -> Result<Option<ObjectMetadata>, ObjectStoreError> {
        self.record("metadata");
        Ok(Some(ObjectMetadata {
            len: 5,
            modified: None,
        }))
    }

    fn presigned_url(
        &self,
        _method: &str,
        organization_id: &str,
        checksum: &str,
    ) -> Result<String, ObjectStoreError> {
        self.record("presigned_url");
        Ok(format!(
            "https://{}.example.test/organization-{organization_id}/checksum-{checksum}",
            self.bucket
        ))
    }

    fn health_check(&self) -> Result<(), ObjectStoreError> {
        self.record("health_check");
        Ok(())
    }

    fn bucket(&self) -> Option<&str> {
        Some(&self.bucket)
    }
}

fn snapshot(
    default: Option<Arc<dyn ObjectStore + Send + Sync>>,
    health: Option<Arc<dyn ObjectStore + Send + Sync>>,
    by_org: Vec<(&str, Arc<dyn ObjectStore + Send + Sync>)>,
) -> TenantSnapshot {
    TenantSnapshot {
        default,
        health,
        by_org: by_org
            .into_iter()
            .map(|(org, store)| (org.to_string(), store))
            .collect::<HashMap<_, _>>(),
    }
}

#[test]
fn mapped_organization_uses_its_own_store() {
    let mapped = Double::new("acme-bucket");
    let other = Double::new("other-bucket");
    let default = Double::new("default-bucket");
    let router = RoutingObjectStore::new(snapshot(
        Some(default.clone()),
        None,
        vec![("acme", mapped.clone()), ("other", other.clone())],
    ));

    router.put("acme", CHEF_BLOB, b"hello").unwrap();
    assert_eq!(router.get("acme", CHEF_BLOB).unwrap(), b"hello");
    assert!(router.exists("acme", CHEF_BLOB));
    assert!(router.delete("acme", CHEF_BLOB).unwrap());
    assert_eq!(router.metadata("acme", CHEF_BLOB).unwrap().unwrap().len, 5);
    router.presigned_url("GET", "acme", CHEF_BLOB).unwrap();

    assert_eq!(
        mapped.calls(),
        vec![
            "put",
            "get",
            "exists",
            "delete",
            "metadata",
            "presigned_url"
        ]
    );
    assert!(other.calls().is_empty());
    assert!(default.calls().is_empty());
}

#[test]
fn unmapped_organization_uses_the_default_store() {
    let mapped = Double::new("acme-bucket");
    let default = Double::new("default-bucket");
    let router = RoutingObjectStore::new(snapshot(
        Some(default.clone()),
        None,
        vec![("acme", mapped.clone())],
    ));

    router.put("unknown", CHEF_BLOB, b"hello").unwrap();
    assert_eq!(router.get("unknown", CHEF_BLOB).unwrap(), b"hello");
    assert!(router.exists("unknown", CHEF_BLOB));
    assert!(router.delete("unknown", CHEF_BLOB).unwrap());
    assert!(router.metadata("unknown", CHEF_BLOB).unwrap().is_some());
    router.presigned_url("GET", "unknown", CHEF_BLOB).unwrap();

    assert_eq!(
        default.calls(),
        vec![
            "put",
            "get",
            "exists",
            "delete",
            "metadata",
            "presigned_url"
        ]
    );
    assert!(mapped.calls().is_empty());
}

#[test]
fn unmapped_organization_without_default_reports_no_org_bucket() {
    // Every call logs an error; the log assertion elsewhere must not see them.
    let _guard = LOG_TEST_LOCK.lock().unwrap();
    let mapped = Double::new("acme-bucket");
    let router = RoutingObjectStore::new(snapshot(None, None, vec![("acme", mapped.clone())]));

    for result in [
        router.put("unknown", CHEF_BLOB, b"hello").map(|_| ()),
        router.get("unknown", CHEF_BLOB).map(|_| ()),
        router.delete("unknown", CHEF_BLOB).map(|_| ()),
        router.metadata("unknown", CHEF_BLOB).map(|_| ()),
        router
            .presigned_url("GET", "unknown", CHEF_BLOB)
            .map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(ObjectStoreError::NoOrgBucket)),
            "expected NoOrgBucket, got {result:?}"
        );
    }
    assert!(!router.exists("unknown", CHEF_BLOB));
    assert!(mapped.calls().is_empty());
}

#[test]
fn health_check_uses_only_the_health_store() {
    let health = Double::new("health-bucket");
    let default = Double::new("default-bucket");
    let mapped = Double::new("acme-bucket");
    let router = RoutingObjectStore::new(snapshot(
        Some(default.clone()),
        Some(health.clone()),
        vec![("acme", mapped.clone())],
    ));

    router.health_check().unwrap();
    router.health_check().unwrap();

    assert_eq!(health.calls(), vec!["health_check", "health_check"]);
    assert!(default.calls().is_empty());
    assert!(mapped.calls().is_empty());
}

#[test]
fn health_check_falls_back_to_the_default_store() {
    let default = Double::new("default-bucket");
    let router = RoutingObjectStore::new(snapshot(Some(default.clone()), None, vec![]));

    router.health_check().unwrap();
    assert_eq!(default.calls(), vec!["health_check"]);
}

#[test]
fn health_check_without_any_store_is_an_invalid_configuration() {
    let router = RoutingObjectStore::new(snapshot(None, None, vec![]));

    let error = router.health_check().unwrap_err();
    assert!(
        matches!(&error, ObjectStoreError::InvalidConfig(message) if message == "health bucket is not configured"),
        "got {error:?}"
    );
}

#[test]
fn install_replaces_the_whole_snapshot() {
    let old = Double::new("old-bucket");
    let default = Double::new("default-bucket");
    let router = RoutingObjectStore::new(snapshot(
        Some(default.clone()),
        None,
        vec![("acme", old.clone()), ("gone", old.clone())],
    ));

    let new = Double::new("new-bucket");
    router.install(snapshot(
        Some(default.clone()),
        None,
        vec![("acme", new.clone())],
    ));

    router.put("acme", CHEF_BLOB, b"hello").unwrap();
    assert_eq!(new.calls(), vec!["put"]);
    assert!(old.calls().is_empty());

    // The removed mapping falls back to the default store.
    router.put("gone", OTHER_BLOB, b"hello").unwrap();
    assert_eq!(default.calls(), vec!["put"]);
    assert!(old.calls().is_empty());
}

#[test]
fn presigned_url_uses_the_mapped_bucket() {
    let mut config = S3Config::new(
        "https://s3.example.com",
        "acme-bucket",
        "us-east-1",
        "key",
        "secret",
    );
    config.path_style = false;
    let mapped: Arc<dyn ObjectStore + Send + Sync> = Arc::new(S3ObjectStore::new(config).unwrap());
    let default = Double::new("default-bucket");
    let router = RoutingObjectStore::new(snapshot(
        Some(default.clone()),
        None,
        vec![("acme", mapped)],
    ));

    let url = router.presigned_url("GET", "acme", CHEF_BLOB).unwrap();
    assert!(
        url.starts_with("https://acme-bucket.s3.example.com/"),
        "unexpected URL: {url}"
    );
    assert!(
        url.contains(&format!("/organization-acme/checksum-{CHEF_BLOB}?")),
        "unexpected URL: {url}"
    );
    assert!(default.calls().is_empty());
}

static LOG_CAPTURE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static LOG_INIT: std::sync::Once = std::sync::Once::new();

#[derive(Clone)]
struct LogWriter;

impl std::io::Write for LogWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        LOG_CAPTURE.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter
    }
}

/// Serialises the log assertion against other tests that log.
static LOG_TEST_LOCK: Mutex<()> = Mutex::new(());

fn captured_log() -> String {
    String::from_utf8_lossy(&LOG_CAPTURE.lock().unwrap()).into_owned()
}

fn init_log() {
    LOG_INIT.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(LogWriter)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

#[test]
fn a_failing_put_logs_organization_operation_and_bucket_without_the_key() {
    init_log();
    let _guard = LOG_TEST_LOCK.lock().unwrap();
    LOG_CAPTURE.lock().unwrap().clear();

    let failing = Double::failing("acme-bucket");
    let router = RoutingObjectStore::new(snapshot(None, None, vec![("acme", failing.clone())]));
    router.put("acme", CHEF_BLOB, b"hello").unwrap_err();

    let log = captured_log();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 1, "expected exactly one log line, got: {log}");
    assert!(log.contains("acme"), "org id missing: {log}");
    assert!(log.contains("put"), "operation missing: {log}");
    assert!(log.contains("acme-bucket"), "bucket missing: {log}");
    assert!(
        !log.contains(CHEF_BLOB),
        "the checksum must not be logged: {log}"
    );
    assert!(
        !log.contains(&format!("organization-acme/checksum-{CHEF_BLOB}")),
        "the object key must not be logged: {log}"
    );
    assert!(
        log.contains("<checksum>"),
        "the URL in the backend error must be redacted: {log}"
    );
    assert!(
        log.contains("bucket is down"),
        "the cause must survive: {log}"
    );
}

/// An unmapped organization has no backend bucket; the line must still name
/// the organization and the operation so one broken tenant is attributable.
#[test]
fn a_missing_org_bucket_logs_organization_and_operation() {
    init_log();
    let _guard = LOG_TEST_LOCK.lock().unwrap();
    LOG_CAPTURE.lock().unwrap().clear();

    let router = RoutingObjectStore::new(snapshot(None, None, vec![]));
    router.put("acme", CHEF_BLOB, b"hello").unwrap_err();

    let log = captured_log();
    assert_eq!(log.lines().count(), 1, "expected one log line, got: {log}");
    assert!(log.contains("acme"), "org id missing: {log}");
    assert!(log.contains("put"), "operation missing: {log}");
    assert!(log.contains("no object store bucket"), "{log}");
    assert!(!log.contains(CHEF_BLOB), "checksum leaked: {log}");
}
