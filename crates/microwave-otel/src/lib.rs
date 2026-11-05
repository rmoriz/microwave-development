//! OpenTelemetry (OTLP/HTTP) metrics for Microwave Server.
//!
//! This is the only crate that names an `opentelemetry` type. The API crate
//! hands over its [`Metrics`] registry and gets a recorder attached to it; the
//! registry keeps rendering the local `/_stats` view at the same time, so both
//! sinks share one catalogue.
//!
//! The provider never dials at [`init`] time. Only the periodic reader touches
//! the network, in the background.

#![deny(unsafe_code)]

use microwave_config::OtelConfig;
use microwave_metrics::{
    Metrics, DEFAULT_SERIES_LIMIT, LATENCY_BUCKETS_SECONDS, REQUEST_SERIES_LIMIT,
};
use opentelemetry::metrics::MeterProvider;
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::{MetricExporter, Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{Instrument, SdkMeterProvider, Stream, Temporality};
use opentelemetry_sdk::runtime::Runtime;
use opentelemetry_sdk::Resource;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

mod gauges;
mod recorder;
pub mod testing;

use recorder::OtelRecorder;

/// Instrumentation scope for every instrument this crate registers.
const METER_NAME: &str = "microwave-server";

/// The request histogram, the only instrument with the raised series cap.
pub(crate) const HTTP_REQUEST_DURATION: &str = "http.server.request.duration";

/// OTLP instrument name to Prometheus exposition name.
///
/// The right-hand side is what the collector's Prometheus exporter (or the
/// `/_stats` parity view) spells; both derive from the same instrument.
pub const CATALOGUE: &[(&str, &str)] = &[
    (
        HTTP_REQUEST_DURATION,
        "microwave_server_request_duration_seconds",
    ),
    (
        "microwave.server.requests",
        "microwave_server_requests_total",
    ),
    (
        "microwave.server.responses",
        "microwave_server_responses_total",
    ),
    (
        "microwave.server.authentication.failures",
        "microwave_server_authentication_failures_total",
    ),
    ("microwave.server.uptime", "microwave_server_uptime_seconds"),
    (
        "microwave.server.maintenance",
        "microwave_server_maintenance",
    ),
    ("microwave.server.nodes", "microwave_server_nodes"),
    (
        "microwave.server.db.pool.connections",
        "microwave_server_db_pool_connections",
    ),
    (
        "microwave.server.db.pool.connections.max",
        "microwave_server_db_pool_connections_max",
    ),
    ("microwave.server.build.info", "microwave_server_build_info"),
    (
        "microwave.server.dependencies",
        "microwave_server_dependency_up",
    ),
    (
        "microwave.keygen.capacity",
        "microwave_server_keygen_capacity",
    ),
    ("microwave.keygen.cached", "microwave_server_keygen_cached"),
    (
        "microwave.keygen.workers",
        "microwave_server_keygen_workers",
    ),
    (
        "microwave.keygen.worker_failures",
        "microwave_server_keygen_worker_failures",
    ),
    (
        "microwave.keygen.warm_up_failures",
        "microwave_server_keygen_warm_up_failures",
    ),
    (
        "microwave.keygen.generated_on_demand",
        "microwave_server_keygen_generated_on_demand",
    ),
    (
        "microwave.keygen.worker_healthy",
        "microwave_server_keygen_worker_healthy",
    ),
    (
        "microwave.mcp.tool.calls",
        "microwave_server_mcp_tool_calls_total",
    ),
    (
        "microwave.mcp.tool.duration",
        "microwave_server_mcp_tool_duration_seconds",
    ),
    (
        "microwave.converge.runs",
        "microwave_server_converge_runs_total",
    ),
    (
        "microwave.store.operation.duration",
        "microwave_server_store_operation_duration_seconds",
    ),
    (
        "microwave.search.request.duration",
        "microwave_server_search_request_duration_seconds",
    ),
    (
        "microwave.object_store.request.duration",
        "microwave_server_object_store_request_duration_seconds",
    ),
    ("microwave.limits.requests", "microwave_server_limits_total"),
    (
        "microwave.limits.in_flight",
        "microwave_server_limits_in_flight",
    ),
];

/// One catalogue row as the recorder and gauge modules register it.
pub(crate) struct InstrumentSpec {
    pub(crate) otlp: &'static str,
    /// Only tests read the exposition spelling; the collector derives it.
    #[allow(dead_code)]
    pub(crate) prom: &'static str,
}

/// Why initialization or a lifecycle call failed.
#[derive(Debug, Error)]
pub enum OtelError {
    /// The `[otel]` block does not validate on its own.
    #[error("invalid otel configuration: {0}")]
    Config(String),
    /// The OTLP exporter could not be built from the endpoint or headers.
    #[error("could not build the OTLP exporter: {0}")]
    Exporter(#[from] opentelemetry_otlp::ExporterBuildError),
    /// The HTTP client for the private CA or the skip-verify flag failed.
    #[error("could not build the TLS HTTP client: {0}")]
    Tls(String),
    /// The registry already has a recorder, so a second provider would export
    /// nothing.
    #[error("a metrics recorder is already attached")]
    AlreadyAttached,
    /// The reader's own runtime could not be created.
    #[error("could not start the metric reader runtime: {0}")]
    Reader(String),
    /// A forced flush failed.
    #[error("could not flush metrics: {0}")]
    Flush(String),
    /// The final flush did not finish inside `shutdown_timeout_ms`.
    #[error("metric export shutdown exceeded the {0:?} budget")]
    ShutdownTimeout(Duration),
    /// Shutdown itself failed.
    #[error("metric export shutdown failed: {0}")]
    Shutdown(String),
}

/// Keeps the provider alive and shuts it down under the configured budget.
#[derive(Debug)]
pub struct OtelGuard {
    provider: SdkMeterProvider,
    shutdown_timeout: Duration,
}

impl OtelGuard {
    /// Flush every pending measurement into the reader.
    pub async fn force_flush(&self) -> Result<(), OtelError> {
        self.provider
            .force_flush()
            .map_err(|error| OtelError::Flush(error.to_string()))
    }

    /// Flush and release the provider within `shutdown_timeout_ms`.
    ///
    /// The inner `shutdown_with_timeout` is the real budget: the outer timeout
    /// cannot preempt a call that never yields.
    pub async fn shutdown(self) -> Result<(), OtelError> {
        let timeout = self.shutdown_timeout;
        let provider = self.provider;
        match tokio::time::timeout(timeout, async { provider.shutdown_with_timeout(timeout) }).await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(OtelError::Shutdown(error.to_string())),
            Err(_) => Err(OtelError::ShutdownTimeout(timeout)),
        }
    }
}

/// Wraps an exporter so the registry learns the outcome of every export.
///
/// The API crate never sees an OTel type: a failed export lands in
/// [`Metrics::set_export_error`] (rendered by `/_stats`) and a successful one
/// clears it again.
struct TrackingExporter<E> {
    inner: E,
    metrics: Arc<Metrics>,
    endpoint: String,
}

impl<E> TrackingExporter<E> {
    fn new(inner: E, metrics: Arc<Metrics>, endpoint: String) -> Self {
        Self {
            inner,
            metrics,
            endpoint,
        }
    }
}

fn error_class(error: &OTelSdkError) -> &'static str {
    match error {
        OTelSdkError::AlreadyShutdown => "already_shutdown",
        OTelSdkError::Timeout(_) => "timeout",
        OTelSdkError::InternalFailure(_) => "internal",
    }
}

