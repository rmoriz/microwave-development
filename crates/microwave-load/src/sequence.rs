//! The per-node converge sequence, in `chef-client` order.
//!
//! `chef-client` reads its node and environment, solves the run list, fetches
//! every file the solution names, saves the node and reports the run. A request
//! that stays non-`2xx` after the retry budget stops the sequence at that
//! endpoint: the remaining requests are skipped, exactly as a converge aborts.
//!
//! Each request is timed into a [`Sample`]; the node run collects them and the
//! driver merges them once the actor is done.

use crate::client::{NodeClient, Outcome};
use crate::metrics::{Metrics, NodeRun, Sample};
use chrono::{SecondsFormat, Utc};
use microwave_objectstore::md5_hex;
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Endpoint labels of the report table.
pub const NODE_READ: &str = "GET nodes/:node";
pub const ENVIRONMENT_READ: &str = "GET environments/:env";
pub const SOLVE: &str = "POST environments/:env/cookbook_versions";
pub const FILE_READ: &str = "GET cookbook files";
pub const NODE_WRITE: &str = "PUT nodes/:node";
pub const DATA_COLLECTOR: &str = "POST data-collector";
pub const SANDBOX_CREATE: &str = "POST sandboxes";
pub const BLOB_WRITE: &str = "PUT blob (presigned)";
pub const SANDBOX_COMMIT: &str = "PUT sandboxes/:sandbox";

/// Build and run the sequence for one node.
///
/// Returns a failed [`NodeRun`] carrying the endpoint and status of the request
/// that stopped it; the samples up to that point are kept.
pub async fn converge(client: &NodeClient, org: &str, node: &str, upload: bool) -> NodeRun {
    let mut run = Converge {
        client,
        samples: Vec::new(),
    };
    let base = format!("/organizations/{org}");
    let node_path = format!("{base}/nodes/{node}");
    let started = Utc::now();

    match run.sequence(&base, node, &node_path, upload, started).await {
        Ok(()) => NodeRun {
            samples: std::mem::take(&mut run.samples),
            completed: true,
            failure: None,
        },
        Err(failure) => NodeRun::failed(std::mem::take(&mut run.samples), failure),
    }
}

/// One actor's sequence, recording a sample per request.
struct Converge<'a> {
    client: &'a NodeClient,
    samples: Vec<Sample>,
}

