//! Observable gauges and counters backed by the local [`Metrics`] registry.
//!
//! The instrumented process writes the values into the registry (the refresh
//! task in the binary and the `/_stats` handler); the callbacks below only read
//! them, so no store or cache handle is needed here.

use crate::InstrumentSpec;
use microwave_metrics::Metrics;
use opentelemetry::metrics::{AsyncInstrument, Meter};
use opentelemetry::KeyValue;
use std::sync::Arc;

/// Observable catalogue rows, one entry per instrument.
#[allow(dead_code)] // read by the catalogue consistency test
pub(crate) const INSTRUMENTS: &[InstrumentSpec] = &[
    InstrumentSpec {
        otlp: "microwave.server.requests",
        prom: "microwave_server_requests_total",
    },
    InstrumentSpec {
        otlp: "microwave.server.responses",
        prom: "microwave_server_responses_total",
    },
    InstrumentSpec {
        otlp: "microwave.server.authentication.failures",
        prom: "microwave_server_authentication_failures_total",
    },
    InstrumentSpec {
        otlp: "microwave.server.uptime",
        prom: "microwave_server_uptime_seconds",
    },
    InstrumentSpec {
        otlp: "microwave.server.maintenance",
        prom: "microwave_server_maintenance",
    },
    InstrumentSpec {
        otlp: "microwave.server.nodes",
        prom: "microwave_server_nodes",
    },
    InstrumentSpec {
        otlp: "microwave.server.db.pool.connections",
        prom: "microwave_server_db_pool_connections",
    },
    InstrumentSpec {
        otlp: "microwave.server.db.pool.connections.max",
        prom: "microwave_server_db_pool_connections_max",
    },
    InstrumentSpec {
        otlp: "microwave.server.build.info",
        prom: "microwave_server_build_info",
    },
    InstrumentSpec {
        otlp: "microwave.server.dependencies",
        prom: "microwave_server_dependency_up",
    },
    InstrumentSpec {
        otlp: "microwave.keygen.capacity",
        prom: "microwave_server_keygen_capacity",
    },
    InstrumentSpec {
        otlp: "microwave.keygen.cached",
        prom: "microwave_server_keygen_cached",
    },
    InstrumentSpec {
        otlp: "microwave.keygen.workers",
        prom: "microwave_server_keygen_workers",
    },
    InstrumentSpec {
        otlp: "microwave.keygen.worker_failures",
        prom: "microwave_server_keygen_worker_failures",
    },
    InstrumentSpec {
        otlp: "microwave.keygen.warm_up_failures",
        prom: "microwave_server_keygen_warm_up_failures",
    },
    InstrumentSpec {
        otlp: "microwave.keygen.generated_on_demand",
        prom: "microwave_server_keygen_generated_on_demand",
    },
    InstrumentSpec {
        otlp: "microwave.keygen.worker_healthy",
        prom: "microwave_server_keygen_worker_healthy",
    },
];

const STATE: &str = "state";
const DEPENDENCY: &str = "dependency";
const CLASS: &str = "class";
const SERVICE_VERSION: &str = "service.version";
const INDEXING_MODE: &str = "indexing.mode";

