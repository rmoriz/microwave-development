//! Recorder implementation over synchronous OTel instruments.
//!
//! One instrument per catalogue row is registered here on the
//! `microwave-server` meter. Attribute names follow the metric catalogue in
//! `docs/superpowers/specs/2026-11-03-opentelemetry-metrics-design.md`.

#[cfg(test)]
use crate::testing;
use crate::{latency_boundaries, InstrumentSpec, CATALOGUE, HTTP_REQUEST_DURATION};
use microwave_metrics::Recorder;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use opentelemetry::KeyValue;
use std::time::Duration;

/// Synchronous instruments, one per non-observable catalogue row.
pub(crate) const INSTRUMENTS: &[InstrumentSpec] = &[
    InstrumentSpec {
        otlp: HTTP_REQUEST_DURATION,
        prom: "microwave_server_request_duration_seconds",
    },
    InstrumentSpec {
        otlp: "microwave.mcp.tool.calls",
        prom: "microwave_server_mcp_tool_calls_total",
    },
    InstrumentSpec {
        otlp: "microwave.mcp.tool.duration",
        prom: "microwave_server_mcp_tool_duration_seconds",
    },
    InstrumentSpec {
        otlp: "microwave.converge.runs",
        prom: "microwave_server_converge_runs_total",
    },
    InstrumentSpec {
        otlp: "microwave.store.operation.duration",
        prom: "microwave_server_store_operation_duration_seconds",
    },
    InstrumentSpec {
        otlp: "microwave.search.request.duration",
        prom: "microwave_server_search_request_duration_seconds",
    },
    InstrumentSpec {
        otlp: "microwave.object_store.request.duration",
        prom: "microwave_server_object_store_request_duration_seconds",
    },
    InstrumentSpec {
        otlp: "microwave.limits.requests",
        prom: "microwave_server_limits_total",
    },
];

