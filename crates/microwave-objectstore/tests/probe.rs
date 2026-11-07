//! Integration tests for the pre-flight bucket probe.
//!
//! A local mock S3 records every `HEAD <bucket>` and the peak number of
//! requests that were in flight at the same time, so no network or Docker is
//! required.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::Router;
use microwave_config::{ObjectStoreBackend, ObjectStoreConfig};
use microwave_objectstore::{
    probe_endpoint, verify_buckets, ObjectStore, ObjectStoreError, S3Config, S3ObjectStore,
    TenantSnapshot,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Mock S3 that answers every bucket request with 200 unless the path is
/// marked as missing, and holds each request for `delay_ms`.
#[derive(Debug, Clone)]
struct Mock {
    inner: Arc<MockInner>,
}

#[derive(Debug, Default)]
struct MockInner {
    requests: Mutex<Vec<String>>,
    failing: Mutex<HashSet<String>>,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
    delay_ms: AtomicU64,
}

impl Mock {
    fn requests(&self) -> Vec<String> {
        self.inner.requests.lock().unwrap().clone()
    }

    fn fail(&self, bucket: &str) {
        self.inner
            .failing
            .lock()
            .unwrap()
            .insert(format!("/{bucket}"));
    }

    fn peak_in_flight(&self) -> usize {
        self.inner.peak_in_flight.load(Ordering::SeqCst)
    }
}

async fn handler(State(mock): State<Mock>, request: Request) -> StatusCode {
    let path = request.uri().path().to_string();
    let in_flight = mock.inner.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    mock.inner
        .peak_in_flight
        .fetch_max(in_flight, Ordering::SeqCst);
    mock.inner
        .requests
        .lock()
        .unwrap()
        .push(format!("{} {path}", request.method()));

    let delay_ms = mock.inner.delay_ms.load(Ordering::SeqCst);
    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
    mock.inner.in_flight.fetch_sub(1, Ordering::SeqCst);

    if mock.inner.failing.lock().unwrap().contains(&path) {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::OK
    }
}

async fn start_mock(delay_ms: u64) -> (String, Mock) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mock = Mock {
        inner: Arc::default(),
    };
    mock.inner.delay_ms.store(delay_ms, Ordering::SeqCst);
    let app = Router::new().fallback(handler).with_state(mock.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), mock)
}

fn s3_config(endpoint: &str, bucket: &str) -> S3Config {
    S3Config::new(endpoint, bucket, "garage", "AKIAEXAMPLE", "SECRETEXAMPLE")
}

fn store(endpoint: &str, bucket: &str) -> Arc<dyn ObjectStore + Send + Sync> {
    Arc::new(S3ObjectStore::new(s3_config(endpoint, bucket)).unwrap())
}

