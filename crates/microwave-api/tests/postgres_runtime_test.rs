//! End-to-end runtime test for the PostgreSQL metadata backend.
//!
//! Set `MICROWAVE_TEST_POSTGRES_URL` to run it (for example via
//! `scripts/run-postgres-tests.sh`). Without the variable the test skips, so a
//! developer laptop without Docker stays green. A configured URL that cannot be
//! reached or migrated is a hard failure, never a skip.

mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use microwave_api::AppState;
use microwave_config::{Config, DatabaseDriver};
use microwave_crypto::SignVersion;
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{ActorType, IdentityRepository, MetadataStore, OAuthRepository, SqliteStore};
use microwave_store_postgres::PostgresStore;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

fn test_url() -> Option<String> {
    std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

fn suffix() -> String {
    Uuid::new_v4().simple().to_string()
}

async fn postgres_store() -> Option<PostgresStore> {
    let Some(url) = test_url() else {
        eprintln!(
            "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; start a disposable \
             PostgreSQL with scripts/run-postgres-tests.sh"
        );
        return None;
    };
    Some(
        PostgresStore::connect(&url)
            .await
            .expect("connect to configured PostgreSQL"),
    )
}

/// Returns the router plus the store handle used for direct setup. The
/// temporary object-store directory is owned by the returned guard so it is
/// removed when the test finishes.
async fn pg_state(store: PostgresStore) -> (tempfile::TempDir, Router, PostgresStore, AppState) {
    let store = Arc::new(store);
    // Authentication uses the real middleware, so the superuser needs a key
    // matching the fixture private key.
    let pivotal = match store.find_user("pivotal").await.expect("find superuser") {
        Some(user) => user,
        None => store
            .create_user("pivotal", "pivotal@example.com", "bcrypt", "h", "s", true)
            .await
            .expect("create superuser"),
    };
    let keys = store
        .find_actor_keys(&pivotal.id, ActorType::User)
        .await
        .expect("superuser keys");
    if !keys
        .iter()
        .any(|key| key.public_key.trim() == support::PUBLIC_KEY.trim())
    {
        store
            .add_actor_key(
                &pivotal.id,
                ActorType::User,
                &format!("runtime-{}", suffix()),
                support::PUBLIC_KEY,
            )
            .await
            .expect("add superuser key");
    }

    let mut config = Config::default();
    support::disable_keygen(&mut config);
    config.database.driver = DatabaseDriver::Postgres;
    config.object_store.secret = Some("pg-runtime-secret".into());
    let object_dir = tempfile::tempdir().expect("create object-store temp dir");
    let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
        object_dir.path().join("bookshelf"),
        "pg-runtime-secret",
        "http://0.0.0.0:8000",
    ));
    let state =
        AppState::new(store.clone(), Arc::new(config), object_store).expect("build app state");
    let app = microwave_api::router(state.clone());
    (object_dir, app, (*store).clone(), state)
}

fn signed(method: &str, path: &str, body: &[u8]) -> Request<Body> {
    support::signed_request(method, path, body, SignVersion::V1_3, "pivotal")
}

async fn json(response: axum::response::Response) -> Value {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert!(status.is_success(), "request failed with {status}: {value}");
    value
}

fn form_body(value: &str) -> Body {
    Body::from(value.to_string())
}

#[tokio::test]
async fn core_api_works_against_postgresql() {
    let Some(store) = postgres_store().await else {
        return;
    };
    let (_object_dir, app, store, state) = pg_state(store).await;
    assert!(
        state.sqlite.is_none(),
        "the PostgreSQL runtime must not expose a SQLite escape hatch"
    );
    let org = format!("pg-runtime-{}", suffix());
    let client = "web1";
    let node = "node1";

    // /_status does not require authentication.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/_status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let status = json(response).await;
    assert_eq!(status["status"], "pong");
    assert_eq!(status["indexing"]["mode"], "embedded");

    // Create an organization through the HTTP API.
    let body = format!(r#"{{"name":"{org}","full_name":"PG Runtime"}}"#);
    let response = app
        .clone()
        .oneshot(signed("POST", "/organizations", body.as_bytes()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // Create a client.
    let body = r#"{"name":"web1","validator":false,"create_key":true}"#;
    let response = app
        .clone()
        .oneshot(signed(
            "POST",
            &format!("/organizations/{org}/clients"),
            body.as_bytes(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created_client = json(response).await;
    assert_eq!(
        created_client["uri"],
        format!("http://0.0.0.0:8000/organizations/{org}/clients/{client}")
    );
    assert!(created_client["chef_key"]["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));

    // Create a node and search for it through the embedded PG backend.
    let body = format!(
        r#"{{"name":"{node}","chef_environment":"_default","normal":{{"platform":"debian","tags":["pg"]}},"run_list":[]}}"#
    );
    let response = app
        .clone()
        .oneshot(signed(
            "POST",
            &format!("/organizations/{org}/nodes"),
            body.as_bytes(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(signed(
            "GET",
            &format!("/organizations/{org}/search/node?q=platform:debian"),
            b"",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let hits = json(response).await;
    assert_eq!(hits["total"], 1);
    assert_eq!(hits["rows"][0]["name"], node);

    // OAuth application, authorization-code grant and access token round trip.
    let pivotal = store.find_user("pivotal").await.unwrap().unwrap();
    let application = store
        .create_oauth_application(
            &format!("runtime-{org}"),
            "http://127.0.0.1/callback",
            "read",
            true,
        )
        .await
        .unwrap();
    let grant = store
        .create_oauth_access_grant(
            &pivotal.id,
            application.id,
            "http://127.0.0.1/callback",
            "read",
            600,
        )
        .await
        .unwrap();
    let form = format!(
        "grant_type=authorization_code&code={}&client_id={}&client_secret={}&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcallback",
        grant.token, application.uid, application.secret
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/id/oauth/token")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(form_body(&form))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let token = json(response).await;
    let access_token = token["access_token"].as_str().expect("access token");
    assert_eq!(token["token_type"], "bearer");
    assert!(token["refresh_token"].is_string());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/id/v1/me/organizations")
                .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let organizations = json(response).await;
    assert!(
        organizations
            .as_array()
            .expect("organizations array")
            .iter()
            .any(|entry| entry["name"] == org),
        "created organization missing from the OAuth organizations response"
    );

    // Clean up so reruns against the same database stay independent.
    store
        .delete_oauth_application(application.id)
        .await
        .expect("delete oauth application");
    let response = app
        .clone()
        .oneshot(signed("DELETE", &format!("/organizations/{org}"), b""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Guard the SQLite escape hatch: `AppState::from_sqlite` must keep working
/// while `AppState::new` stays backend-neutral.
#[test]
fn app_state_sqlite_escape_hatch_is_optional() {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let mut config = Config::default();
    support::disable_keygen(&mut config);
    let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
        std::env::temp_dir().join(format!("mw-escape-{}", suffix())),
        "secret",
        "http://0.0.0.0:8000",
    ));
    let state = AppState::from_sqlite(store.clone(), Arc::new(config), object_store).unwrap();
    assert!(state.sqlite.is_some());
    let _: &dyn MetadataStore = state.store.as_ref();
}
