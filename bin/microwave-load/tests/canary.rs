//! The canary: real `chef-client` processes next to the driver.
//!
//! The canary is a fidelity check, so its outcome goes into the report but
//! never into the exit status.

use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NODE: &str = r#"{"name":"load-node-00000","chef_environment":"load-default",
    "json_class":"Chef::Node","chef_type":"node","run_list":[],"normal":{}}"#;

async fn respond(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    match (method.as_str(), path) {
        ("GET", path) if path.starts_with("/organizations/load/nodes/") => {
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

const KEY: &str = include_str!("../../../crates/microwave-crypto/tests/fixtures/test_key.pem");

/// A manifest with one driver client and two canaries, and their key files.
///
/// The canary keys live outside the default `load.keys.d`, so a run that
/// rebuilds the path instead of reading the manifest fails the client-key
/// assertion.
fn manifest(directory: &Path) -> PathBuf {
    manifest_with(directory, true)
}

/// Like [`manifest`], optionally without canary entries.
fn manifest_with(directory: &Path, canaries: bool) -> PathBuf {
    let keys = directory.join("load.keys");
    let key_directory = directory.join("load.keys.d");
    std::fs::create_dir(&key_directory).expect("key directory");
    let key = key_directory.join("shared.pem");
    std::fs::write(&key, KEY).expect("key writes");
    let mut lines = format!("superuser load-superuser {}\n", key.display());
    lines.push_str(&format!("client load-client-00000 {}\n", key.display()));
    if canaries {
        let elsewhere = directory.join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("elsewhere directory");
        for canary in 0..2 {
            let path = elsewhere.join(format!("canary-{canary}.pem"));
            std::fs::write(&path, KEY).expect("canary key writes");
            lines.push_str(&format!("canary canary-{canary} {}\n", path.display()));
        }
    }
    std::fs::write(&keys, lines).expect("manifest writes");
    keys
}

/// A `chef-client` stub on `PATH`: `canary-0` succeeds, `canary-1` fails with
/// a stderr tail, and every call records its arguments.
fn stub(directory: &Path) -> PathBuf {
    let bin = directory.join("bin");
    std::fs::create_dir(&bin).expect("bin directory");
    let script = bin.join("chef-client");
    let record = directory.display();
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
args="$*"
node=
while [ $# -gt 0 ]; do
  if [ "$1" = "--node-name" ]; then node="$2"; fi
  shift
done
printf '%s\n' "$args" > "{record}/$node.argv"
case "$node" in
  canary-0) printf 'canary warning\n' >&2 ;;
  canary-1) printf 'converge failed\n' >&2; exit 1 ;;
esac
exit 0
"#
        ),
    )
    .expect("stub writes");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("stub is executable");
    bin
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

async fn run(args: Vec<String>, path: String) -> Output {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_microwave-load"))
            .args(args)
            .env("PATH", path)
            .output()
            .expect("binary runs")
    })
    .await
    .expect("blocking task")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Every argv file the stub recorded, joined.
