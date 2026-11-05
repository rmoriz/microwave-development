//! Process-local metric registry.
//!
//! The counters are deliberately process-local and atomic: the server is a
//! single HTTP process, and the values only need to survive concurrent request
//! handling, not a restart. Scrapers render `/metrics`-style text from this
//! registry or read it back over the optional OTLP recorder.

use crate::histogram::{
    Counters, HistogramKey, HistogramSeries, Histograms, DEFAULT_SERIES_LIMIT, OVERFLOW_LABEL,
    OVERFLOW_ROUTE, REQUEST_SERIES_LIMIT,
};
use crate::Recorder;
use http::StatusCode;
use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

macro_rules! histogram_series {
    (
        $(#[$meta:meta])*
        $name:ident {
            $($field:ident : $ty:ty),+ $(,)?
        } collapsed: $collapsed:ident with $overflow:expr
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        pub struct $name {
            $(pub $field: $ty,)+
            pub count: u64,
            pub sum: f64,
            pub buckets: Vec<u64>,
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                $(self.$field == other.$field)&&+
            }
        }

        impl Eq for $name {}

        impl Hash for $name {
            fn hash<H: Hasher>(&self, state: &mut H) {
                $(self.$field.hash(state);)+
            }
        }

        impl HistogramSeries for $name {
            fn label_pairs(&self) -> Vec<(&'static str, String)> {
                vec![$((stringify!($field), self.$field.to_string()),)+]
            }

            fn count(&self) -> u64 {
                self.count
            }

            fn sum(&self) -> f64 {
                self.sum
            }

            fn bucket_counts(&self) -> &[u64] {
                &self.buckets
            }
        }

        impl HistogramKey for $name {
            fn collapsed(&self) -> Self {
                Self {
                    $collapsed: $overflow.to_string(),
                    ..self.clone()
                }
            }
        }
    };
}

macro_rules! counter_series {
    (
        $(#[$meta:meta])*
        $name:ident {
            $($field:ident : $ty:ty),+ $(,)?
        } collapsed: $collapsed:ident with $overflow:expr
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        pub struct $name {
            $(pub $field: $ty,)+
            pub count: u64,
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                $(self.$field == other.$field)&&+
            }
        }

        impl Eq for $name {}

        impl Hash for $name {
            fn hash<H: Hasher>(&self, state: &mut H) {
                $(self.$field.hash(state);)+
            }
        }

        impl HistogramKey for $name {
            fn collapsed(&self) -> Self {
                Self {
                    $collapsed: $overflow.to_string(),
                    ..self.clone()
                }
            }
        }
    };
}

histogram_series! {
    /// One request histogram series. `method` and `status` are bounded, `route`
    /// comes from a template or the normalizer, and `org` from the path.
    HttpSeries {
        route: String,
        method: String,
        org: String,
        status: u16,
    } collapsed: route with OVERFLOW_ROUTE
}

histogram_series! {
    /// One MCP tool-call histogram series.
    McpSeries {
        tool: String,
        org: String,
        outcome: String,
    } collapsed: tool with OVERFLOW_LABEL
}

histogram_series! {
    /// One metadata-store operation histogram series.
    StoreSeries {
        db_system: String,
        operation: String,
        outcome: String,
    } collapsed: operation with OVERFLOW_LABEL
}

histogram_series! {
    /// One search-backend request histogram series.
    SearchSeries {
        backend: String,
        outcome: String,
    } collapsed: backend with OVERFLOW_LABEL
}

histogram_series! {
    /// One object-store request histogram series.
    ObjectStoreSeries {
        backend: String,
        operation: String,
        outcome: String,
    } collapsed: operation with OVERFLOW_LABEL
}

counter_series! {
    /// One converge counter series. Converge is a counter, so it has no sum or
    /// buckets.
    ConvergeSeries {
        org: String,
        status: String,
    } collapsed: org with OVERFLOW_LABEL
}