impl Converge<'_> {
    async fn sequence(
        &mut self,
        base: &str,
        node: &str,
        node_path: &str,
        upload: bool,
        started: chrono::DateTime<Utc>,
    ) -> Result<(), String> {
        let read = self.call(NODE_READ, Method::GET, node_path, None).await?;
        let mut body: Value = serde_json::from_slice(&read.body)
            .map_err(|error| self.parse_failure(NODE_READ, &error.to_string()))?;
        let environment = body
            .get("chef_environment")
            .and_then(Value::as_str)
            .filter(|environment| !environment.is_empty())
            .map(str::to_string)
            .ok_or_else(|| self.parse_failure(NODE_READ, "no chef_environment"))?;
        let run_list = body
            .get("run_list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let environment_path = format!("{base}/environments/{environment}");
        self.call(ENVIRONMENT_READ, Method::GET, &environment_path, None)
            .await?;

        let solve_body = serde_json::to_vec(&json!({ "run_list": run_list }))
            .map_err(|error| self.parse_failure(SOLVE, &error.to_string()))?;
        let solved = self
            .call(
                SOLVE,
                Method::POST,
                &format!("{environment_path}/cookbook_versions"),
                Some(solve_body),
            )
            .await?;
        for url in file_urls(&solved.body).map_err(|error| self.parse_failure(SOLVE, &error))? {
            self.call(FILE_READ, Method::GET, &url, None).await?;
        }

        let iteration =
            bump_iteration(&mut body).map_err(|error| self.parse_failure(NODE_READ, &error))?;
        let write = serde_json::to_vec(&body)
            .map_err(|error| self.parse_failure(NODE_WRITE, &error.to_string()))?;
        self.call(NODE_WRITE, Method::PUT, node_path, Some(write))
            .await?;

        let payload = serde_json::to_vec(&run_converge(node, iteration, started))
            .map_err(|error| self.parse_failure(DATA_COLLECTOR, &error.to_string()))?;
        self.call(
            DATA_COLLECTOR,
            Method::POST,
            &format!("{base}/data-collector"),
            Some(payload),
        )
        .await?;

        if upload {
            self.upload_blob(base, node, iteration).await?;
        }
        Ok(())
    }

    /// Create a sandbox for one fresh blob, upload it through its presigned URL
    /// and commit the sandbox, like a converge that ships a new cookbook file.
    async fn upload_blob(&mut self, base: &str, node: &str, iteration: u64) -> Result<(), String> {
        let content = format!("{node} {iteration}\n").into_bytes();
        let checksum = md5_hex(&content);
        let body = serde_json::to_vec(&json!({ "checksums": { (checksum.clone()): null } }))
            .map_err(|error| self.parse_failure(SANDBOX_CREATE, &error.to_string()))?;
        let created = self
            .call(
                SANDBOX_CREATE,
                Method::POST,
                &format!("{base}/sandboxes"),
                Some(body),
            )
            .await?;
        let sandbox: Value = serde_json::from_slice(&created.body)
            .map_err(|error| self.parse_failure(SANDBOX_CREATE, &error.to_string()))?;
        let url = sandbox
            .pointer(&format!("/checksums/{checksum}/url"))
            .and_then(Value::as_str)
            .ok_or_else(|| self.parse_failure(SANDBOX_CREATE, "no upload URL"))?
            .to_string();
        let sandbox_id = sandbox
            .get("sandbox_id")
            .and_then(Value::as_str)
            .ok_or_else(|| self.parse_failure(SANDBOX_CREATE, "no sandbox id"))?
            .to_string();

        self.call(BLOB_WRITE, Method::PUT, &url, Some(content))
            .await?;
        self.call(
            SANDBOX_COMMIT,
            Method::PUT,
            &format!("{base}/sandboxes/{sandbox_id}"),
            Some(br#"{"is_completed":true}"#.to_vec()),
        )
        .await?;
        Ok(())
    }

    /// Send one request and record its sample.
    ///
    /// Anything but `2xx` is final: it returns the endpoint and status so the
    /// caller skips the rest, like an aborted converge.
    async fn call(
        &mut self,
        endpoint: &'static str,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Outcome, String> {
        let started = Instant::now();
        let outcome = self.client.send(method, path, body).await;
        let latency = started.elapsed();
        match outcome {
            Ok(outcome) => {
                self.samples.push(Sample {
                    endpoint,
                    status: Some(outcome.status.as_u16()),
                    retries: outcome.retries.0,
                    solver_503: outcome.solver_overloaded.0,
                    latency,
                });
                if outcome.status.is_success() {
                    Ok(outcome)
                } else {
                    Err(format!("{endpoint} returned {}", outcome.status))
                }
            }
            Err(error) => {
                let (retries, solver_503) = match &error {
                    crate::LoadError::Transport {
                        retries,
                        solver_503,
                        ..
                    } => (retries.0, solver_503.0),
                    _ => (0, 0),
                };
                self.samples.push(Sample {
                    endpoint,
                    status: None,
                    retries,
                    solver_503,
                    latency,
                });
                Err(format!("{endpoint}: {error}"))
            }
        }
    }

    /// Record a `2xx` whose body could not be used as status class `0`.
    fn parse_failure(&mut self, endpoint: &'static str, detail: &str) -> String {
        self.samples.push(Sample {
            endpoint,
            status: None,
            retries: 0,
            solver_503: 0,
            latency: std::time::Duration::ZERO,
        });
        format!("{endpoint}: unparseable response ({detail})")
    }
}

/// The `all_files[].url` entries of a solve response.
///
/// A file without a URL is skipped: the body still fetched, and the server
/// always annotates. A body that is no JSON object is an error.
fn file_urls(body: &[u8]) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_slice(body).map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "solve body is not an object".to_string())?;
    let mut urls = Vec::new();
    for cookbook in object.values() {
        let Some(files) = cookbook.get("all_files").and_then(Value::as_array) else {
            continue;
        };
        for url in files.iter().filter_map(|file| file.get("url")?.as_str()) {
            urls.push(url.to_string());
        }
    }
    Ok(urls)
}

/// `normal.load_test.iteration += 1`, creating the path when it is missing, and
/// return the new value.
fn bump_iteration(body: &mut Value) -> Result<u64, String> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| "node body is not an object".to_string())?;
    let normal = object.entry("normal").or_insert_with(|| json!({}));
    let normal = normal
        .as_object_mut()
        .ok_or_else(|| "normal is not an object".to_string())?;
    let load_test = normal.entry("load_test").or_insert_with(|| json!({}));
    let load_test = load_test
        .as_object_mut()
        .ok_or_else(|| "normal.load_test is not an object".to_string())?;
    let iteration = load_test
        .get("iteration")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    load_test.insert("iteration".into(), json!(iteration));
    Ok(iteration)
}