impl<E: PushMetricExporter> PushMetricExporter for TrackingExporter<E> {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        match self.inner.export(metrics).await {
            Ok(()) => {
                self.metrics.clear_export_error();
                Ok(())
            }
            Err(error) => {
                self.metrics
                    .set_export_error(format!("{}: {error}", self.endpoint));
                tracing::warn!(
                    endpoint = %self.endpoint,
                    error.class = error_class(&error),
                    error = %error,
                    "OTLP metric export failed"
                );
                Err(error)
            }
        }
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.inner.shutdown()
    }

    fn temporality(&self) -> Temporality {
        self.inner.temporality()
    }
}

/// Build the provider, attach the recorder, register the gauges, and install
/// the provider globally.
///
/// `scheme` becomes the `url.scheme` attribute on the request histogram; only
/// the binary knows whether the listener terminates TLS. `indexing_mode` is the
/// resolved search backend, exported on the build-info series.
///
/// A registry takes one recorder, so a second call on the same [`Metrics`]
/// fails with [`OtelError::AlreadyAttached`] after shutting the fresh provider
/// down again.
pub fn init(
    config: &OtelConfig,
    metrics: Arc<Metrics>,
    scheme: &'static str,
    indexing_mode: &'static str,
) -> Result<OtelGuard, OtelError> {
    config
        .validate()
        .map_err(|(key, message)| OtelError::Config(format!("{key}: {message}")))?;

    let endpoint = config
        .endpoint
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let signal_endpoint = signal_endpoint(endpoint);
    let exporter = build_exporter(config, endpoint, &signal_endpoint)?;
    let reader = PeriodicReader::builder(
        TrackingExporter::new(
            exporter,
            metrics.clone(),
            redacted_endpoint(&signal_endpoint),
        ),
        ReaderRuntime::new()?,
    )
    .with_interval(Duration::from_secs(config.interval_seconds))
    .with_timeout(Duration::from_millis(config.timeout_ms))
    .build();

    build(config, metrics, scheme, indexing_mode, reader)
}

