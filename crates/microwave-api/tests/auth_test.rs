mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use microwave_api::{router, AppState};
use microwave_config::Config;
use microwave_crypto::{sign_request, CanonicalRequest, SignVersion};
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{ActorType, SqliteStore};
use std::sync::Arc;
use tower::ServiceExt;

const PRIVATE_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key.pem");
const PUBLIC_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key_pub.pem");

fn state_with_pivotal() -> AppState {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let user = store
        .create_user("pivotal", "pivotal@example.com", "bcrypt", "h", "s", true)
        .unwrap();
    store
        .add_actor_key(&user.id, ActorType::User, "default", PUBLIC_KEY)
        .unwrap();
    let mut config = Config::default();
    support::disable_keygen(&mut config);
    config.object_store.secret = Some("test".into());
    let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
        std::env::temp_dir().join("mw-auth-test"),
        "test",
        "http://0.0.0.0:8000",
    ));
    AppState::from_sqlite(store, Arc::new(config), object_store).unwrap()
}

fn signed_headers(
    method: &str,
    path: &str,
    body: &[u8],
    version: SignVersion,
    user_id: &str,
) -> Vec<(String, String)> {
    let request = CanonicalRequest {
        method,
        path,
        body,
        user_id,
        timestamp: "2026-10-24T12:00:00Z",
        api_version: Some("2"),
        version,
    };
    let signed = sign_request(PRIVATE_KEY, &request).unwrap();
    let sign = match version {
        SignVersion::V1_0 => "algorithm=sha1;version=1.0;",
        SignVersion::V1_1 => "algorithm=sha1;version=1.1;",
        SignVersion::V1_3 => "algorithm=sha256;version=1.3;",
    };
    let encoded = STANDARD.encode(signed.signature);
    let mut headers = vec![
        ("x-ops-sign".into(), sign.into()),
        ("x-ops-userid".into(), user_id.into()),
        ("x-ops-timestamp".into(), signed.timestamp),
        ("x-ops-content-hash".into(), signed.content_hash),
        ("x-ops-server-api-version".into(), "2".into()),
    ];
    for (index, chunk) in encoded.as_bytes().chunks(60).enumerate() {
        headers.push((
            format!("x-ops-authorization-{}", index + 1),
            String::from_utf8(chunk.to_vec()).unwrap(),
        ));
    }
    headers
}

fn request(
    method: &str,
    path: &str,
    body: &'static [u8],
    version: SignVersion,
    user_id: &str,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in signed_headers(method, path, body, version, user_id) {
        builder = builder.header(name, value);
    }
    builder.body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn status_and_version_are_public() {
    let app = router(state_with_pivotal());
    for path in ["/_status", "/server_api_version"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn license_accepts_a_signed_request() {
    let app = router(state_with_pivotal());
    let response = app
        .oneshot(request(
            "GET",
            "/license",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn rejects_out_of_range_api_version() {
    let app = router(state_with_pivotal());
    let response = app
        .oneshot(
            Request::get("/server_api_version")
                .header("x-ops-server-api-version", "9")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn rejects_tampered_body() {
    let app = router(state_with_pivotal());
    let mut request = request(
        "POST",
        "/users",
        br#"{"username":"alice"}"#,
        SignVersion::V1_3,
        "pivotal",
    );
    *request.body_mut() = Body::from(br#"{"username":"mallory"}"#.to_vec());

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_unknown_actor() {
    let app = router(state_with_pivotal());
    let response = app
        .oneshot(request("GET", "/license", b"", SignVersion::V1_3, "nobody"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