/// The minimal `run_converge` payload the data collector accepts.
fn run_converge(node: &str, iteration: u64, started: chrono::DateTime<Utc>) -> Value {
    json!({
        "message_type": "run_converge",
        "node_name": node,
        "run_id": format!("{node}-{iteration}"),
        "status": "success",
        "start_time": started.to_rfc3339_opts(SecondsFormat::Millis, true),
        "end_time": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "total_resource_count": 0,
        "updated_resource_count": 0,
        "resources": [],
    })
}

/// One node to converge, with the client that signs for it.
pub struct Target {
    pub node: String,
    pub client: NodeClient,
}

/// Everything the scheduler needs besides the targets.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub org: String,
    pub upload: bool,
    /// Every request signs as the superuser; the report must say so.
    pub superuser: bool,
    pub concurrency: usize,
    /// Window over which the node starts are spread.
    pub ramp: Duration,
}

/// Converge every target: at most `concurrency` sequences at a time, the starts
/// spread over the ramp window.
pub async fn run_all(config: &RunConfig, targets: Vec<Target>) -> Metrics {
    let mut metrics = Metrics::new(config.superuser);
    let semaphore = Arc::new(Semaphore::new(config.concurrency.max(1)));
    let step = match u32::try_from(targets.len()) {
        Ok(count) if count > 0 => config.ramp / count,
        _ => config.ramp,
    };
    let mut tasks = JoinSet::new();

    for (index, target) in targets.into_iter().enumerate() {
        let delay = step.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX));
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
            break;
        };
        let org = config.org.clone();
        let upload = config.upload;
        tasks.spawn(async move {
            let _permit = permit;
            converge(&target.client, &org, &target.node, upload).await
        });
    }

    while let Some(finished) = tasks.join_next().await {
        match finished {
            Ok(run) => metrics.record(run),
            // A panicked actor would otherwise vanish from the outcome counts.
            Err(_) => metrics.failed += 1,
        }
    }
    metrics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::NodeClient;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, Method as AxumMethod, StatusCode as AxumStatus, Uri};
    use axum::response::{IntoResponse, Response};
    use axum::Router;
    use std::sync::{Arc, Mutex};

    const PRIVATE_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key.pem");

    /// One recorded request.
    #[derive(Clone, Debug)]
    struct Call {
        method: String,
        path: String,
        accept: String,
        body: String,
    }

    #[derive(Clone)]
    struct Server {
        base: String,
        calls: Arc<Mutex<Vec<Call>>>,
        arrivals: Arc<Mutex<Vec<(String, tokio::time::Instant)>>>,
    }

    /// Answer the converge sequence, recording every call.
    async fn respond(
        State(server): State<Server>,
        method: AxumMethod,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let accept = headers
            .get("accept")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        server
            .arrivals
            .lock()
            .expect("arrivals lock")
            .push((uri.path().to_string(), tokio::time::Instant::now()));
        server.calls.lock().expect("calls lock").push(Call {
            method: method.to_string(),
            path: uri.path().to_string(),
            accept,
            body: String::from_utf8_lossy(&body).to_string(),
        });

        match (method.as_str(), uri.path()) {
            ("GET", path) if path.starts_with("/organizations/load/nodes/") => (
                AxumStatus::OK,
                r#"{"name":"load-node-00000","chef_environment":"load-default",
                    "json_class":"Chef::Node","chef_type":"node",
                    "normal":{"load_test":{"iteration":4}},"default":{},"override":{},
                    "automatic":{},"run_list":["role[load-role-0000]","role[load-role-0001]"]}"#,
            )
                .into_response(),
            ("GET", "/organizations/load/environments/load-default") => {
                (AxumStatus::OK, r#"{"name":"load-default"}"#).into_response()
            }
            ("POST", "/organizations/load/environments/load-default/cookbook_versions") => (
                AxumStatus::OK,
                format!(
                    r#"{{"load-cb-0001":{{"all_files":[{{"name":"recipes/default.rb",
                        "checksum":"abc","url":"{}/bookshelf/abc?token=get"}}]}}}}"#,
                    server.base
                ),
            )
                .into_response(),
            ("GET", "/bookshelf/abc") => (AxumStatus::OK, "cookbook file").into_response(),
            ("PUT", path) if path.starts_with("/organizations/load/nodes/") => {
                (AxumStatus::OK, body.clone()).into_response()
            }
            ("POST", "/organizations/load/data-collector") => {
                (AxumStatus::OK, "{}").into_response()
            }
            ("POST", "/organizations/load/sandboxes") => {
                let request: Value = serde_json::from_slice(&body).expect("sandbox body");
                let checksum = request["checksums"]
                    .as_object()
                    .and_then(|checksums| checksums.keys().next())
                    .expect("one checksum");
                let text = format!(
                    r#"{{"sandbox_id":"sandbox-1","checksums":{{"{checksum}":
                        {{"needs_upload":true,"url":"{}/bookshelf/new?token=put"}}}}}}"#,
                    server.base
                );
                (AxumStatus::OK, text).into_response()
            }
            ("PUT", "/bookshelf/new") => (AxumStatus::OK, "").into_response(),
            ("PUT", "/organizations/load/sandboxes/sandbox-1") => {
                (AxumStatus::OK, "{}").into_response()
            }
            _ => (AxumStatus::NOT_FOUND, "missing").into_response(),
        }
    }

    /// Bind a port, give the router its base URL and serve it.
    async fn serve_router(
        server: Server,
        build: impl FnOnce(&Server) -> Router,
    ) -> (Server, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let server = Server { base, ..server };
        let app = build(&server);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server runs");
        });
        (server, handle)
    }

    fn empty_server() -> Server {
        Server {
            base: String::new(),
            calls: Arc::new(Mutex::new(Vec::new())),
            arrivals: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Serve the full converge router.
    async fn server() -> (Server, tokio::task::JoinHandle<()>) {
        serve_router(empty_server(), |server| {
            Router::new().fallback(respond).with_state(server.clone())
        })
        .await
    }

    fn client(server: &Server) -> NodeClient {
        NodeClient::new(
            reqwest::Client::new(),
            server.base.clone(),
            "load-client-00000".to_string(),
            PRIVATE_KEY.to_string(),
        )
    }

    fn calls(server: &Server) -> Vec<Call> {
        server.calls.lock().expect("calls lock").clone()
    }

    #[tokio::test]
    async fn the_sequence_follows_chef_client_order() {
        let (server, handle) = server().await;
        let run = converge(&client(&server), "load", "load-node-00000", true).await;

        assert!(run.completed, "failure: {:?}", run.failure);
        assert_eq!(run.samples.len(), 9, "one sample per request");
        let calls = calls(&server);
        let order: Vec<String> = calls
            .iter()
            .map(|call| format!("{} {}", call.method, call.path))
            .collect();
        assert_eq!(
            order,
            [
                "GET /organizations/load/nodes/load-node-00000",
                "GET /organizations/load/environments/load-default",
                "POST /organizations/load/environments/load-default/cookbook_versions",
                "GET /bookshelf/abc",
                "PUT /organizations/load/nodes/load-node-00000",
                "POST /organizations/load/data-collector",
                "POST /organizations/load/sandboxes",
                "PUT /bookshelf/new",
                "PUT /organizations/load/sandboxes/sandbox-1",
            ]
        );

        let solve = calls
            .iter()
            .find(|call| call.path.ends_with("cookbook_versions"))
            .expect("solve request");
        assert_eq!(
            serde_json::from_str::<Value>(&solve.body).expect("solve body"),
            json!({ "run_list": ["role[load-role-0000]", "role[load-role-0001]"] })
        );

        let write = calls
            .iter()
            .find(|call| call.method == "PUT" && call.path.contains("/nodes/"))
            .expect("node write");
        let body: Value = serde_json::from_str(&write.body).expect("write body");
        assert_eq!(body["normal"]["load_test"]["iteration"], 5);
        assert_eq!(body["run_list"][0], "role[load-role-0000]");

        let blob = calls
            .iter()
            .find(|call| call.path == "/bookshelf/abc")
            .expect("blob read");
        assert_eq!(blob.accept, "application/octet-stream");

        let report = calls
            .iter()
            .find(|call| call.path.ends_with("/data-collector"))
            .expect("report request");
        let payload: Value = serde_json::from_str(&report.body).expect("report body");
        assert_eq!(payload["message_type"], "run_converge");
        assert_eq!(payload["node_name"], "load-node-00000");
        assert_eq!(payload["status"], "success");
        handle.abort();
    }

    #[tokio::test]
    async fn a_403_on_the_node_read_stops_the_sequence() {
        async fn forbidden(State(server): State<Server>) -> Response {
            server.calls.lock().expect("calls lock").push(Call {
                method: "GET".into(),
                path: "/organizations/load/nodes/load-node-00000".into(),
                accept: String::new(),
                body: String::new(),
            });
            (AxumStatus::FORBIDDEN, "nope").into_response()
        }

        let (server, handle) = serve_router(empty_server(), |server| {
            Router::new().fallback(forbidden).with_state(server.clone())
        })
        .await;
        let client = client(&server);

        let run = converge(&client, "load", "load-node-00000", false).await;

        assert!(!run.completed);
        let failure = run.failure.as_deref().expect("failure recorded");
        assert!(failure.contains(NODE_READ), "{failure}");
        assert!(failure.contains("403"), "{failure}");
        assert_eq!(calls(&server).len(), 1, "no request after the failure");
        handle.abort();
    }

    #[tokio::test]
    async fn an_unparseable_node_body_is_reported_as_class_zero() {
        let (server, handle) = serve_router(empty_server(), |server| {
            Router::new()
                .fallback(unparseable)
                .with_state(server.clone())
        })
        .await;
        let client = client(&server);

        let run = converge(&client, "load", "load-node-00000", false).await;

        assert!(!run.completed);
        assert!(run.samples.iter().any(|sample| sample.status.is_none()));
        handle.abort();
    }

    async fn unparseable(State(server): State<Server>) -> Response {
        server.calls.lock().expect("calls lock").push(Call {
            method: "GET".into(),
            path: "node".into(),
            accept: String::new(),
            body: String::new(),
        });
        (AxumStatus::OK, "not json").into_response()
    }

    #[tokio::test]
    async fn the_semaphore_bounds_the_in_flight_sequences() {
        use axum::extract::Request;
        use axum::middleware::{from_fn, Next};
        use std::sync::atomic::{AtomicUsize, Ordering};

        static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
        static PEAK: AtomicUsize = AtomicUsize::new(0);

        async fn track(request: Request, next: Next) -> Response {
            let now = IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
            PEAK.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            let response = next.run(request).await;
            IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
            response
        }

        let (server, handle) = serve_router(empty_server(), |server| {
            Router::new()
                .fallback(respond)
                .with_state(server.clone())
                .layer(from_fn(track))
        })
        .await;
        let client = client(&server);

        let targets: Vec<Target> = (0..6)
            .map(|_| Target {
                node: "load-node-00000".to_string(),
                client: client.clone(),
            })
            .collect();
        let config = RunConfig {
            org: "load".to_string(),
            upload: false,
            superuser: false,
            concurrency: 2,
            ramp: Duration::ZERO,
        };
        let metrics = run_all(&config, targets).await;

        assert_eq!(metrics.completed, 6);
        let peak = PEAK.load(Ordering::SeqCst);
        assert!((1..=2).contains(&peak), "peak in flight was {peak}");
        handle.abort();
    }

    #[tokio::test]
    async fn the_ramp_spreads_the_node_starts() {
        let (server, handle) = server().await;
        let config = RunConfig {
            org: "load".to_string(),
            upload: false,
            superuser: false,
            concurrency: 3,
            ramp: Duration::from_millis(300),
        };
        let targets: Vec<Target> = (0..3)
            .map(|index| Target {
                node: format!("load-node-0000{index}"),
                client: client(&server),
            })
            .collect();

        let metrics = run_all(&config, targets).await;

        assert_eq!(metrics.completed, 3);
        let arrivals = server.arrivals.lock().expect("arrivals lock").clone();
        let mut first: Vec<tokio::time::Instant> = Vec::new();
        for index in 0..3 {
            let marker = format!("/organizations/load/nodes/load-node-0000{index}");
            let arrival = arrivals
                .iter()
                .find(|(path, _)| path == &marker)
                .map(|(_, at)| *at)
                .expect("node read arrived");
            first.push(arrival);
        }
        first.sort();
        for pair in first.windows(2) {
            let gap = pair[1] - pair[0];
            assert!(gap >= Duration::from_millis(60), "gap was {gap:?}");
        }
        handle.abort();
    }
}
