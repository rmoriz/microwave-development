use axum::body::Body;
use axum::http::Request;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use microwave_api::{router, AppState};
use microwave_config::Config;
use microwave_crypto::{sign_request, CanonicalRequest, SignVersion};
use microwave_store::{ActorType, SqliteStore};
use std::sync::Arc;

pub const PRIVATE_KEY: &str = include_str!("../../../microwave-crypto/tests/fixtures/test_key.pem");
pub const PUBLIC_KEY: &str =
    include_str!("../../../microwave-crypto/tests/fixtures/test_key_pub.pem");

pub fn state_with_pivotal() -> AppState {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let user = store
        .create_user("pivotal", "pivotal@example.com", "bcrypt", "h", "s", true)
        .unwrap();
    store
        .add_actor_key(&user.id, ActorType::User, "default", PUBLIC_KEY)
        .unwrap();
    AppState {
        store,
        config: Arc::new(Config::default()),
    }
}

pub fn app(state: AppState) -> axum::Router {
    router(state)
}

pub fn signed_request(
    method: &str,
    path: &str,
    body: &[u8],
    version: SignVersion,
    user_id: &str,
) -> Request<Body> {
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
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    for (name, value) in [
        ("x-ops-sign", sign.to_string()),
        ("x-ops-userid", user_id.to_string()),
        ("x-ops-timestamp", signed.timestamp),
        ("x-ops-content-hash", signed.content_hash),
        ("x-ops-server-api-version", "2".to_string()),
    ] {
        builder = builder.header(name, value);
    }
    for (index, chunk) in encoded.as_bytes().chunks(60).enumerate() {
        builder = builder.header(
            format!("x-ops-authorization-{}", index + 1),
            String::from_utf8(chunk.to_vec()).unwrap(),
        );
    }
    builder.body(Body::from(body.to_vec())).unwrap()
}

pub async fn response_json(response: axum::response::Response) -> serde_json::Value {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}
