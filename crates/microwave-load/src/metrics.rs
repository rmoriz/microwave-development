//! Per-endpoint counters, latency percentiles and the `/_stats` pool poller.
//!
//! One [`Sample`] stands for one request. The sequence of a node collects its
//! samples locally and hands the whole [`NodeRun`] back, so 5,000 concurrent
//! actors never contend on a shared lock while they are being measured; the
//! driver merges the runs into [`Metrics`] as they finish.
//!
//! A request that produced no status (transport exhaustion, an unparseable
//! body) is counted under the status class `0`, so it shows up in the table
//! instead of vanishing into a separate error counter.

use reqwest::StatusCode;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One measured request.
#[derive(Debug, Clone)]
pub struct Sample {
    pub endpoint: &'static str,
    /// `None` is the status class `0`: no status was produced.
    pub status: Option<u16>,
    pub retries: u32,
    pub solver_503: u32,
    pub latency: Duration,
}

/// Everything one node's sequence measured.
#[derive(Debug, Default)]
pub struct NodeRun {
    pub samples: Vec<Sample>,
    /// True when the sequence reached the data collector without a failure.
    pub completed: bool,
    /// Endpoint and status of the request that stopped the sequence.
    pub failure: Option<String>,
}

impl NodeRun {
    /// A run that failed at `failure`; the samples are already recorded.
    pub fn failed(samples: Vec<Sample>, failure: String) -> Self {
        Self {
            samples,
            completed: false,
            failure: Some(failure),
        }
    }
}

/// Counters and raw latencies of one endpoint.
#[derive(Debug, Default)]
pub struct Endpoint {
    pub count: u64,
    pub statuses: BTreeMap<u16, u64>,
    pub retries: u64,
    pub solver_503: u64,
    pub latencies_us: Vec<u32>,
}

impl Endpoint {
    /// Fold one sample in.
    pub fn record(&mut self, sample: &Sample) {
        self.count += 1;
        let class = sample.status.unwrap_or(0);
        *self.statuses.entry(class).or_default() += 1;
        self.retries += u64::from(sample.retries);
        self.solver_503 += u64::from(sample.solver_503);
        let micros = u32::try_from(sample.latency.as_micros()).unwrap_or(u32::MAX);
        self.latencies_us.push(micros);
    }

    /// The endpoint with its latencies reduced to percentiles and the maximum.
    pub fn summary(&self) -> Summary {
        let mut sorted = self.latencies_us.clone();
        sorted.sort_unstable();
        Summary {
            count: self.count,
            statuses: self.statuses.clone(),
            retries: self.retries,
            solver_503: self.solver_503,
            p50: percentile(&sorted, 50),
            p95: percentile(&sorted, 95),
            p99: percentile(&sorted, 99),
            max: sorted.last().copied().unwrap_or(0),
        }
    }
}

/// One endpoint of the report: counters plus the percentile band.
#[derive(Debug, Clone)]
pub struct Summary {
    pub count: u64,
    pub statuses: BTreeMap<u16, u64>,
    pub retries: u64,
    pub solver_503: u64,
    pub p50: u32,
    pub p95: u32,
    pub p99: u32,
    pub max: u32,
}

/// Nearest-rank percentile over `sorted` micros.
///
/// Zero for an empty slice; the rank is `ceil(percent * len / 100)`, clamped to
/// the slice, so p99 of five values is the maximum rather than an
/// interpolation between two of them.
pub fn percentile(sorted: &[u32], percent: u8) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let len = sorted.len();
    let rank = (usize::from(percent) * len).div_ceil(100).clamp(1, len);
    sorted[rank - 1]
}

/// A whole run: one [`Endpoint`] per request kind plus the node outcome counts.
#[derive(Debug)]
pub struct Metrics {
    pub endpoints: BTreeMap<&'static str, Endpoint>,
    pub completed: u64,
    pub failed: u64,
    pub pool_samples: Vec<Value>,
    /// Whether every driver request signed as the superuser.
    pub superuser: bool,
}

impl Metrics {
    pub fn new(superuser: bool) -> Self {
        Self {
            endpoints: BTreeMap::new(),
            completed: 0,
            failed: 0,
            pool_samples: Vec::new(),
            superuser,
        }
    }

    /// Merge one finished node run.
    pub fn record(&mut self, run: NodeRun) {
        for sample in &run.samples {
            self.endpoints
                .entry(sample.endpoint)
                .or_default()
                .record(sample);
        }
        if run.completed {
            self.completed += 1;
        } else {
            self.failed += 1;
        }
    }

    /// Every endpoint with its percentile band, keyed like [`Self::endpoints`].
    pub fn summaries(&self) -> BTreeMap<&'static str, Summary> {
        self.endpoints
            .iter()
            .map(|(endpoint, counters)| (*endpoint, counters.summary()))
            .collect()
    }
}