/// The OTLP/HTTP metrics path below a configured base URL.
///
/// An endpoint that already names the signal path is not appended twice.
fn signal_endpoint(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    let base = base.strip_suffix("/v1/metrics").unwrap_or(base);
    format!("{}/v1/metrics", base.trim_end_matches('/'))
}

/// The endpoint as it may be printed: the `user:pass@` userinfo is dropped.
fn redacted_endpoint(endpoint: &str) -> String {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return endpoint.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let Some(at) = rest[..authority_end].rfind('@') else {
        return endpoint.to_string();
    };
    format!("{scheme}://{}", &rest[at + 1..])
}

fn build_exporter(
    config: &OtelConfig,
    endpoint: &str,
    signal_endpoint: &str,
) -> Result<MetricExporter, OtelError> {
    let timeout = Duration::from_millis(config.timeout_ms);
    let headers: HashMap<String, String> = config
        .headers_parsed()
        .map_err(OtelError::Config)?
        .into_iter()
        .collect();

    let mut builder = MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_timeout(timeout)
        .with_headers(headers);
    if !endpoint.is_empty() {
        builder = builder.with_endpoint(signal_endpoint);
    }

    if config.tls_ca_path.is_some() || config.insecure_skip_tls_verify {
        builder = builder.with_http_client(http_client(config, timeout)?);
    }

    Ok(builder.build()?)
}

