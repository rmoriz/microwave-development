//! Construction helpers for tests.
//!
//! A plain module rather than `#[cfg(test)]` so integration tests can use it
//! too. It shares the whole provider construction path with [`crate::init`],
//! only the reader differs: an in-memory exporter instead of OTLP.

use crate::{build, OtelError, OtelGuard, ReaderRuntime, TrackingExporter};
use microwave_config::OtelConfig;
use microwave_metrics::Metrics;
use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
use opentelemetry_sdk::metrics::InMemoryMetricExporter;
use std::sync::Arc;
use std::time::Duration;

/// Build a provider that exports into `exporter` on flush.
pub fn init_with_exporter(
    metrics: Arc<Metrics>,
    exporter: InMemoryMetricExporter,
) -> Result<OtelGuard, OtelError> {
    let config = OtelConfig::default();
    let reader = PeriodicReader::builder(
        TrackingExporter::new(exporter, metrics.clone(), "in-memory".to_string()),
        ReaderRuntime::new()?,
    )
    // Long enough that only an explicit force_flush exports.
    .with_interval(Duration::from_secs(3_600))
    .build();
    build(&config, metrics, "http", reader)
}