/// Register every observable instrument.
pub(crate) fn register(meter: &Meter, metrics: &Arc<Metrics>) {
    let traffic = metrics.clone();
    meter
        .u64_observable_counter("microwave.server.requests")
        .with_description("HTTP requests seen by the middleware")
        .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
            observer.observe(traffic.snapshot().total_requests, &[]);
        })
        .build();

    let responses = metrics.clone();
    meter
        .u64_observable_counter("microwave.server.responses")
        .with_description("HTTP responses by status class")
        .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
            let snapshot = responses.snapshot();
            for (class, value) in [
                ("2xx", snapshot.responses_2xx),
                ("4xx", snapshot.responses_4xx),
                ("5xx", snapshot.responses_5xx),
            ] {
                observer.observe(value, &[KeyValue::new(CLASS, class)]);
            }
        })
        .build();

    let authentication = metrics.clone();
    meter
        .u64_observable_counter("microwave.server.authentication.failures")
        .with_description("Authentication failures")
        .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
            observer.observe(authentication.snapshot().authentication_failures, &[]);
        })
        .build();

    let uptime = metrics.clone();
    meter
        .u64_observable_gauge("microwave.server.uptime")
        .with_unit("s")
        .with_description("Process uptime")
        .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
            observer.observe(uptime.uptime_seconds(), &[]);
        })
        .build();

    let maintenance = metrics.clone();
    meter
        .u64_observable_gauge("microwave.server.maintenance")
        .with_description("Maintenance mode is on")
        .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
            observer.observe(u64::from(maintenance.maintenance()), &[]);
        })
        .build();

    let nodes = metrics.clone();
    meter
        .u64_observable_gauge("microwave.server.nodes")
        .with_description("Stored nodes")
        .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
            observer.observe(nodes.nodes(), &[]);
        })
        .build();

    let pool = metrics.clone();
    meter
        .u64_observable_gauge("microwave.server.db.pool.connections")
        .with_description("Database pool connections by state")
        .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
            if let Some(gauges) = pool.pool() {
                observer.observe(u64::from(gauges.idle), &[KeyValue::new(STATE, "idle")]);
                observer.observe(u64::from(gauges.in_use), &[KeyValue::new(STATE, "in_use")]);
            }
        })
        .build();

    let pool_max = metrics.clone();
    meter
        .u64_observable_gauge("microwave.server.db.pool.connections.max")
        .with_description("Database pool maximum connections")
        .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
            if let Some(gauges) = pool_max.pool() {
                observer.observe(u64::from(gauges.max), &[]);
            }
        })
        .build();

    meter
        .u64_observable_gauge("microwave.server.build.info")
        .with_description("Build information")
        .with_callback(|observer: &dyn AsyncInstrument<u64>| {
            observer.observe(
                1,
                &[
                    KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                    KeyValue::new(INDEXING_MODE, indexing_mode()),
                ],
            );
        })
        .build();

    let dependencies = metrics.clone();
    meter
        .u64_observable_gauge("microwave.server.dependencies")
        .with_description("Dependency probes, 1 when up")
        .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
            for dependency in dependencies.dependencies() {
                observer.observe(
                    u64::from(dependency.up),
                    &[
                        KeyValue::new(DEPENDENCY, dependency.name),
                        KeyValue::new(STATE, if dependency.up { "up" } else { "down" }),
                    ],
                );
            }
        })
        .build();

    let keygen = metrics.clone();
    for (name, read) in [
        (
            "microwave.keygen.capacity",
            (|gauges: microwave_metrics::KeygenGauges| gauges.capacity as u64)
                as fn(microwave_metrics::KeygenGauges) -> u64,
        ),
        ("microwave.keygen.cached", |gauges| gauges.cached as u64),
        ("microwave.keygen.workers", |gauges| gauges.workers as u64),
        ("microwave.keygen.worker_failures", |gauges| {
            gauges.worker_failures
        }),
        ("microwave.keygen.warm_up_failures", |gauges| {
            gauges.warm_up_failures
        }),
        ("microwave.keygen.generated_on_demand", |gauges| {
            gauges.generated_on_demand
        }),
        ("microwave.keygen.worker_healthy", |gauges| {
            u64::from(gauges.worker_healthy)
        }),
    ] {
        let keygen = keygen.clone();
        meter
            .u64_observable_gauge(name)
            .with_description("Keygen cache state")
            .with_callback(move |observer: &dyn AsyncInstrument<u64>| {
                observer.observe(read(keygen.keygen()), &[]);
            })
            .build();
    }
}

/// Indexing backend name for the build-info series.
///
/// `init` only receives the `[otel]` block, so the environment copy of
/// `MW_SEARCH__BACKEND` is the best available source; the embedded backend is
/// the default.
fn indexing_mode() -> String {
    std::env::var("MW_SEARCH__BACKEND").unwrap_or_else(|_| "embedded".to_string())
}