/// Probe on/off has no other configuration job here.
fn probe_config(verify: bool) -> ObjectStoreConfig {
    ObjectStoreConfig {
        backend: ObjectStoreBackend::S3,
        verify_buckets: verify,
        ..ObjectStoreConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reachable_buckets_pass_the_probe() {
    let (endpoint, mock) = start_mock(0).await;
    let mut snapshot = TenantSnapshot {
        default: Some(store(&endpoint, "chef")),
        health: Some(store(&endpoint, "health-bucket")),
        ..TenantSnapshot::default()
    };
    snapshot
        .by_org
        .insert("acme".into(), store(&endpoint, "acme-bucket"));

    verify_buckets(&probe_config(true), &snapshot)
        .await
        .unwrap();

    let mut requests = mock.requests();
    requests.sort();
    assert_eq!(
        requests,
        ["HEAD /acme-bucket", "HEAD /chef", "HEAD /health-bucket"]
    );
}

/// In strict mode the default store is absent, so the health bucket is the
/// only probe target of a fresh install and must actually be probed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_health_bucket_is_probed_when_it_is_the_only_target() {
    let (endpoint, mock) = start_mock(0).await;
    let snapshot = TenantSnapshot {
        health: Some(store(&endpoint, "health-bucket")),
        ..TenantSnapshot::default()
    };

    verify_buckets(&probe_config(true), &snapshot)
        .await
        .unwrap();

    assert_eq!(mock.requests(), ["HEAD /health-bucket"]);
}

/// A health bucket equal to the default bucket is the same target and gets one
/// request, not two.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_health_bucket_equal_to_the_default_is_probed_once() {
    let (endpoint, mock) = start_mock(0).await;
    let snapshot = TenantSnapshot {
        default: Some(store(&endpoint, "chef")),
        health: Some(store(&endpoint, "chef")),
        ..TenantSnapshot::default()
    };

    verify_buckets(&probe_config(true), &snapshot)
        .await
        .unwrap();

    assert_eq!(mock.requests(), ["HEAD /chef"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_bucket_rejects_the_snapshot() {
    let (endpoint, mock) = start_mock(0).await;
    mock.fail("typo-bucket");
    let mut snapshot = TenantSnapshot {
        default: Some(store(&endpoint, "chef")),
        ..TenantSnapshot::default()
    };
    snapshot
        .by_org
        .insert("broken".into(), store(&endpoint, "typo-bucket"));

    let error = verify_buckets(&probe_config(true), &snapshot)
        .await
        .expect_err("a 404 must reject the snapshot");

    match error {
        ObjectStoreError::BucketProbe { bucket, source } => {
            assert_eq!(bucket, "typo-bucket");
            assert!(matches!(*source, ObjectStoreError::NotFound));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disabled_probe_makes_no_request() {
    let (endpoint, mock) = start_mock(0).await;
    let mut snapshot = TenantSnapshot {
        default: Some(store(&endpoint, "chef")),
        health: Some(store(&endpoint, "health")),
        ..TenantSnapshot::default()
    };
    snapshot
        .by_org
        .insert("acme".into(), store(&endpoint, "acme"));

    verify_buckets(&probe_config(false), &snapshot)
        .await
        .unwrap();

    assert!(mock.requests().is_empty(), "{:?}", mock.requests());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn twenty_buckets_never_exceed_eight_probes() {
    let (endpoint, mock) = start_mock(50).await;
    let mut snapshot = TenantSnapshot::default();
    for index in 0..20 {
        snapshot.by_org.insert(
            format!("org-{index}"),
            store(&endpoint, &format!("bucket-{index}")),
        );
    }

    verify_buckets(&probe_config(true), &snapshot)
        .await
        .unwrap();

    assert_eq!(mock.requests().len(), 20);
    let peak = mock.peak_in_flight();
    assert!(peak <= 8, "{peak} probes ran at once");
    assert!(peak > 1, "the probes ran one after another");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hanging_bucket_is_bounded_to_one_and_a_half_seconds() {
    let (endpoint, _mock) = start_mock(2_500).await;
    let snapshot = TenantSnapshot {
        default: Some(store(&endpoint, "chef")),
        ..TenantSnapshot::default()
    };

    let started = Instant::now();
    let error = verify_buckets(&probe_config(true), &snapshot)
        .await
        .expect_err("a hanging bucket must time out");
    let elapsed = started.elapsed();

    assert!(matches!(error, ObjectStoreError::BucketProbe { .. }));
    assert!(elapsed >= Duration::from_millis(1_400), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(2_400), "{elapsed:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_endpoint_uses_the_given_endpoint_and_bucket() {
    let (endpoint, mock) = start_mock(0).await;
    // The base config points somewhere else; only its scalar fields carry over.
    let base = s3_config("http://127.0.0.1:1", "ignored");

    probe_endpoint(&endpoint, "probed-bucket", &base)
        .await
        .unwrap();

    assert_eq!(mock.requests(), ["HEAD /probed-bucket"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_endpoint_reports_a_missing_bucket() {
    let (endpoint, mock) = start_mock(0).await;
    mock.fail("typo-bucket");

    let error = probe_endpoint(&endpoint, "typo-bucket", &s3_config(&endpoint, "ignored"))
        .await
        .expect_err("a 404 must be reported");

    match error {
        ObjectStoreError::BucketProbe { bucket, source } => {
            assert_eq!(bucket, "typo-bucket");
            assert!(matches!(*source, ObjectStoreError::NotFound));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_endpoint_rejects_a_blank_bucket_without_a_request() {
    let (endpoint, mock) = start_mock(0).await;

    let error = probe_endpoint(&endpoint, "  ", &s3_config(&endpoint, "ignored"))
        .await
        .expect_err("a blank bucket is invalid");

    assert!(matches!(error, ObjectStoreError::InvalidConfig(_)));
    assert!(mock.requests().is_empty());
}
