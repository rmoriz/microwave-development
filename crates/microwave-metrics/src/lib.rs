//! Process-local metric registry for Microwave Server.
//!
//! The registry owns the counters, gauges, and histograms that `/_stats`
//! renders and that the optional OpenTelemetry recorder receives. It is
//! deliberately dependency-free apart from `http`, so the API crate and the
//! OTLP crate can share one catalogue without either depending on the other.

#![deny(unsafe_code)]

mod histogram;
mod recorder;
mod registry;
mod route;

pub use histogram::{
    HistogramSeries, DEFAULT_SERIES_LIMIT, LATENCY_BUCKETS_SECONDS, OVERFLOW_LABEL, OVERFLOW_ROUTE,
    REQUEST_SERIES_LIMIT,
};
pub use recorder::Recorder;
pub use registry::{
    ConvergeSeries, DependencyState, HttpSeries, KeygenGauges, McpSeries, Metrics, MetricsSnapshot,
    ObjectStoreSeries, PoolGauges, SearchSeries, StoreSeries,
};
pub use route::{metrics_route, organization_label};
