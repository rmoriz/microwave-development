//! In-process request metrics exposed through `GET /_stats`.
//!
//! The counters are deliberately process-local and atomic: the server is a
//! single HTTP process, and the values only need to survive concurrent request
//! handling, not a restart. `/metrics`-style aggregation can scrape the
//! Prometheus rendering from each instance.

use axum::http::StatusCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Atomic counters accumulated by the request middleware.
#[derive(Debug)]
pub struct Metrics {
    started: Instant,
    total_requests: AtomicU64,
    responses_2xx: AtomicU64,
    responses_4xx: AtomicU64,
    responses_5xx: AtomicU64,
    authentication_failures: AtomicU64,
}

/// A point-in-time copy of [`Metrics`], suitable for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub uptime_seconds: u64,
    pub total_requests: u64,
    pub responses_2xx: u64,
    pub responses_4xx: u64,
    pub responses_5xx: u64,
    pub authentication_failures: u64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            total_requests: AtomicU64::new(0),
            responses_2xx: AtomicU64::new(0),
            responses_4xx: AtomicU64::new(0),
            responses_5xx: AtomicU64::new(0),
            authentication_failures: AtomicU64::new(0),
        }
    }

    /// Count a request as soon as it enters the middleware, before it can be
    /// rejected by maintenance mode, version checks, or authentication.
    pub fn record_request(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Classify the final response status.
    pub fn record_response(&self, status: StatusCode) {
        match status.as_u16() / 100 {
            2 => {
                self.responses_2xx.fetch_add(1, Ordering::Relaxed);
            }
            4 => {
                self.responses_4xx.fetch_add(1, Ordering::Relaxed);
            }
            5 => {
                self.responses_5xx.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        if status == StatusCode::UNAUTHORIZED {
            self.authentication_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            uptime_seconds: self.started.elapsed().as_secs(),
            total_requests: self.total_requests.load(Ordering::Relaxed),
            responses_2xx: self.responses_2xx.load(Ordering::Relaxed),
            responses_4xx: self.responses_4xx.load(Ordering::Relaxed),
            responses_5xx: self.responses_5xx.load(Ordering::Relaxed),
            authentication_failures: self.authentication_failures.load(Ordering::Relaxed),
        }
    }
}