/// A reqwest client carrying the private CA and the skip-verify switch.
///
/// The exporter's own client builder has no hook for either, so TLS options
/// need a supplied client.
fn http_client(config: &OtelConfig, timeout: Duration) -> Result<reqwest::Client, OtelError> {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    if let Some(path) = &config.tls_ca_path {
        let pem = std::fs::read(path)
            .map_err(|error| OtelError::Tls(format!("{}: {error}", path.display())))?;
        let certificate = reqwest::Certificate::from_pem(&pem)
            .map_err(|error| OtelError::Tls(format!("{}: {error}", path.display())))?;
        builder = builder.add_root_certificate(certificate);
    }
    if config.insecure_skip_tls_verify {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder
        .build()
        .map_err(|error| OtelError::Tls(error.to_string()))
}

fn resource(config: &OtelConfig) -> Resource {
    let mut builder = Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attributes([
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new("service.instance.id", uuid::Uuid::new_v4().to_string()),
            KeyValue::new("host.name", host_name()),
        ]);
    if let Some(environment) = &config.environment {
        builder =
            builder.with_attribute(KeyValue::new("deployment.environment", environment.clone()));
    }
    builder.build()
}

fn host_name() -> String {
    // SAFETY: `gethostname` writes at most `buffer.len()` bytes into `buffer`
    // and does not retain the pointer; the buffer outlives the call.
    #[allow(unsafe_code)]
    let probed = unsafe {
        let mut buffer = [0_u8; 256];
        let status = libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len());
        if status != 0 {
            None
        } else {
            let end = buffer
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(buffer.len());
            std::str::from_utf8(&buffer[..end]).ok().map(str::to_string)
        }
    };
    if let Some(name) = probed.filter(|name| !name.trim().is_empty()) {
        return name;
    }
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(value) = std::fs::read_to_string(path) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn build<R: MetricReader>(
    config: &OtelConfig,
    metrics: Arc<Metrics>,
    scheme: &'static str,
    indexing_mode: &'static str,
    reader: R,
) -> Result<OtelGuard, OtelError> {
    let mut builder = SdkMeterProvider::builder()
        .with_resource(resource(config))
        .with_reader(reader);

    // One view per instrument keeps the series cap of each family. The request
    // histogram gets the raised limit because organization multiplies its set.
    for spec in CATALOGUE {
        let limit = if spec.0 == HTTP_REQUEST_DURATION {
            REQUEST_SERIES_LIMIT
        } else {
            DEFAULT_SERIES_LIMIT
        };
        builder = builder.with_view(move |instrument: &Instrument| {
            if instrument.name() != spec.0 {
                return None;
            }
            Stream::builder().with_cardinality_limit(limit).build().ok()
        });
    }

    let provider = builder.build();
    let meter = provider.meter(METER_NAME);

    let recorder = OtelRecorder::new(&meter, scheme);
    if !metrics.attach(Arc::new(recorder)) {
        // The fresh provider has no recorder to feed and nothing was installed
        // globally yet: shut it down instead of leaving a second, empty export
        // path behind.
        tracing::warn!("a metrics recorder is already attached; dropping the new provider");
        if let Err(error) = provider.shutdown() {
            tracing::warn!(%error, "shutting down the unused provider failed");
        }
        return Err(OtelError::AlreadyAttached);
    }

    gauges::register(&meter, &metrics, indexing_mode);

    global::set_meter_provider(provider.clone());

    Ok(OtelGuard {
        provider,
        shutdown_timeout: Duration::from_millis(config.shutdown_timeout_ms),
    })
}

/// A private runtime for the periodic reader.
///
/// The reader blocks while it collects and exports, and its shutdown is a
/// blocking call. Running the worker on its own runtime keeps `force_flush`,
/// `shutdown`, and a plain guard drop working from any caller runtime,
/// including a current-thread one, and never stalls the server's reactor.
///
/// The runtime itself lives on that dedicated thread: dropping a runtime from
/// async code panics, so [`Drop`] only signals the thread and lets it wind down
/// on its own.
struct ReaderRuntime {
    handle: tokio::runtime::Handle,
    stop: Option<std::sync::mpsc::Sender<()>>,
}

impl ReaderRuntime {
    fn new() -> Result<Self, OtelError> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("microwave-otel-reader".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build();
                let runtime = match runtime {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(runtime.handle().clone()));
                runtime.block_on(async {
                    let _ = tokio::task::spawn_blocking(move || stop_rx.recv()).await;
                });
                // The runtime drops here, on its own thread.
            })
            .map_err(|error| OtelError::Reader(error.to_string()))?;
        let handle = ready_rx
            .recv()
            .map_err(|error| OtelError::Reader(error.to_string()))?
            .map_err(OtelError::Reader)?;
        Ok(Self {
            handle,
            stop: Some(stop_tx),
        })
    }
}

impl Clone for ReaderRuntime {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            stop: self.stop.clone(),
        }
    }
}

impl Drop for ReaderRuntime {
    fn drop(&mut self) {
        drop(self.stop.take());
    }
}

impl Runtime for ReaderRuntime {
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.handle.spawn(future);
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send + 'static {
        tokio::time::sleep(duration)
    }
}

