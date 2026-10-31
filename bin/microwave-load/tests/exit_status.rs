//! The exit status of a run: `0` completed, `2` server/fixture, `3`
//! fingerprint/report. A run that finished with failures still exits `0`.

use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NODE: &str = r#"{"name":"load-node-00000","chef_environment":"load-default",
    "json_class":"Chef::Node","chef_type":"node","run_list":[],"normal":{}}"#;

/// Answer the liveness probe, the stats poll and one node's converge.
///
/// The node read sleeps long enough for the one-second stats poller to have
/// sampled the pool, so the report always carries a `pool_size`.
async fn respond(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    match (method.as_str(), path) {
        ("GET", path) if path.starts_with("/organizations/load/nodes/") => {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            (StatusCode::OK, NODE).into_response()
        }
        ("GET", "/organizations/load/environments/load-default") => {
            (StatusCode::OK, "{}").into_response()
        }
        ("POST", path) if path.ends_with("/cookbook_versions") => {
            (StatusCode::OK, "{}").into_response()
        }
        ("PUT", path) if path.starts_with("/organizations/load/nodes/") => {
            (StatusCode::OK, "{}").into_response()
        }
        ("POST", "/organizations/load/data-collector") => (StatusCode::OK, "{}").into_response(),
        _ => (StatusCode::NOT_FOUND, "missing").into_response(),
    }
}

async fn serve() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/_status", get(|| async { "ok" }))
        .route(
            "/_stats",
            get(|| async { r#"{"db_pool":{"idle":9,"in_use":1,"max":10}}"# }),
        )
        .fallback(respond);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener binds");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server runs");
    });
    (url, server)
}

/// A server that answers the probes but rejects every converge request.
async fn serve_failing() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/_status", get(|| async { "ok" }))
        .route(
            "/_stats",
            get(|| async { r#"{"db_pool":{"idle":9,"in_use":1,"max":10}}"# }),
        )
        .fallback(|| async { (StatusCode::FORBIDDEN, "nope") });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener binds");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server runs");
    });
    (url, server)
}

/// A free address nothing listens on any more.
fn closed_address() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds");
    let address = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{address}")
}

const KEY: &str = include_str!("../../../crates/microwave-crypto/tests/fixtures/test_key.pem");

fn manifest(directory: &Path) -> PathBuf {
    let key = directory.join("client.pem");
    std::fs::write(&key, KEY).expect("key writes");
    let manifest = directory.join("load.keys");
    std::fs::write(
        &manifest,
        format!(
            "superuser load-superuser {key}\nclient load-client-00000 {key}\n",
            key = key.display()
        ),
    )
    .expect("manifest writes");
    manifest
}

fn args(url: &str, keys: &Path, out: &Path) -> Vec<String> {
    [
        "run",
        "--server-url",
        url,
        "--keys",
        &keys.display().to_string(),
        "--out",
        &out.display().to_string(),
        "--concurrency",
        "2",
    ]
    .map(str::to_string)
    .to_vec()
}

async fn run(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_microwave-load"))
            .args(args)
            .output()
            .expect("binary runs")
    })
    .await
    .expect("blocking task")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completed_run_exits_zero_and_writes_the_report() {
    let (url, server) = serve().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let keys = manifest(directory.path());
    let out = directory.path().join("out");

    let output = run(args(&url, &keys, &out)).await;
    server.abort();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("report: "), "{stdout}");
    let files: Vec<PathBuf> = std::fs::read_dir(&out)
        .expect("out exists")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(files.len(), 2, "markdown and json: {files:?}");
    let markdown = files
        .iter()
        .find(|path| path.extension() == Some("md".as_ref()))
        .expect("markdown written");
    let json = files
        .iter()
        .find(|path| path.extension() == Some("json".as_ref()))
        .expect("json written");

    let markdown = std::fs::read_to_string(markdown).expect("markdown reads");
    let table = markdown.find("| Endpoint").expect("endpoint table");
    assert!(
        markdown.find("- cores:").expect("cores") < table,
        "the fingerprint precedes the table"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(json).expect("json reads")).expect("json parses");
    assert_eq!(json["fingerprint"]["pool_size"], 10);
    assert_eq!(json["results"]["completed"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_with_failed_nodes_still_exits_zero_and_says_so() {
    let (url, server) = serve_failing().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let keys = manifest(directory.path());
    let out = directory.path().join("out");

    let output = run(args(&url, &keys, &out)).await;
    server.abort();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let markdown = std::fs::read_dir(&out)
        .expect("out exists")
        .map(|entry| entry.expect("entry").path())
        .find(|path| path.extension() == Some("md".as_ref()))
        .expect("markdown written");
    let markdown = std::fs::read_to_string(markdown).expect("markdown reads");
    assert!(markdown.contains("1 nodes did not complete"), "{markdown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_server_exits_two() {
    let directory = tempfile::tempdir().expect("tempdir");
    let keys = manifest(directory.path());
    let out = directory.path().join("out");

    let output = run(args(&closed_address(), &keys, &out)).await;

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("unreachable"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_fixture_exits_two() {
    let (url, server) = serve().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let keys = directory.path().join("absent.keys");
    let out = directory.path().join("out");

    let output = run(args(&url, &keys, &out)).await;
    server.abort();

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("fixture"), "{}", stderr(&output));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_fingerprint_exits_three() {
    let (url, server) = serve().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let keys = manifest(directory.path());
    let out = directory.path().join("out");
    let fingerprint = directory.path().join("host-fingerprint.json");
    std::fs::write(&fingerprint, "{not json").expect("broken file writes");
    let mut args = args(&url, &keys, &out);
    args.extend([
        "--fingerprint".to_string(),
        fingerprint.display().to_string(),
    ]);

    let output = run(args).await;
    server.abort();

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("fingerprint"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unwritable_report_directory_exits_three() {
    let (url, server) = serve().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let keys = manifest(directory.path());
    let out = directory.path().join("not-a-directory");
    std::fs::write(&out, "file").expect("blocking file writes");

    let output = run(args(&url, &keys, &out)).await;
    server.abort();

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("output directory"),
        "{}",
        stderr(&output)
    );
}