/// Background `/_stats` poller for the pool occupancy range.
pub struct StatsPoller {
    samples: Arc<Mutex<Vec<Value>>>,
    running: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl StatsPoller {
    /// Poll `stats_url` once a second until [`Self::finish`].
    ///
    /// A `401` (or any other non-`200`) is skipped: the credentials are a
    /// convenience, and losing the pool range must not fail the run.
    pub fn start(
        http: reqwest::Client,
        stats_url: String,
        credentials: Option<(String, String)>,
    ) -> Self {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(true));
        let task = tokio::spawn({
            let samples = Arc::clone(&samples);
            let running = Arc::clone(&running);
            async move {
                let mut tick = tokio::time::interval(Duration::from_secs(1));
                while running.load(Ordering::Relaxed) {
                    if let Some(pool) = sample_pool(&http, &stats_url, credentials.as_ref()).await {
                        samples.lock().expect("samples lock").push(pool);
                    }
                    tick.tick().await;
                }
            }
        });
        Self {
            samples,
            running,
            task,
        }
    }

    /// Stop polling and return the recorded `db_pool` documents.
    pub fn finish(self) -> Vec<Value> {
        self.running.store(false, Ordering::Relaxed);
        self.task.abort();
        self.samples.lock().expect("samples lock").clone()
    }
}

impl Drop for StatsPoller {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One `/_stats` poll; returns the `db_pool` object when the server has a pool.
pub async fn sample_pool(
    http: &reqwest::Client,
    stats_url: &str,
    credentials: Option<&(String, String)>,
) -> Option<Value> {
    let mut request = http.get(stats_url).header("Accept", "application/json");
    if let Some((user, pass)) = credentials {
        request = request.basic_auth(user, Some(pass));
    }
    let response = request.send().await.ok()?;
    if response.status() != StatusCode::OK {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    body.get("db_pool").cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Response};
    use axum::Router;
    use std::sync::atomic::AtomicUsize;

    fn sample(status: Option<u16>, retries: u32, solver: u32, micros: u64) -> Sample {
        Sample {
            endpoint: "GET nodes/:node",
            status,
            retries,
            solver_503: solver,
            latency: Duration::from_micros(micros),
        }
    }

    #[test]
    fn percentiles_are_nearest_rank_over_sorted_micros() {
        let sorted = [1, 2, 3, 4, 5];
        assert_eq!(percentile(&sorted, 50), 3);
        assert_eq!(percentile(&sorted, 95), 5);
        assert_eq!(percentile(&sorted, 99), 5);
        assert_eq!(percentile(&sorted, 0), 1);
        assert_eq!(percentile(&[], 95), 0);

        let mut endpoint = Endpoint::default();
        for micros in [5, 1, 3, 2, 4] {
            endpoint.record(&sample(Some(200), 0, 0, micros));
        }
        let summary = endpoint.summary();
        assert_eq!(
            (summary.p50, summary.p95, summary.p99, summary.max),
            (3, 5, 5, 5)
        );
        assert_eq!(summary.count, 5);
        assert_eq!(summary.statuses.get(&200), Some(&5));
    }

    #[test]
    fn a_status_less_request_is_counted_as_class_zero() {
        let mut metrics = Metrics::new(false);
        metrics.record(NodeRun::failed(
            vec![sample(None, 5, 1, 1_000)],
            "GET nodes/:node: transport".into(),
        ));

        let endpoint = &metrics.endpoints["GET nodes/:node"];
        assert_eq!(endpoint.statuses.get(&0), Some(&1));
        assert_eq!(endpoint.retries, 5);
        assert_eq!(endpoint.solver_503, 1);
        assert_eq!(metrics.failed, 1);
        assert_eq!(metrics.completed, 0);
    }

    #[test]
    fn a_solver_503_is_counted_without_failing_the_node() {
        let mut metrics = Metrics::new(true);
        metrics.record(NodeRun {
            samples: vec![sample(Some(200), 2, 2, 500)],
            completed: true,
            failure: None,
        });

        let endpoint = &metrics.endpoints["GET nodes/:node"];
        assert_eq!(endpoint.solver_503, 2);
        assert_eq!(endpoint.statuses.get(&200), Some(&1));
        assert_eq!(metrics.failed, 0);
        assert_eq!(metrics.completed, 1);
        assert!(metrics.superuser);
    }

    /// Serve `first` once, then `second` forever; count the calls.
    async fn scripted(
        first: StatusCode,
        second: (StatusCode, &'static str),
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        #[derive(Clone)]
        struct Script {
            first: StatusCode,
            second: (StatusCode, &'static str),
            calls: Arc<AtomicUsize>,
        }

        async fn respond(State(script): State<Script>, headers: HeaderMap) -> Response {
            let call = script.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                return (script.first, "").into_response();
            }
            if !headers.contains_key("authorization") {
                return (reqwest::StatusCode::BAD_REQUEST, "no basic auth").into_response();
            }
            (script.second.0, script.second.1).into_response()
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new().fallback(respond).with_state(Script {
            first,
            second,
            calls: Arc::clone(&calls),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server runs");
        });
        (url, calls, server)
    }

    #[tokio::test]
    async fn the_pool_poller_tolerates_a_401_and_keeps_the_sample() {
        let (url, calls, server) = scripted(
            StatusCode::UNAUTHORIZED,
            (StatusCode::OK, r#"{"db_pool":{"size":3,"max":10}}"#),
        )
        .await;
        let http = reqwest::Client::new();
        let credentials = ("stats".to_string(), "secret".to_string());

        assert!(sample_pool(&http, &url, Some(&credentials)).await.is_none());
        let pool = sample_pool(&http, &url, Some(&credentials))
            .await
            .expect("second poll answers");
        assert_eq!(pool["max"], 10);
        assert_eq!(pool["size"], 3);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