/// Request histogram boundaries, shared with the local registry.
pub(crate) fn latency_boundaries() -> Vec<f64> {
    LATENCY_BUCKETS_SECONDS.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_signal_path_is_appended_to_the_base_url() {
        assert_eq!(
            signal_endpoint("http://collector:4318"),
            "http://collector:4318/v1/metrics"
        );
        assert_eq!(
            signal_endpoint("https://collector.example/"),
            "https://collector.example/v1/metrics"
        );
        // A path that already names the signal is not appended twice.
        assert_eq!(
            signal_endpoint("https://collector.example/v1/metrics"),
            "https://collector.example/v1/metrics"
        );
        assert_eq!(
            signal_endpoint("https://collector.example/v1/metrics/"),
            "https://collector.example/v1/metrics"
        );
    }

    #[test]
    fn the_redacted_endpoint_drops_the_userinfo_only() {
        assert_eq!(
            redacted_endpoint("https://collector:4318/v1/metrics"),
            "https://collector:4318/v1/metrics"
        );
        assert_eq!(
            redacted_endpoint("https://otel:secret@collector:4318/v1/metrics"),
            "https://collector:4318/v1/metrics"
        );
        assert_eq!(
            redacted_endpoint("https://otel:sec@ret@collector/v1/metrics"),
            "https://collector/v1/metrics"
        );
        assert_eq!(redacted_endpoint("collector:4318"), "collector:4318");
    }

    #[tokio::test]
    async fn a_second_init_reports_the_already_attached_recorder() {
        use opentelemetry_sdk::metrics::InMemoryMetricExporter;

        let metrics = Arc::new(Metrics::new());
        let _guard =
            testing::init_with_exporter(metrics.clone(), InMemoryMetricExporter::default())
                .unwrap();
        let error =
            testing::init_with_exporter(metrics, InMemoryMetricExporter::default()).unwrap_err();
        assert!(matches!(error, OtelError::AlreadyAttached), "{error}");
    }

    #[tokio::test]
    async fn init_rejects_an_enabled_block_without_an_endpoint() {
        let config = OtelConfig {
            enabled: true,
            ..Default::default()
        };
        let error = init(&config, Arc::new(Metrics::new()), "http", "embedded").unwrap_err();
        assert!(matches!(error, OtelError::Config(_)), "{error}");
    }

    #[tokio::test]
    async fn init_fails_on_a_missing_ca_file() {
        let path = std::env::temp_dir().join(format!("mw-otel-{}.pem", uuid::Uuid::new_v4()));
        let config = OtelConfig {
            enabled: true,
            endpoint: Some("https://collector.example".to_string()),
            tls_ca_path: Some(path),
            ..Default::default()
        };
        let error = init(&config, Arc::new(Metrics::new()), "https", "embedded").unwrap_err();
        assert!(matches!(error, OtelError::Config(_)), "{error}");
    }

    #[tokio::test]
    async fn init_fails_on_an_unreadable_ca_file() {
        let path = std::env::temp_dir().join(format!("mw-otel-{}.pem", uuid::Uuid::new_v4()));
        std::fs::write(&path, "not a certificate").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            // root ignores the mode bits, so the case cannot be reproduced.
            return;
        }
        let config = OtelConfig {
            enabled: true,
            endpoint: Some("https://collector.example".to_string()),
            tls_ca_path: Some(path.clone()),
            ..Default::default()
        };
        let error = init(&config, Arc::new(Metrics::new()), "https", "embedded").unwrap_err();
        assert!(matches!(error, OtelError::Tls(_)), "{error}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_failed_export_reports_the_endpoint_without_credentials() {
        let metrics = Arc::new(Metrics::new());
        let config = OtelConfig {
            enabled: true,
            endpoint: Some("http://otel:s3cret@127.0.0.1:1".to_string()),
            timeout_ms: 500,
            ..Default::default()
        };
        let guard = init(&config, metrics.clone(), "http", "embedded").unwrap();
        metrics.record_http("/", "GET", "acme", 200, Duration::from_millis(1));
        let _ = guard.force_flush().await;

        let recorded = metrics.export_error().expect("the failure is recorded");
        assert!(recorded.contains("127.0.0.1:1"), "{recorded}");
        assert!(!recorded.contains('@'), "{recorded}");
        assert!(!recorded.contains("s3cret"), "{recorded}");
    }

    #[test]
    fn export_error_classes_stay_bounded() {
        assert_eq!(
            error_class(&OTelSdkError::AlreadyShutdown),
            "already_shutdown"
        );
        assert_eq!(
            error_class(&OTelSdkError::Timeout(Duration::from_secs(1))),
            "timeout"
        );
        assert_eq!(
            error_class(&OTelSdkError::InternalFailure("boom".into())),
            "internal"
        );
    }
}