/// The catalogue row for an instrument name.
fn spec(otlp: &'static str) -> &'static InstrumentSpec {
    debug_assert!(CATALOGUE.iter().any(|(name, _)| *name == otlp));
    INSTRUMENTS
        .iter()
        .find(|spec| spec.otlp == otlp)
        .expect("registered instruments are all in the catalogue")
}

const HTTP_METHOD: &str = "http.request.method";
const HTTP_ROUTE: &str = "http.route";
const HTTP_STATUS: &str = "http.response.status_code";
const URL_SCHEME: &str = "url.scheme";
const ORGANIZATION: &str = "microwave.organization";
const DB_SYSTEM: &str = "db.system";
const DB_OPERATION: &str = "db.operation";
const BACKEND: &str = "backend";
const OPERATION: &str = "operation";
const OUTCOME: &str = "outcome";
const TOOL: &str = "tool";
const STATUS: &str = "status";
const SCOPE: &str = "scope";

/// The recorder attached to [`microwave_metrics::Metrics`].
pub(crate) struct OtelRecorder {
    scheme: &'static str,
    http: Histogram<f64>,
    mcp_calls: Counter<u64>,
    mcp_duration: Histogram<f64>,
    converge: Counter<u64>,
    store: Histogram<f64>,
    search: Histogram<f64>,
    object_store: Histogram<f64>,
    limits: Counter<u64>,
}

impl OtelRecorder {
    pub(crate) fn new(meter: &Meter, scheme: &'static str) -> Self {
        Self {
            scheme,
            http: meter
                .f64_histogram(spec(HTTP_REQUEST_DURATION).otlp)
                .with_unit("s")
                .with_description("HTTP server request duration")
                .with_boundaries(latency_boundaries())
                .build(),
            mcp_calls: meter
                .u64_counter(spec("microwave.mcp.tool.calls").otlp)
                .with_description("MCP tool calls")
                .build(),
            mcp_duration: meter
                .f64_histogram(spec("microwave.mcp.tool.duration").otlp)
                .with_unit("s")
                .with_description("MCP tool call duration")
                .with_boundaries(latency_boundaries())
                .build(),
            converge: meter
                .u64_counter(spec("microwave.converge.runs").otlp)
                .with_description("Chef converge runs")
                .build(),
            store: meter
                .f64_histogram(spec("microwave.store.operation.duration").otlp)
                .with_unit("s")
                .with_description("Metadata store operation duration")
                .with_boundaries(latency_boundaries())
                .build(),
            search: meter
                .f64_histogram(spec("microwave.search.request.duration").otlp)
                .with_unit("s")
                .with_description("Search backend request duration")
                .with_boundaries(latency_boundaries())
                .build(),
            object_store: meter
                .f64_histogram(spec("microwave.object_store.request.duration").otlp)
                .with_unit("s")
                .with_description("Object store request duration")
                .with_boundaries(latency_boundaries())
                .build(),
            limits: meter
                .u64_counter(spec("microwave.limits.requests").otlp)
                .with_description("Request limit decisions")
                .build(),
        }
    }
}

impl Recorder for OtelRecorder {
    fn http_request(&self, route: &str, method: &str, org: &str, status: u16, duration: Duration) {
        self.http.record(
            duration.as_secs_f64(),
            &[
                KeyValue::new(HTTP_ROUTE, route.to_string()),
                KeyValue::new(HTTP_METHOD, method.to_string()),
                KeyValue::new(HTTP_STATUS, i64::from(status)),
                KeyValue::new(URL_SCHEME, self.scheme),
                KeyValue::new(ORGANIZATION, org.to_string()),
            ],
        );
    }

    fn mcp_tool(&self, tool: &str, org: &str, outcome: &str, duration: Duration) {
        let attributes = [
            KeyValue::new(TOOL, tool.to_string()),
            KeyValue::new(OUTCOME, outcome.to_string()),
            KeyValue::new(ORGANIZATION, org.to_string()),
        ];
        self.mcp_calls.add(1, &attributes);
        self.mcp_duration
            .record(duration.as_secs_f64(), &attributes);
    }

    fn converge(&self, org: &str, status: &str) {
        self.converge.add(
            1,
            &[
                KeyValue::new(STATUS, status.to_string()),
                KeyValue::new(ORGANIZATION, org.to_string()),
            ],
        );
    }

    fn store_operation(&self, db_system: &str, operation: &str, outcome: &str, duration: Duration) {
        self.store.record(
            duration.as_secs_f64(),
            &[
                KeyValue::new(DB_SYSTEM, db_system.to_string()),
                KeyValue::new(DB_OPERATION, operation.to_string()),
                KeyValue::new(OUTCOME, outcome.to_string()),
            ],
        );
    }

    fn search_request(&self, backend: &str, outcome: &str, duration: Duration) {
        self.search.record(
            duration.as_secs_f64(),
            &[
                KeyValue::new(BACKEND, backend.to_string()),
                KeyValue::new(OUTCOME, outcome.to_string()),
            ],
        );
    }

    fn object_store_request(
        &self,
        backend: &str,
        operation: &str,
        outcome: &str,
        duration: Duration,
    ) {
        self.object_store.record(
            duration.as_secs_f64(),
            &[
                KeyValue::new(BACKEND, backend.to_string()),
                KeyValue::new(OPERATION, operation.to_string()),
                KeyValue::new(OUTCOME, outcome.to_string()),
            ],
        );
    }

    fn limit(&self, scope: &str, org: &str, outcome: &str) {
        self.limits.add(
            1,
            &[
                KeyValue::new(SCOPE, scope.to_string()),
                KeyValue::new(ORGANIZATION, org.to_string()),
                KeyValue::new(OUTCOME, outcome.to_string()),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TrackingExporter;
    use microwave_metrics::{KeygenGauges, Metrics, DEFAULT_SERIES_LIMIT, LATENCY_BUCKETS_SECONDS};
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, Temporality};
    use std::sync::Arc;
    use std::time::Duration;

    /// Pull the attributes of one exported histogram's data points.
    fn attributes(exporter: &InMemoryMetricExporter, name: &str) -> Vec<KeyValue> {
        exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .filter(|metric| metric.name() == name)
            .flat_map(|metric| match metric.data() {
                AggregatedMetrics::F64(MetricData::Histogram(histogram)) => histogram
                    .data_points()
                    .flat_map(|point| point.attributes().cloned())
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect()
    }

    /// Pull the attributes of one exported counter's data points.
    fn counter_attributes(exporter: &InMemoryMetricExporter, name: &str) -> Vec<KeyValue> {
        exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .filter(|metric| metric.name() == name)
            .flat_map(|metric| match metric.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
                    .data_points()
                    .flat_map(|point| point.attributes().cloned())
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect()
    }

    /// The first exported value of one gauge.
    fn gauge_value(exporter: &InMemoryMetricExporter, name: &str) -> Option<u64> {
        exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .find(|metric| metric.name() == name)
            .and_then(|metric| match metric.data() {
                AggregatedMetrics::U64(MetricData::Gauge(gauge)) => {
                    gauge.data_points().next().map(|point| point.value())
                }
                _ => None,
            })
    }

    /// The number of exported data points of one histogram.
    fn point_count(exporter: &InMemoryMetricExporter, name: &str) -> usize {
        exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .filter(|metric| metric.name() == name)
            .map(|metric| match metric.data() {
                AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
                    histogram.data_points().count()
                }
                _ => 0,
            })
            .sum()
    }

    /// The exported bucket bounds of the single data point of `name`.
    fn bounds(exporter: &InMemoryMetricExporter, name: &str) -> Vec<f64> {
        exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .find(|metric| metric.name() == name)
            .and_then(|metric| match metric.data() {
                AggregatedMetrics::F64(MetricData::Histogram(histogram)) => histogram
                    .data_points()
                    .next()
                    .map(|point| point.bounds().collect()),
                _ => None,
            })
            .expect("the instrument exported a data point")
    }

    #[tokio::test]
    async fn records_into_an_in_memory_provider() {
        let metrics = Arc::new(Metrics::new());
        let exporter = opentelemetry_sdk::metrics::InMemoryMetricExporter::default();
        let guard = super::testing::init_with_exporter(metrics.clone(), exporter.clone()).unwrap();
        metrics.record_http(
            "/_status",
            "GET",
            "acme",
            200,
            std::time::Duration::from_millis(4),
        );
        guard.force_flush().await.unwrap();
        let names: Vec<String> = exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| {
                resource
                    .scope_metrics()
                    .flat_map(|scope| scope.metrics().map(|metric| metric.name().to_string()))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(names
            .iter()
            .any(|name| name == "http.server.request.duration"));
        assert!(names.iter().any(|name| name == "microwave.server.requests"));
    }

    #[test]
    fn catalogue_pins_the_names_and_stays_unique() {
        assert!(super::CATALOGUE.contains(&(
            "http.server.request.duration",
            "microwave_server_request_duration_seconds"
        )));
        assert!(super::CATALOGUE.contains(&(
            "microwave.mcp.tool.calls",
            "microwave_server_mcp_tool_calls_total"
        )));
        assert!(super::CATALOGUE
            .contains(&("microwave.limits.requests", "microwave_server_limits_total")));
        assert!(super::CATALOGUE.contains(&(
            "microwave.limits.in_flight",
            "microwave_server_limits_in_flight"
        )));
        assert!(super::CATALOGUE
            .iter()
            .all(|(otlp, prom)| otlp.contains('.') && prom.starts_with("microwave_server_")));
        let mut prom: Vec<&str> = super::CATALOGUE.iter().map(|(_, prom)| *prom).collect();
        prom.sort_unstable();
        prom.dedup();
        assert_eq!(prom.len(), super::CATALOGUE.len());
    }

    #[tokio::test]
    async fn maps_the_request_attributes_and_the_scheme() {
        let metrics = Arc::new(Metrics::new());
        let exporter = InMemoryMetricExporter::default();
        let guard = testing::init_with_exporter(metrics.clone(), exporter.clone()).unwrap();
        metrics.record_http(
            "/organizations/acme/nodes",
            "GET",
            "acme",
            401,
            Duration::from_millis(4),
        );
        guard.force_flush().await.unwrap();

        let attributes: std::collections::HashMap<String, String> =
            attributes(&exporter, "http.server.request.duration")
                .into_iter()
                .map(|kv| (kv.key.to_string(), kv.value.to_string()))
                .collect();
        assert_eq!(attributes["http.route"], "/organizations/acme/nodes");
        assert_eq!(attributes["http.request.method"], "GET");
        assert_eq!(attributes["http.response.status_code"], "401");
        assert_eq!(attributes["microwave.organization"], "acme");
        assert_eq!(attributes["url.scheme"], "http");
        // The per-instrument view must keep the shared bucket boundaries.
        assert_eq!(
            bounds(&exporter, "http.server.request.duration"),
            LATENCY_BUCKETS_SECONDS
        );
    }

    #[tokio::test]
    async fn records_limits_decisions() {
        let metrics = Arc::new(Metrics::new());
        let exporter = InMemoryMetricExporter::default();
        let guard = testing::init_with_exporter(metrics.clone(), exporter.clone()).unwrap();
        metrics.record_limit("org", "acme", "rejected");
        guard.force_flush().await.unwrap();

        let attributes: std::collections::HashMap<String, String> =
            counter_attributes(&exporter, "microwave.limits.requests")
                .into_iter()
                .map(|kv| (kv.key.to_string(), kv.value.to_string()))
                .collect();
        assert!(
            !attributes.is_empty(),
            "{:?}",
            exported_names(&exporter).await
        );
        assert_eq!(attributes["scope"], "org");
        assert_eq!(attributes["microwave.organization"], "acme");
        assert_eq!(attributes["outcome"], "rejected");
    }

    #[tokio::test]
    async fn the_views_cap_only_the_request_histogram() {
        let metrics = Arc::new(Metrics::new());
        let exporter = InMemoryMetricExporter::default();
        let guard = testing::init_with_exporter(metrics.clone(), exporter.clone()).unwrap();

        // One series above the default cap: the registry collapses the overflow
        // itself, so only the view can bring the count back down.
        for index in 0..=DEFAULT_SERIES_LIMIT {
            metrics.record_mcp_tool(&format!("tool-{index}"), "acme", "ok", Duration::ZERO);
        }
        // Above the default cap but below the raised request cap.
        let routes = DEFAULT_SERIES_LIMIT + 100;
        for index in 0..routes {
            metrics.record_http(
                &format!("/routes/{index}"),
                "GET",
                "acme",
                200,
                Duration::ZERO,
            );
        }
        guard.force_flush().await.unwrap();

        assert_eq!(
            point_count(&exporter, "microwave.mcp.tool.duration"),
            DEFAULT_SERIES_LIMIT + 1
        );
        assert_eq!(
            point_count(&exporter, "http.server.request.duration"),
            routes
        );
    }

    async fn exported_names(exporter: &InMemoryMetricExporter) -> Vec<String> {
        exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| {
                resource
                    .scope_metrics()
                    .flat_map(|scope| scope.metrics().map(|metric| metric.name().to_string()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[tokio::test]
    async fn exports_the_registry_gauges() {
        let metrics = Arc::new(Metrics::new());
        let exporter = InMemoryMetricExporter::default();
        let guard = testing::init_with_exporter(metrics.clone(), exporter.clone()).unwrap();
        metrics.set_nodes(3);
        metrics.set_maintenance(true);
        metrics.set_keygen(KeygenGauges {
            cached: 1,
            capacity: 2,
            ..KeygenGauges::default()
        });
        metrics.set_dependencies(vec![microwave_metrics::DependencyState {
            name: "sqlite",
            up: true,
        }]);
        metrics.adjust_limits_in_flight("org", 3);
        guard.force_flush().await.unwrap();

        let names = exported_names(&exporter).await;
        for name in [
            "microwave.server.nodes",
            "microwave.server.maintenance",
            "microwave.server.uptime",
            "microwave.server.build.info",
            "microwave.keygen.cached",
            "microwave.server.dependencies",
            "microwave.limits.in_flight",
        ] {
            assert!(names.iter().any(|found| found == name), "{name}");
        }
        assert_eq!(
            gauge_value(&exporter, "microwave.limits.in_flight"),
            Some(3)
        );
    }

    struct FailingExporter;

    impl PushMetricExporter for FailingExporter {
        async fn export(&self, _metrics: &ResourceMetrics) -> OTelSdkResult {
            Err(opentelemetry_sdk::error::OTelSdkError::InternalFailure(
                "boom".to_string(),
            ))
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }
    }

    struct WorkingExporter;

    impl PushMetricExporter for WorkingExporter {
        async fn export(&self, _metrics: &ResourceMetrics) -> OTelSdkResult {
            Ok(())
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }
    }

    #[tokio::test]
    async fn export_errors_are_tracked_and_cleared() {
        let metrics = Arc::new(Metrics::new());
        let failing = TrackingExporter::new(
            FailingExporter,
            metrics.clone(),
            "http://collector:4318".to_string(),
        );
        failing
            .export(&ResourceMetrics::default())
            .await
            .unwrap_err();
        let recorded = metrics.export_error().expect("the failure is recorded");
        assert!(recorded.contains("http://collector:4318"));
        assert!(recorded.contains("boom"));

        let working = TrackingExporter::new(
            WorkingExporter,
            metrics.clone(),
            "http://collector:4318".to_string(),
        );
        working.export(&ResourceMetrics::default()).await.unwrap();
        assert_eq!(metrics.export_error(), None);
    }

    #[test]
    fn catalogue_matches_the_registered_instruments() {
        let mut names: Vec<&str> = INSTRUMENTS
            .iter()
            .chain(crate::gauges::INSTRUMENTS.iter())
            .map(|spec| spec.otlp)
            .collect();
        let mut catalogue: Vec<&str> = CATALOGUE.iter().map(|(otlp, _)| *otlp).collect();
        names.sort_unstable();
        catalogue.sort_unstable();
        assert_eq!(names, catalogue);
    }
}
