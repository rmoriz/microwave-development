//! Shared axum fixture for the live-crawler tests.
//!
//! The server records every request and verifies its Chef signature, then
//! answers the routes a test registers; everything else is a 404, which the
//! crawlers treat as absent. Later crawl tests reuse the helpers, so not every
//! item is used by every test file.
#![allow(dead_code)]

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use microwave_crypto::{
    decode_signature, parse_sign_header, verify_request, AuthHeaders, CanonicalRequest,
};
use microwave_migrate::GoiardiOptions;
use microwave_objectstore::md5_hex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

const PUBLIC_KEY: &str = include_str!("../../../microwave-crypto/tests/fixtures/test_key_pub.pem");
const PRIVATE_KEY: &str = include_str!("../../../microwave-crypto/tests/fixtures/test_key.pem");

#[derive(Clone)]
pub struct AppState {
    routes: Arc<HashMap<String, Route>>,
    records: Arc<Mutex<Vec<Recorded>>>,
}

#[derive(Clone)]
pub struct Route {
    status: u16,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub verified: bool,
    pub verification_error: Option<String>,
}

pub fn json_route(value: Value) -> Route {
    Route {
        status: 200,
        body: serde_json::to_string(&value).unwrap().into_bytes(),
    }
}

/// Serve a raw JSON body.
///
/// Every `"<PUBLIC_KEY>"` placeholder is replaced with the test public key so
/// fixtures stay readable while the crawler still sees a valid PEM.
pub fn raw_route(body: &'static str) -> Route {
    let body = body.replace("<PUBLIC_KEY>", &escaped_public_key());
    Route {
        status: 200,
        body: body.into_bytes(),
    }
}

pub fn text_route(status: u16, body: impl Into<String>) -> Route {
    Route {
        status,
        body: body.into().into_bytes(),
    }
}

pub fn bytes_route(status: u16, body: impl Into<Vec<u8>>) -> Route {
    Route {
        status,
        body: body.into(),
    }
}

/// Build a deterministic cookbook blob, its MD5 and its leaked
/// `/file_store/<checksum>` path.
pub fn blob_fixture(seed: &str) -> (Vec<u8>, String, &'static str) {
    let body = format!("cookbook blob {seed}\n").into_bytes();
    let checksum = md5_hex(&body);
    let path = Box::leak(format!("/file_store/{checksum}").into_boxed_str());
    (body, checksum, path)
}

pub fn start_server(routes: Vec<(&str, Route)>) -> (String, Arc<Mutex<Vec<Recorded>>>) {
    let routes: HashMap<String, Route> = routes
        .into_iter()
        .map(|(path, route)| (path.to_string(), route))
        .collect();
    let records = Arc::new(Mutex::new(Vec::new()));
    let state = AppState {
        routes: Arc::new(routes),
        records: records.clone(),
    };
    let app = Router::new().fallback(handle).with_state(state);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    (format!("http://{address}"), records)
}

async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();

    let (verified, verification_error) = verify_signature(&method, &path, &headers, &body);
    let mut header_map = HashMap::new();
    for (name, value) in headers.iter() {
        header_map.insert(
            name.as_str().to_ascii_lowercase(),
            value.to_str().unwrap_or_default().to_string(),
        );
    }
    state.records.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        headers: header_map,
        verified,
        verification_error,
    });

    match state.routes.get(&path) {
        Some(route) => {
            let status = StatusCode::from_u16(route.status).unwrap();
            (
                status,
                [(header::CONTENT_TYPE, "application/json")],
                route.body.clone(),
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"not found"}"#.to_string(),
        )
            .into_response(),
    }
}

fn verify_signature(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> (bool, Option<String>) {
    match verify_signature_inner(method, path, headers, body) {
        Ok(()) => (true, None),
        Err(error) => (false, Some(error)),
    }
}

fn verify_signature_inner(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), String> {
    let sign = header(headers, "x-ops-sign")?;
    let version = parse_sign_header(&sign).map_err(|error| error.to_string())?;
    let user_id = header(headers, "x-ops-userid")?;
    let timestamp = header(headers, "x-ops-timestamp")?;
    let content_hash = header(headers, "x-ops-content-hash")?;
    let api_version = header(headers, "x-ops-server-api-version")?;

    let mut lines = Vec::new();
    for index in 1..=64 {
        match headers.get(format!("x-ops-authorization-{index}")) {
            Some(value) => lines.push(value.to_str().map_err(|error| error.to_string())?),
            None => break,
        }
    }
    let signature = decode_signature(&lines).map_err(|error| error.to_string())?;

    let auth = AuthHeaders {
        version,
        user_id: user_id.clone(),
        timestamp: timestamp.clone(),
        content_hash,
        signature,
    };
    let request = CanonicalRequest {
        method: method.as_str(),
        path,
        body,
        user_id: &user_id,
        timestamp: &timestamp,
        api_version: Some(&api_version),
        version,
    };
    verify_request(PUBLIC_KEY, &auth, &request).map_err(|error| error.to_string())
}

fn header(headers: &HeaderMap, name: &str) -> Result<String, String> {
    headers
        .get(name)
        .ok_or_else(|| format!("missing {name} header"))?
        .to_str()
        .map(str::to_string)
        .map_err(|error| error.to_string())
}

fn escaped_public_key() -> String {
    PUBLIC_KEY
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn write_key(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("migrator.pem");
    std::fs::write(&path, PRIVATE_KEY).unwrap();
    path
}

/// Minimal goiardi options: `acme` organization into `dir/snapshot.json`.
///
/// Tests override only the fields they exercise, for example `dry_run`.
pub fn options(url: &str, dir: &Path) -> GoiardiOptions {
    GoiardiOptions::new(
        url,
        "migrator",
        write_key(dir),
        dir.join("snapshot.json"),
        "acme",
    )
}
