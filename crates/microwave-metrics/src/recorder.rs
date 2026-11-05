//! Sink trait for measured events.
//!
//! The crate itself only keeps process-local counters. An optional
//! implementation installed with [`crate::Metrics::attach`] receives the same
//! events and forwards them to a second sink, such as OpenTelemetry.

use std::time::Duration;

/// Receives every recorded event so an external sink can mirror it.
pub trait Recorder: Send + Sync {
    fn http_request(&self, route: &str, method: &str, org: &str, status: u16, duration: Duration);
    fn mcp_tool(&self, tool: &str, org: &str, outcome: &str, duration: Duration);
    fn converge(&self, org: &str, status: &str);
    fn store_operation(&self, db_system: &str, operation: &str, outcome: &str, duration: Duration);
    fn search_request(&self, backend: &str, outcome: &str, duration: Duration);
    fn object_store_request(
        &self,
        backend: &str,
        operation: &str,
        outcome: &str,
        duration: Duration,
    );
    fn limit(&self, scope: &str, org: &str, outcome: &str);
}