counter_series! {
    /// One request-limit decision series.
    LimitSeries {
        scope: String,
        org: String,
        outcome: String,
    } collapsed: scope with OVERFLOW_LABEL
}

/// Keygen cache gauges, mirroring `microwave_crypto::KeygenStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeygenGauges {
    pub cached: usize,
    pub capacity: usize,
    pub workers: usize,
    pub worker_failures: u64,
    pub warm_up_failures: u64,
    pub generated_on_demand: u64,
    pub worker_healthy: bool,
}

impl Default for KeygenGauges {
    fn default() -> Self {
        Self {
            cached: 0,
            capacity: 0,
            workers: 0,
            worker_failures: 0,
            warm_up_failures: 0,
            generated_on_demand: 0,
            worker_healthy: true,
        }
    }
}

/// Database pool gauges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PoolGauges {
    pub idle: u32,
    pub in_use: u32,
    pub max: u32,
}

/// One dependency probe result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyState {
    pub name: &'static str,
    pub up: bool,
}

/// Atomic counters accumulated by the request middleware.
pub struct Metrics {
    started: Instant,
    total_requests: AtomicU64,
    responses_2xx: AtomicU64,
    responses_4xx: AtomicU64,
    responses_5xx: AtomicU64,
    authentication_failures: AtomicU64,
    recorder: OnceLock<Arc<dyn Recorder>>,
    export_error: Mutex<Option<String>>,
    nodes: AtomicU64,
    maintenance: AtomicBool,
    pool: Mutex<Option<PoolGauges>>,
    keygen: Mutex<KeygenGauges>,
    dependencies: Mutex<Vec<DependencyState>>,
    http: Histograms<HttpSeries>,
    mcp: Histograms<McpSeries>,
    converge: Counters<ConvergeSeries>,
    limits: Counters<LimitSeries>,
    limits_in_flight: Mutex<BTreeMap<&'static str, u64>>,
    store: Histograms<StoreSeries>,
    search: Histograms<SearchSeries>,
    object_store: Histograms<ObjectStoreSeries>,
}