fn captured_argv(directory: &Path) -> String {
    std::fs::read_dir(directory)
        .expect("directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension() == Some("argv".as_ref()))
        .map(|path| std::fs::read_to_string(path).expect("argv reads"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn report(out: &Path) -> (String, serde_json::Value) {
    let markdown = std::fs::read_dir(out)
        .expect("out exists")
        .map(|entry| entry.expect("entry").path())
        .find(|path| path.extension() == Some("md".as_ref()))
        .expect("markdown written");
    let json = markdown.with_extension("json");
    (
        std::fs::read_to_string(markdown).expect("markdown reads"),
        serde_json::from_slice(&std::fs::read(json).expect("json reads")).expect("json parses"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canary_outcomes_and_the_failure_tail_reach_the_report() {
    let (url, server) = serve().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let bin = stub(directory.path());
    let keys = manifest(directory.path());
    let out = directory.path().join("out");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut args = args(&url, &keys, &out);
    args.extend(["--canary".to_string(), "2".to_string()]);

    let output = run(args, path).await;
    server.abort();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let canary_lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("canary-"))
        .map(str::to_string)
        .collect();
    let ok: Vec<&String> = canary_lines
        .iter()
        .filter(|line| line.ends_with(": ok"))
        .collect();
    let failed: Vec<&String> = canary_lines
        .iter()
        .filter(|line| line.starts_with("canary-1: failed"))
        .collect();
    assert_eq!(ok.len(), 1, "{canary_lines:?}");
    assert_eq!(failed.len(), 1, "{canary_lines:?}");
    assert_eq!(ok[0], "canary-0: ok");

    // The canary block follows the driver summary.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let summary = stdout.find("converged ").expect("driver summary");
    let canary = stdout.find("canary-0: ok").expect("canary line");
    assert!(summary < canary, "{stdout}");

    let (markdown, json) = report(&out);
    assert!(markdown.contains("- canary-0: ok"), "{markdown}");
    assert!(markdown.contains("  canary warning"), "{markdown}");
    assert!(markdown.contains("canary-1: failed (exit 1"), "{markdown}");
    assert!(markdown.contains("  converge failed"), "{markdown}");
    let lines = json["results"]["canary"]
        .as_array()
        .expect("canary lines in the json");
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().any(|line| line
        .as_str()
        .is_some_and(|line| line.starts_with("canary-0: ok") && line.contains("canary warning"))));
    assert!(lines.iter().any(|line| line
        .as_str()
        .is_some_and(|line| line.starts_with("canary-1: failed (exit 1)")
            && line.contains("converge failed"))));

    let called = std::fs::read_to_string(directory.path().join("canary-0.argv")).expect("argv");
    for expected in [
        "--node-name canary-0",
        "--environment load-default",
        "--chef-license accept",
        &format!("--server {url}/organizations/load"),
        &format!(
            "--client_key {}",
            directory.path().join("elsewhere/canary-0.pem").display()
        ),
    ] {
        assert!(called.contains(expected), "{expected:?} in {called:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_chef_client_skips_the_canary_and_still_exits_zero() {
    let (url, server) = serve().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let keys = manifest(directory.path());
    let out = directory.path().join("out");
    let absent = directory.path().join("no-such-chef-client");
    let mut args = args(&url, &keys, &out);
    args.extend([
        "--canary".to_string(),
        "2".to_string(),
        "--chef-client".to_string(),
        absent.display().to_string(),
    ]);

    let output = run(args, std::env::var("PATH").unwrap_or_default()).await;
    server.abort();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let skipped = "canary skipped: chef-client not found";
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(skipped), "{stdout}");

    let (markdown, json) = report(&out);
    assert!(markdown.contains(skipped), "{markdown}");
    assert_eq!(json["results"]["canary_note"], skipped);
    assert_eq!(json["results"]["canary"].as_array().map(Vec::len), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_canaries_skip_the_processes_entirely() {
    let (url, server) = serve().await;
    let directory = tempfile::tempdir().expect("tempdir");
    // A stub the run must not start.
    let bin = stub(directory.path());
    let keys = manifest(directory.path());
    let out = directory.path().join("out");
    let mut args = args(&url, &keys, &out);
    args.extend(["--canary".to_string(), "0".to_string()]);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = run(args, path).await;
    server.abort();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("canary-0:"), "{stdout}");
    assert!(!stdout.contains("canary skipped"), "{stdout}");
    let called = captured_argv(directory.path());
    assert!(!called.contains("canary-0"), "{called}");

    let (markdown, json) = report(&out);
    assert!(markdown.contains("No canary ran."), "{markdown}");
    assert!(json["results"]["canary_note"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manifest_without_canary_entries_says_so() {
    let (url, server) = serve().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let bin = stub(directory.path());
    let keys = manifest_with(directory.path(), false);
    let out = directory.path().join("out");
    let mut args = args(&url, &keys, &out);
    args.extend(["--canary".to_string(), "2".to_string()]);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = run(args, path).await;
    server.abort();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let note = "canary skipped: the manifest lists no canary entries";
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(note), "{stdout}");

    let (markdown, json) = report(&out);
    assert!(markdown.contains(note), "{markdown}");
    assert_eq!(json["results"]["canary_note"], note);
    assert_eq!(json["results"]["canary"].as_array().map(Vec::len), Some(0));
    assert_eq!(captured_argv(directory.path()), "");
}
