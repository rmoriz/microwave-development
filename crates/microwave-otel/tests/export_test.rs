//! End-to-end export against a stub OTLP/HTTP collector.
//!
//! The collector is a real listener, so this covers the whole path from a
//! registry call through the recorder, the reader, and the exporter.
//!
//! `force_flush` blocks the calling thread, so both tests need the multi-thread
//! runtime: on a current-thread one the stub server would never be polled while
//! the flush waits for its answer.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::Router;
use microwave_config::OtelConfig;
use microwave_metrics::Metrics;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use prost::Message;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// A config that points at `endpoint` and exports fast enough for a test.
fn config_for(endpoint: &str) -> OtelConfig {
    OtelConfig {
        enabled: true,
        endpoint: Some(endpoint.to_string()),
        interval_seconds: 1,
        ..Default::default()
    }
}

#[derive(Clone)]
struct Stub {
    status: u16,
    bodies: mpsc::Sender<Vec<u8>>,
}

async fn collect(State(stub): State<Stub>, body: Bytes) -> StatusCode {
    // Sending before answering means a caller that saw the response can already
    // receive the body.
    let _ = stub.bodies.send(body.to_vec()).await;
    StatusCode::from_u16(stub.status).expect("a valid status")
}

/// Serve `POST /v1/metrics`, answer with `status`, and forward every raw body.
///
/// Both tests in this binary install a global meter provider, so neither the
/// port nor a process-wide counter can be shared: the listener picks an
/// ephemeral port and the assertions tolerate exports the periodic reader
/// sends on its own.
async fn stub_collector(status: u16) -> (String, mpsc::Receiver<Vec<u8>>) {
    let (bodies, received) = mpsc::channel(64);
    let app = Router::new()
        .route("/v1/metrics", post(collect))
        .with_state(Stub { status, bodies });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    // Leaked for the test's lifetime; the other test binds its own port.
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), received)
}

#[tokio::test(flavor = "multi_thread")]
async fn exports_a_request_histogram_to_a_stub_collector() {
    let (endpoint, mut received) = stub_collector(200).await;
    let metrics = Arc::new(Metrics::new());
    let guard =
        microwave_otel::init(&config_for(&endpoint), metrics.clone(), "http", "embedded").unwrap();
    metrics.record_http("/_status", "GET", "acme", 200, Duration::from_millis(3));
    guard.force_flush().await.unwrap();

    // A body the periodic reader sent before the recording is not the one
    // under test, so keep reading until the histogram shows up.
    let mut exported = false;
    for _ in 0..10 {
        let body = received.recv().await.expect("an export");
        let Ok(request) = ExportMetricsServiceRequest::decode(&body[..]) else {
            continue;
        };
        exported = request
            .resource_metrics
            .iter()
            .flat_map(|rm| rm.scope_metrics.iter())
            .flat_map(|sm| sm.metrics.iter())
            .any(|metric| metric.name == "http.server.request.duration");
        if exported {
            break;
        }
    }
    assert!(exported, "the request histogram reached the collector");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_collector_does_not_break_recording() {
    let (endpoint, mut received) = stub_collector(500).await;
    let metrics = Arc::new(Metrics::new());
    let guard =
        microwave_otel::init(&config_for(&endpoint), metrics.clone(), "http", "embedded").unwrap();
    metrics.record_http("/_status", "GET", "_default", 200, Duration::from_millis(1));
    // The flush itself fails; that must not touch the recording or the snapshot.
    assert!(guard.force_flush().await.is_err());
    assert!(received.try_recv().is_ok());
    assert_eq!(metrics.http_series()[0].count, 1);
    assert_eq!(metrics.snapshot().total_requests, 0);
    assert!(
        metrics.export_error().is_some(),
        "a failed export is recorded"
    );
}