impl fmt::Debug for Metrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Metrics")
            .field("uptime_seconds", &self.uptime_seconds())
            .field(
                "total_requests",
                &self.total_requests.load(Ordering::Relaxed),
            )
            .field("responses_2xx", &self.responses_2xx.load(Ordering::Relaxed))
            .field("responses_4xx", &self.responses_4xx.load(Ordering::Relaxed))
            .field("responses_5xx", &self.responses_5xx.load(Ordering::Relaxed))
            .field(
                "authentication_failures",
                &self.authentication_failures.load(Ordering::Relaxed),
            )
            .finish()
    }
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Bound a converge status to the values the ingest knows plus `other`.
fn converge_status(status: &str) -> &str {
    match status {
        "success" | "failure" | "unknown" => status,
        _ => "other",
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
            recorder: OnceLock::new(),
            export_error: Mutex::new(None),
            nodes: AtomicU64::new(0),
            maintenance: AtomicBool::new(false),
            pool: Mutex::new(None),
            keygen: Mutex::new(KeygenGauges::default()),
            dependencies: Mutex::new(Vec::new()),
            http: Histograms::new(REQUEST_SERIES_LIMIT),
            mcp: Histograms::new(DEFAULT_SERIES_LIMIT),
            converge: Counters::new(DEFAULT_SERIES_LIMIT),
            limits: Counters::new(DEFAULT_SERIES_LIMIT),
            limits_in_flight: Mutex::new(BTreeMap::new()),
            store: Histograms::new(DEFAULT_SERIES_LIMIT),
            search: Histograms::new(DEFAULT_SERIES_LIMIT),
            object_store: Histograms::new(DEFAULT_SERIES_LIMIT),
        }
    }

    /// Install the external sink. Returns `false` when one is already attached.
    pub fn attach(&self, recorder: Arc<dyn Recorder>) -> bool {
        self.recorder.set(recorder).is_ok()
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

    /// Record a finished HTTP request.
    pub fn record_http(
        &self,
        route: &str,
        method: &str,
        org: &str,
        status: u16,
        duration: Duration,
    ) {
        self.http.record(
            HttpSeries {
                route: route.to_string(),
                method: method.to_string(),
                org: org.to_string(),
                status,
                count: 0,
                sum: 0.0,
                buckets: Vec::new(),
            },
            duration,
        );
        if let Some(recorder) = self.recorder.get() {
            recorder.http_request(route, method, org, status, duration);
        }
    }

    /// Record a finished MCP tool call.
    pub fn record_mcp_tool(&self, tool: &str, org: &str, outcome: &str, duration: Duration) {
        self.mcp.record(
            McpSeries {
                tool: tool.to_string(),
                org: org.to_string(),
                outcome: outcome.to_string(),
                count: 0,
                sum: 0.0,
                buckets: Vec::new(),
            },
            duration,
        );
        if let Some(recorder) = self.recorder.get() {
            recorder.mcp_tool(tool, org, outcome, duration);
        }
    }

    /// Count a converge run.
    ///
    /// The payload's `status` is client text, so only the three known values
    /// keep their own series; anything else shares `other`.
    pub fn record_converge(&self, org: &str, status: &str) {
        let status = converge_status(status);
        self.converge.increment(ConvergeSeries {
            org: org.to_string(),
            status: status.to_string(),
            count: 0,
        });
        if let Some(recorder) = self.recorder.get() {
            recorder.converge(org, status);
        }
    }

    /// Count one request-limit decision.
    pub fn record_limit(&self, scope: &str, org: &str, outcome: &str) {
        self.limits.increment(LimitSeries {
            scope: scope.to_string(),
            org: org.to_string(),
            outcome: outcome.to_string(),
            count: 0,
        });
        if let Some(recorder) = self.recorder.get() {
            recorder.limit(scope, org, outcome);
        }
    }

    /// Move one in-flight gauge. The value never drops below zero, even if a
    /// permit is released twice.
    pub fn adjust_limits_in_flight(&self, scope: &'static str, delta: i64) {
        let mut in_flight = lock(&self.limits_in_flight);
        let value = in_flight.entry(scope).or_insert(0);
        *value = value.saturating_add_signed(delta);
    }

    /// Every scope that currently has, or recently had, a held permit.
    pub fn limits_in_flight(&self) -> Vec<(&'static str, u64)> {
        lock(&self.limits_in_flight)
            .iter()
            .map(|(scope, value)| (*scope, *value))
            .collect()
    }

    pub fn limit_series(&self) -> Vec<LimitSeries> {
        let mut series: Vec<_> = self
            .limits
            .snapshot()
            .into_iter()
            .map(|(key, count)| LimitSeries { count, ..key })
            .collect();
        series.sort_unstable_by(|a, b| {
            (&a.scope, &a.org, &a.outcome).cmp(&(&b.scope, &b.org, &b.outcome))
        });
        series
    }

    /// Record a finished metadata-store operation.
    pub fn record_store_operation(
        &self,
        db_system: &str,
        operation: &str,
        outcome: &str,
        duration: Duration,
    ) {
        self.store.record(
            StoreSeries {
                db_system: db_system.to_string(),
                operation: operation.to_string(),
                outcome: outcome.to_string(),
                count: 0,
                sum: 0.0,
                buckets: Vec::new(),
            },
            duration,
        );
        if let Some(recorder) = self.recorder.get() {
            recorder.store_operation(db_system, operation, outcome, duration);
        }
    }

    /// Record a finished search request.
    pub fn record_search_request(&self, backend: &str, outcome: &str, duration: Duration) {
        self.search.record(
            SearchSeries {
                backend: backend.to_string(),
                outcome: outcome.to_string(),
                count: 0,
                sum: 0.0,
                buckets: Vec::new(),
            },
            duration,
        );
        if let Some(recorder) = self.recorder.get() {
            recorder.search_request(backend, outcome, duration);
        }
    }

    /// Record a finished object-store request.
    pub fn record_object_store_request(
        &self,
        backend: &str,
        operation: &str,
        outcome: &str,
        duration: Duration,
    ) {
        self.object_store.record(
            ObjectStoreSeries {
                backend: backend.to_string(),
                operation: operation.to_string(),
                outcome: outcome.to_string(),
                count: 0,
                sum: 0.0,
                buckets: Vec::new(),
            },
            duration,
        );
        if let Some(recorder) = self.recorder.get() {
            recorder.object_store_request(backend, operation, outcome, duration);
        }
    }

    pub fn http_series(&self) -> Vec<HttpSeries> {
        crate::histogram::sorted(
            self.http
                .snapshot()
                .into_iter()
                .map(|(key, buckets)| HttpSeries {
                    count: buckets.count,
                    sum: buckets.sum,
                    buckets: buckets.buckets,
                    ..key
                })
                .collect(),
        )
    }

    pub fn mcp_series(&self) -> Vec<McpSeries> {
        crate::histogram::sorted(
            self.mcp
                .snapshot()
                .into_iter()
                .map(|(key, buckets)| McpSeries {
                    count: buckets.count,
                    sum: buckets.sum,
                    buckets: buckets.buckets,
                    ..key
                })
                .collect(),
        )
    }

    pub fn store_series(&self) -> Vec<StoreSeries> {
        crate::histogram::sorted(
            self.store
                .snapshot()
                .into_iter()
                .map(|(key, buckets)| StoreSeries {
                    count: buckets.count,
                    sum: buckets.sum,
                    buckets: buckets.buckets,
                    ..key
                })
                .collect(),
        )
    }

    pub fn search_series(&self) -> Vec<SearchSeries> {
        crate::histogram::sorted(
            self.search
                .snapshot()
                .into_iter()
                .map(|(key, buckets)| SearchSeries {
                    count: buckets.count,
                    sum: buckets.sum,
                    buckets: buckets.buckets,
                    ..key
                })
                .collect(),
        )
    }

    pub fn object_store_series(&self) -> Vec<ObjectStoreSeries> {
        crate::histogram::sorted(
            self.object_store
                .snapshot()
                .into_iter()
                .map(|(key, buckets)| ObjectStoreSeries {
                    count: buckets.count,
                    sum: buckets.sum,
                    buckets: buckets.buckets,
                    ..key
                })
                .collect(),
        )
    }

    pub fn converge_series(&self) -> Vec<ConvergeSeries> {
        let mut series: Vec<_> = self
            .converge
            .snapshot()
            .into_iter()
            .map(|(key, count)| ConvergeSeries { count, ..key })
            .collect();
        series.sort_unstable_by(|a, b| (&a.org, &a.status).cmp(&(&b.org, &b.status)));
        series
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            uptime_seconds: self.uptime_seconds(),
            total_requests: self.total_requests.load(Ordering::Relaxed),
            responses_2xx: self.responses_2xx.load(Ordering::Relaxed),
            responses_4xx: self.responses_4xx.load(Ordering::Relaxed),
            responses_5xx: self.responses_5xx.load(Ordering::Relaxed),
            authentication_failures: self.authentication_failures.load(Ordering::Relaxed),
        }
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    pub fn set_export_error(&self, message: String) {
        *lock(&self.export_error) = Some(message);
    }

    pub fn export_error(&self) -> Option<String> {
        lock(&self.export_error).clone()
    }

    pub fn clear_export_error(&self) {
        *lock(&self.export_error) = None;
    }

    pub fn set_keygen(&self, gauges: KeygenGauges) {
        *lock(&self.keygen) = gauges;
    }

    pub fn keygen(&self) -> KeygenGauges {
        *lock(&self.keygen)
    }

    pub fn set_dependencies(&self, dependencies: Vec<DependencyState>) {
        *lock(&self.dependencies) = dependencies;
    }

    pub fn dependencies(&self) -> Vec<DependencyState> {
        lock(&self.dependencies).clone()
    }

    pub fn set_maintenance(&self, maintenance: bool) {
        self.maintenance.store(maintenance, Ordering::Relaxed);
    }

    pub fn maintenance(&self) -> bool {
        self.maintenance.load(Ordering::Relaxed)
    }

    pub fn set_nodes(&self, nodes: u64) {
        self.nodes.store(nodes, Ordering::Relaxed);
    }

    pub fn nodes(&self) -> u64 {
        self.nodes.load(Ordering::Relaxed)
    }

    pub fn set_pool(&self, pool: Option<PoolGauges>) {
        *lock(&self.pool) = pool;
    }

    pub fn pool(&self) -> Option<PoolGauges> {
        *lock(&self.pool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LATENCY_BUCKETS_SECONDS;
    use std::sync::Arc;
    use std::time::Duration;

    struct Counting(std::sync::atomic::AtomicUsize);

    impl Recorder for Counting {
        fn http_request(&self, _r: &str, _m: &str, _o: &str, _s: u16, _d: Duration) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn mcp_tool(&self, _t: &str, _o: &str, _oc: &str, _d: Duration) {}
        fn converge(&self, _o: &str, _s: &str) {}
        fn store_operation(&self, _d: &str, _op: &str, _oc: &str, _du: Duration) {}
        fn search_request(&self, _b: &str, _oc: &str, _d: Duration) {}
        fn object_store_request(&self, _b: &str, _op: &str, _oc: &str, _d: Duration) {}
        fn limit(&self, _s: &str, _o: &str, _oc: &str) {}
    }

    #[test]
    fn attach_happens_once_and_fans_out() {
        let metrics = Metrics::new();
        let recorder = Arc::new(Counting(Default::default()));
        assert!(metrics.attach(recorder.clone()));
        assert!(!metrics.attach(Arc::new(Counting(Default::default()))));
        metrics.record_http("/_status", "GET", "_default", 200, Duration::from_millis(3));
        assert_eq!(recorder.0.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn records_every_family_without_a_recorder() {
        let metrics = Metrics::new();
        metrics.record_http("/_status", "GET", "_default", 200, Duration::from_millis(1));
        metrics.record_mcp_tool("list_nodes", "acme", "ok", Duration::from_millis(2));
        metrics.record_converge("acme", "success");
        metrics.record_store_operation("sqlite", "find_node", "ok", Duration::from_millis(1));
        metrics.record_search_request("embedded", "ok", Duration::from_millis(1));
        metrics.record_object_store_request("local", "get", "error", Duration::from_millis(1));
        metrics.record_limit("org", "acme", "rejected");
        assert_eq!(metrics.http_series()[0].count, 1);
        assert_eq!(metrics.mcp_series()[0].tool, "list_nodes");
        assert_eq!(metrics.converge_series()[0].status, "success");
        assert_eq!(metrics.store_series()[0].operation, "find_node");
        assert_eq!(metrics.search_series()[0].backend, "embedded");
        assert_eq!(metrics.object_store_series()[0].outcome, "error");
        assert_eq!(metrics.limit_series()[0].outcome, "rejected");
    }

    #[test]
    fn limits_series_carry_scope_org_and_outcome() {
        let metrics = Metrics::new();
        metrics.record_limit("org", "acme", "rejected");
        metrics.record_limit("global", "", "allowed");
        let series = metrics.limit_series();
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].scope, "global");
        assert_eq!(series[0].outcome, "allowed");
        assert_eq!(series[1].scope, "org");
        assert_eq!(series[1].org, "acme");
        assert_eq!(series[1].outcome, "rejected");
        assert_eq!(series[1].count, 1);
    }

    #[test]
    fn limits_in_flight_saturates_at_zero() {
        let metrics = Metrics::new();
        metrics.adjust_limits_in_flight("org", 2);
        metrics.adjust_limits_in_flight("org", -1);
        assert_eq!(metrics.limits_in_flight(), vec![("org", 1)]);
        metrics.adjust_limits_in_flight("org", -5);
        assert_eq!(metrics.limits_in_flight(), vec![("org", 0)]);
    }

    #[test]
    fn an_unbounded_converge_status_collapses_to_one_series() {
        let metrics = Metrics::new();
        for status in ["success", "failure", "unknown", "aborted"] {
            metrics.record_converge("acme", status);
        }
        metrics.record_converge("acme", "failed with a very long free-form text");
        let series = metrics.converge_series();
        assert_eq!(series.len(), 4);
        let other = series.iter().find(|entry| entry.status == "other").unwrap();
        assert_eq!(other.count, 2);
        assert!(series.iter().any(|entry| entry.status == "success"));
    }

    #[test]
    fn collapses_unknown_keys_at_the_cap() {
        let metrics = Metrics::new();
        for index in 0..=REQUEST_SERIES_LIMIT {
            metrics.record_http(
                &format!("/r{index}"),
                "GET",
                "_default",
                200,
                Duration::from_millis(1),
            );
        }
        let series = metrics.http_series();
        assert!(series.len() <= REQUEST_SERIES_LIMIT + 1);
        assert!(series.iter().any(|entry| entry.route == OVERFLOW_ROUTE));
    }

    #[test]
    fn buckets_are_strictly_increasing() {
        assert!(LATENCY_BUCKETS_SECONDS
            .windows(2)
            .all(|pair| pair[0] < pair[1]));
        assert!(*LATENCY_BUCKETS_SECONDS.last().unwrap() >= 10.0);
    }

    #[test]
    fn buckets_are_cumulative_across_observations() {
        let metrics = Metrics::new();
        metrics.record_http("/_status", "GET", "_default", 200, Duration::from_millis(3));
        metrics.record_http("/_status", "GET", "_default", 200, Duration::from_secs(3));
        let entry = &metrics.http_series()[0];
        assert_eq!(entry.count, 2);
        assert_eq!(entry.buckets.len(), LATENCY_BUCKETS_SECONDS.len() + 1);
        for (index, count) in entry.buckets.iter().enumerate() {
            let expected = [0.003_f64, 3.0]
                .iter()
                .filter(|seconds| {
                    index == LATENCY_BUCKETS_SECONDS.len()
                        || **seconds <= LATENCY_BUCKETS_SECONDS[index]
                })
                .count() as u64;
            assert_eq!(*count, expected, "bucket {index}");
        }
        assert_eq!(*entry.buckets.last().unwrap(), 2);
    }

    #[test]
    fn non_request_families_cap_at_the_default_limit() {
        let metrics = Metrics::new();
        for index in 0..=DEFAULT_SERIES_LIMIT {
            metrics.record_search_request(&format!("b{index}"), "ok", Duration::from_millis(1));
        }
        let series = metrics.search_series();
        assert_eq!(series.len(), DEFAULT_SERIES_LIMIT + 1);
        assert!(series.len() < REQUEST_SERIES_LIMIT);
        assert!(series.iter().any(|entry| entry.backend == OVERFLOW_LABEL));
    }

    #[test]
    fn object_store_overflow_collapses_the_operation() {
        let metrics = Metrics::new();
        for index in 0..=DEFAULT_SERIES_LIMIT {
            metrics.record_object_store_request(
                "local",
                &format!("op{index}"),
                "ok",
                Duration::from_millis(1),
            );
        }
        let series = metrics.object_store_series();
        let overflow = series
            .iter()
            .find(|entry| entry.operation == OVERFLOW_LABEL)
            .expect("overflow series");
        assert_eq!(overflow.backend, "local");
    }
}
