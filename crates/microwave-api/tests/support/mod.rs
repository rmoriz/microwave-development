#![allow(dead_code)]

use axum::body::Body;
use axum::http::Request;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use microwave_api::{router, AppState};
use microwave_config::Config;
use microwave_crypto::{sign_request, CanonicalRequest, SignVersion};
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{ActorType, Org, SqliteStore, User};
use std::sync::Arc;
use uuid::Uuid;

pub const PRIVATE_KEY: &str = include_str!("../../../microwave-crypto/tests/fixtures/test_key.pem");
pub const PUBLIC_KEY: &str =
    include_str!("../../../microwave-crypto/tests/fixtures/test_key_pub.pem");

pub mod mock_opensearch;

pub fn state_with_pivotal() -> AppState {
    state_with_config(Config::default())
}

pub fn state_with_config(mut config: Config) -> AppState {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let user = store
        .create_user("pivotal", "pivotal@example.com", "bcrypt", "h", "s", true)
        .unwrap();
    store
        .add_actor_key(&user.id, ActorType::User, "default", PUBLIC_KEY)
        .unwrap();
    config
        .object_store
        .secret
        .get_or_insert_with(|| "test-secret".into());
    let secret = config.object_store.secret.clone().unwrap();
    let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
        std::env::temp_dir().join(format!("mw-test-{}", Uuid::new_v4())),
        &secret,
        "http://0.0.0.0:8000",
    ));
    AppState::new(store, Arc::new(config), object_store).unwrap()
}

pub fn app(state: AppState) -> axum::Router {
    router(state)
}

/// Add a user with the shared test key and a default profile derived from the
/// username. The `admin` flag is global metadata; organization-level admin
/// behavior is driven by org group membership.
pub fn add_user(state: &AppState, username: &str, email: &str, admin: bool) -> User {
    let user = state
        .store
        .create_user(username, email, "bcrypt", "h", "s", admin)
        .unwrap();
    state
        .store
        .add_actor_key(&user.id, ActorType::User, "default", PUBLIC_KEY)
        .unwrap();
    user
}

pub fn add_org(state: &AppState, name: &str) -> Org {
    let creator = state.store.find_user("pivotal").unwrap().unwrap();
    microwave_authz::create_organization(&state.store, name, name, &creator.id, &creator.authz_id)
        .unwrap()
}

pub fn associate(state: &AppState, org: &Org, user: &User) {
    state.store.add_user_to_org(&org.id, &user.id).unwrap();
}

pub fn signed_request(
    method: &str,
    path: &str,
    body: &[u8],
    version: SignVersion,
    user_id: &str,
) -> Request<Body> {
    signed_request_with_key_and_api_version(method, path, body, version, user_id, PRIVATE_KEY, "2")
}

pub fn signed_request_with_api_version(
    method: &str,
    path: &str,
    body: &[u8],
    version: SignVersion,
    user_id: &str,
    api_version: &str,
) -> Request<Body> {
    signed_request_with_key_and_api_version(
        method,
        path,
        body,
        version,
        user_id,
        PRIVATE_KEY,
        api_version,
    )
}

pub fn signed_request_with_key(
    method: &str,
    path: &str,
    body: &[u8],
    version: SignVersion,
    user_id: &str,
    private_key: &str,
) -> Request<Body> {
    signed_request_with_key_and_api_version(method, path, body, version, user_id, private_key, "2")
}

fn signed_request_with_key_and_api_version(
    method: &str,
    path: &str,
    body: &[u8],
    version: SignVersion,
    user_id: &str,
    private_key: &str,
    api_version: &str,
) -> Request<Body> {
    let canonical_path = path.split('?').next().unwrap_or(path);
    let request = CanonicalRequest {
        method,
        path: canonical_path,
        body,
        user_id,
        timestamp: "2026-10-24T12:00:00Z",
        api_version: Some(api_version),
        version,
    };
    let signed = sign_request(private_key, &request).unwrap();
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
        ("x-ops-server-api-version", api_version.to_string()),
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
