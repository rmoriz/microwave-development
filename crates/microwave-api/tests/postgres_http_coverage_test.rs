//! HTTP-level coverage of the PostgreSQL metadata backend.
//!
//! These tests drive the real router, real signed requests, the real object
//! store and the real PostgreSQL-backed `MetadataStore`. Set
//! `MICROWAVE_TEST_POSTGRES_URL` to run them (the helper script does this).
//! Without the variable every test skips; a configured URL that cannot be
//! reached or migrated fails loudly.

mod support;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use microwave_api::AppState;
use microwave_config::{Config, DatabaseDriver};
use microwave_crypto::SignVersion;
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{
    ActorType, AuthzRepository, IdentityRepository, MembershipRepository, SandboxRepository,
};
use microwave_store_postgres::PostgresStore;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

const HELLO_CHECKSUM: &str = "5d41402abc4b2a76b9719d911017c592";
const WORLD_CHECKSUM: &str = "7d793037a0760186574b0282f2f435e7";
const MISSING_CHECKSUM: &str = "00000000000000000000000000000000";

fn test_url() -> Option<String> {
    std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

fn suffix() -> String {
    Uuid::new_v4().simple().to_string()
}

fn unique_name(prefix: &str) -> String {
    format!("{prefix}{}", &Uuid::new_v4().simple().to_string()[..12])
}

struct PgApp {
    app: Router,
    store: PostgresStore,
    _object_dir: tempfile::TempDir,
}

async fn pg_app() -> Option<PgApp> {
    let Some(url) = test_url() else {
        eprintln!(
            "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; start a disposable \
             PostgreSQL with scripts/run-postgres-tests.sh"
        );
        return None;
    };
    let store = PostgresStore::connect(&url)
        .await
        .expect("connect to configured PostgreSQL");
    Some(pg_app_with_store(store).await)
}

async fn pg_app_with_store(store: PostgresStore) -> PgApp {
    let store = Arc::new(store);
    // Authentication uses the real middleware, so the superuser needs a key
    // matching the fixture private key. Several tests run concurrently, so a
    // create race is resolved by re-reading the row.
    let pivotal = match store.find_user("pivotal").await.expect("find superuser") {
        Some(user) => user,
        None => match store
            .create_user("pivotal", "pivotal@example.com", "bcrypt", "h", "s", true)
            .await
        {
            Ok(user) => user,
            Err(_) => store
                .find_user("pivotal")
                .await
                .expect("find superuser after race")
                .expect("superuser created concurrently"),
        },
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
    config.database.driver = DatabaseDriver::Postgres;
    config.object_store.secret = Some("pg-http-secret".into());
    let object_dir = tempfile::tempdir().expect("create object-store temp dir");
    let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
        object_dir.path().join("bookshelf"),
        "pg-http-secret",
        "http://0.0.0.0:8000",
    ));
    let state =
        AppState::new(store.clone(), Arc::new(config), object_store).expect("build app state");
    let app = microwave_api::router(state);
    let store = (*store).clone();
    PgApp {
        app,
        store,
        _object_dir: object_dir,
    }
}

async fn send(
    pg: &PgApp,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, Value) {
    send_with_headers(pg, method, path, body, user)
        .await
        .map(|(status, _, value)| (status, value))
        .unwrap()
}

async fn send_with_headers(
    pg: &PgApp,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> Result<(StatusCode, HeaderMap, Value), String> {
    let response = pg
        .app
        .clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
        ))
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid JSON from {method} {path}: {error}"))?
    };
    Ok((status, headers, value))
}

async fn send_api(
    pg: &PgApp,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
    api_version: &str,
) -> (StatusCode, Value) {
    let response = pg
        .app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
            api_version,
        ))
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn ok(pg: &PgApp, method: &str, path: &str, body: &[u8], user: &str) -> Value {
    let (status, value) = send(pg, method, path, body, user).await;
    assert!(
        status.is_success(),
        "{method} {path} failed with {status}: {value}"
    );
    value
}

async fn create_org(pg: &PgApp, org: &str) {
    let body = format!(r#"{{"name":"{org}","full_name":"PG HTTP Coverage"}}"#);
    let (status, value) = send(pg, "POST", "/organizations", body.as_bytes(), "pivotal").await;
    assert_eq!(status, StatusCode::CREATED, "create org {org}: {value}");
}

async fn create_user(pg: &PgApp, username: &str) {
    let body = serde_json::to_vec(&json!({
        "username": username,
        "email": format!("{username}@example.com"),
        "display_name": username,
        "password": "secret",
        "public_key": support::PUBLIC_KEY,
    }))
    .unwrap();
    let (status, value) = send(pg, "POST", "/users", &body, "pivotal").await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create user {username}: {value}"
    );
}

async fn associate_user(pg: &PgApp, org: &str, user: &str) {
    let body = format!(r#"{{"username":"{user}"}}"#);
    let (status, value) = send(
        pg,
        "POST",
        &format!("/organizations/{org}/users"),
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "associate {user} with {org}: {value}"
    );
}

/// Upload `contents` through a sandbox and commit it, returning the checksum.
async fn upload_blob(pg: &PgApp, org: &str, checksum: &str, contents: &str) {
    let body = serde_json::to_vec(&json!({
        "checksums": { checksum: Value::Null }
    }))
    .unwrap();
    let created = ok(
        pg,
        "POST",
        &format!("/organizations/{org}/sandboxes"),
        &body,
        "pivotal",
    )
    .await;
    let url = created["checksums"][checksum]["url"]
        .as_str()
        .unwrap_or_else(|| panic!("missing upload URL for {checksum}: {created}"));
    let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
    let response = pg
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .body(Body::from(contents.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "upload blob {checksum} failed"
    );

    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let committed = ok(
        pg,
        "PUT",
        &format!("/organizations/{org}/sandboxes/{sandbox_id}"),
        br#"{"is_completed":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(committed["is_completed"], true);
}

async fn put_manifest(
    pg: &PgApp,
    org: &str,
    name: &str,
    version: &str,
    api_version: &str,
    manifest: &Value,
) -> (StatusCode, Value) {
    send_api(
        pg,
        "PUT",
        &format!("/organizations/{org}/cookbooks/{name}/{version}"),
        &serde_json::to_vec(manifest).unwrap(),
        "pivotal",
        api_version,
    )
    .await
}

async fn get_manifest(
    pg: &PgApp,
    org: &str,
    name: &str,
    version: &str,
    api_version: &str,
) -> Value {
    let (status, value) = send_api(
        pg,
        "GET",
        &format!("/organizations/{org}/cookbooks/{name}/{version}"),
        b"",
        "pivotal",
        api_version,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "get {name} {version}: {value}");
    value
}

#[tokio::test]
async fn cookbook_upload_versions_and_depsolver_work_against_postgresql() {
    let Some(pg) = pg_app().await else {
        return;
    };
    let org = unique_name("pgcook");

    create_org(&pg, &org).await;
    upload_blob(&pg, &org, HELLO_CHECKSUM, "hello").await;

    // Upload through the v2 route so the manifest stores the full file list.
    let apache = json!({
        "name": "apache-1.0.0",
        "cookbook_name": "apache",
        "version": "1.0.0",
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "metadata": {
            "name": "apache",
            "version": "1.0.0",
            "dependencies": { "dep": "~> 1.0" },
        },
        "all_files": [{
            "name": "recipes/default.rb",
            "path": "recipes/default.rb",
            "checksum": HELLO_CHECKSUM,
            "specificity": "default",
        }],
    });
    let (status, body) = put_manifest(&pg, &org, "apache", "1.0.0", "2", &apache).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    for version in ["1.0.0", "2.0.0"] {
        let manifest = json!({
            "name": format!("dep-{version}"),
            "cookbook_name": "dep",
            "version": version,
            "json_class": "Chef::CookbookVersion",
            "chef_type": "cookbook_version",
            "frozen?": false,
            "metadata": { "version": version },
            "all_files": [],
        });
        let (status, body) = put_manifest(&pg, &org, "dep", version, "2", &manifest).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    // v0 and v1 render the segmented manifest with signed download URLs.
    for api_version in ["0", "1"] {
        let manifest = get_manifest(&pg, &org, "apache", "1.0.0", api_version).await;
        assert!(manifest.get("all_files").is_none(), "api v{api_version}");
        assert_eq!(manifest["cookbook_name"], "apache");
        assert_eq!(manifest["recipes"][0]["name"], "default.rb");
        assert!(manifest["recipes"][0]["url"]
            .as_str()
            .unwrap()
            .contains("/bookshelf/organizations/"));
    }

    // v2 renders the flat all_files form.
    let v2 = get_manifest(&pg, &org, "apache", "1.0.0", "2").await;
    assert!(v2.get("recipes").is_none());
    assert_eq!(v2["name"], "apache-1.0.0");
    assert_eq!(v2["version"], "1.0.0");
    assert_eq!(v2["metadata"]["dependencies"]["dep"], "~> 1.0");
    let files = v2["all_files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["path"], "recipes/default.rb");
    assert_eq!(files[0]["checksum"], HELLO_CHECKSUM);
    let blob_url = files[0]["url"].as_str().unwrap();

    // The cookbook list is served from the PostgreSQL rows.
    let listed = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/cookbooks"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(listed["apache"]["versions"][0]["version"], "1.0.0");
    assert_eq!(listed["dep"]["versions"][0]["version"], "2.0.0");

    // Fetch the uploaded recipe through its bookshelf URL.
    let blob_path = blob_url.strip_prefix("http://0.0.0.0:8000").unwrap();
    let response = pg
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(blob_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"hello");

    // Depsolver reads the cookbook graph, metadata and versions from
    // PostgreSQL, including the pessimistic dependency constraint.
    let (status, solved) = send_api(
        &pg,
        "POST",
        &format!("/organizations/{org}/environments/_default/cookbook_versions"),
        br#"{"run_list":["recipe[apache]"]}"#,
        "pivotal",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{solved}");
    assert_eq!(solved["apache"]["version"], "1.0.0");
    assert_eq!(solved["dep"]["version"], "1.0.0");
    assert!(solved["apache"]["all_files"][0]["url"]
        .as_str()
        .unwrap()
        .contains("/checksums/"));

    let (status, missing) = send_api(
        &pg,
        "POST",
        &format!("/organizations/{org}/environments/_default/cookbook_versions"),
        br#"{"run_list":["recipe[missing]"]}"#,
        "pivotal",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        missing["error"][0]["non_existent_cookbooks"],
        json!(["missing"])
    );
}

#[tokio::test]
async fn sandbox_create_commit_and_empty_sandbox_behaviour_work_against_postgresql() {
    let Some(pg) = pg_app().await else {
        return;
    };
    let org = unique_name("pgsandbox");
    create_org(&pg, &org).await;

    // An empty sandbox request is rejected, as are malformed checksums.
    let (status, body) = send(
        &pg,
        "POST",
        &format!("/organizations/{org}/sandboxes"),
        br#"{}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": ["Field 'checksums' missing"]}));

    let (status, body) = send(
        &pg,
        "POST",
        &format!("/organizations/{org}/sandboxes"),
        br#"{"checksums":{}}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": ["Bad checksums!"]}));

    // Create a sandbox with two checksums and verify the response shape.
    let body = serde_json::to_vec(&json!({
        "checksums": { HELLO_CHECKSUM: Value::Null, WORLD_CHECKSUM: Value::Null }
    }))
    .unwrap();
    let (status, created) = send(
        &pg,
        "POST",
        &format!("/organizations/{org}/sandboxes"),
        &body,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["checksums"][HELLO_CHECKSUM]["needs_upload"], true);
    assert_eq!(created["checksums"][WORLD_CHECKSUM]["needs_upload"], true);
    assert!(created["checksums"][HELLO_CHECKSUM]["url"].is_string());
    assert!(created["uri"].as_str().unwrap().ends_with(&format!(
        "/organizations/{org}/sandboxes/{}",
        created["sandbox_id"].as_str().unwrap()
    )));
    let sandbox_id = created["sandbox_id"].as_str().unwrap().to_string();

    // Committing an unknown sandbox is a 404.
    let (status, _) = send(
        &pg,
        "PUT",
        &format!("/organizations/{org}/sandboxes/{MISSING_CHECKSUM}"),
        br#"{"is_completed":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Committing before every checksum is uploaded reports the missing ones.
    let (status, error) = send(
        &pg,
        "PUT",
        &format!("/organizations/{org}/sandboxes/{sandbox_id}"),
        br#"{"is_completed":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let message = error["error"][0].as_str().unwrap();
    assert!(message.contains(HELLO_CHECKSUM), "{message}");
    assert!(message.contains(WORLD_CHECKSUM), "{message}");

    // Upload both blobs and commit; the response mirrors the upstream shape.
    for (checksum, content) in [(HELLO_CHECKSUM, "hello"), (WORLD_CHECKSUM, "world")] {
        let url = created["checksums"][checksum]["url"].as_str().unwrap();
        let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
        let response = pg
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(path)
                    .body(Body::from(content))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let (status, committed) = send(
        &pg,
        "PUT",
        &format!("/organizations/{org}/sandboxes/{sandbox_id}"),
        br#"{"is_completed":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_eq!(committed["guid"], sandbox_id);
    assert_eq!(committed["name"], sandbox_id);
    assert_eq!(committed["is_completed"], true);
    let checksums = committed["checksums"].as_array().unwrap();
    assert_eq!(checksums.len(), 2);
    let create_time = committed["create_time"].as_str().unwrap();
    assert_eq!(create_time.len(), 25, "create_time {create_time}");
    assert!(create_time.ends_with("+00:00"), "{create_time}");

    // A follow-up sandbox recognises the committed checksums.
    let second = ok(
        &pg,
        "POST",
        &format!("/organizations/{org}/sandboxes"),
        &body,
        "pivotal",
    )
    .await;
    assert_eq!(
        second["checksums"][HELLO_CHECKSUM],
        json!({"needs_upload": false})
    );
    assert_eq!(
        second["checksums"][WORLD_CHECKSUM],
        json!({"needs_upload": false})
    );

    // Commit moves the checksums into the durable table and removes the
    // sandbox rows, exactly like the SQLite backend.
    let org_id = pg.store_org_id(&org).await;
    assert!(pg
        .store
        .checksum_exists(&org_id, HELLO_CHECKSUM)
        .await
        .unwrap());
    assert!(pg
        .store
        .checksum_exists(&org_id, WORLD_CHECKSUM)
        .await
        .unwrap());
    assert!(!pg.store.sandbox_exists(&org_id, &sandbox_id).await.unwrap());
}

#[tokio::test]
async fn user_and_client_key_lifecycles_work_against_postgresql() {
    let Some(pg) = pg_app().await else {
        return;
    };
    let org = unique_name("pgkeys");
    let alice = unique_name("alice");
    create_org(&pg, &org).await;
    create_user(&pg, &alice).await;

    // The user's default key is created with the account.
    let keys = ok(&pg, "GET", &format!("/users/{alice}/keys"), b"", "pivotal").await;
    assert_eq!(keys.as_array().unwrap().len(), 1);
    assert_eq!(keys[0]["name"], "default");
    assert_eq!(
        keys[0]["uri"],
        format!("http://0.0.0.0:8000/users/{alice}/keys/default")
    );
    assert_eq!(keys[0]["expired"], false);

    let body = serde_json::to_vec(&json!({
        "name": "alt",
        "public_key": support::PUBLIC_KEY,
        "expiration_date": "infinity",
    }))
    .unwrap();
    let (status, headers, created) = send_with_headers(
        &pg,
        "POST",
        &format!("/users/{alice}/keys"),
        &body,
        "pivotal",
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let expected = format!("http://0.0.0.0:8000/users/{alice}/keys/alt");
    // POST only reports the key location (plus generated private material).
    assert_eq!(created, json!({ "uri": expected }));
    assert!(created.get("name").is_none());
    assert!(created.get("public_key").is_none());
    assert!(created.get("expiration_date").is_none());
    assert!(created.get("private_key").is_none());
    assert_eq!(headers.get(header::LOCATION).unwrap(), expected.as_str());

    // GET returns the full key record, including the exact stored public key.
    let fetched = ok(
        &pg,
        "GET",
        &format!("/users/{alice}/keys/alt"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(fetched["uri"], expected);
    assert_eq!(fetched["name"], "alt");
    assert_eq!(fetched["public_key"], support::PUBLIC_KEY);
    assert_eq!(fetched["expiration_date"], "infinity");
    assert!(fetched.get("private_key").is_none());

    // Renaming returns 201 and a fresh Location; expiration-only updates
    // return 200. Both echo only the fields present in the request.
    let (status, headers, renamed) = send_with_headers(
        &pg,
        "PUT",
        &format!("/users/{alice}/keys/alt"),
        br#"{"name":"renamed","expiration_date":"2050-12-24T21:00:00Z"}"#,
        "pivotal",
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::CREATED, "{renamed}");
    let renamed_uri = format!("http://0.0.0.0:8000/users/{alice}/keys/renamed");
    assert_eq!(
        renamed,
        json!({
            "name": "renamed",
            "expiration_date": "2050-12-24T21:00:00Z",
        })
    );
    assert_eq!(headers.get(header::LOCATION).unwrap(), renamed_uri.as_str());
    let (status, _) = send(
        &pg,
        "GET",
        &format!("/users/{alice}/keys/alt"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let renamed_key = ok(
        &pg,
        "GET",
        &format!("/users/{alice}/keys/renamed"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(renamed_key["uri"], renamed_uri);
    assert_eq!(renamed_key["name"], "renamed");
    assert_eq!(renamed_key["expiration_date"], "2050-12-24T21:00:00Z");

    let (status, value) = send(
        &pg,
        "PUT",
        &format!("/users/{alice}/keys/renamed"),
        br#"{"expiration_date":"infinity"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value, json!({ "expiration_date": "infinity" }));

    let deleted = ok(
        &pg,
        "DELETE",
        &format!("/users/{alice}/keys/renamed"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(deleted["uri"], renamed_uri);
    assert_eq!(deleted["name"], "renamed");
    assert_eq!(deleted["public_key"], support::PUBLIC_KEY);
    assert_eq!(deleted["expiration_date"], "infinity");
    let keys = ok(&pg, "GET", &format!("/users/{alice}/keys"), b"", "pivotal").await;
    assert_eq!(keys.as_array().unwrap().len(), 1);
    assert_eq!(keys[0]["name"], "default");

    // Clients follow the same lifecycle, with the generated default key.
    let (status, _) = send(
        &pg,
        "POST",
        &format!("/organizations/{org}/clients"),
        br#"{"name":"web1","validator":false,"create_key":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let keys = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/clients/web1/keys"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(keys.as_array().unwrap().len(), 1);
    assert_eq!(keys[0]["name"], "default");
    assert_eq!(
        keys[0]["uri"],
        format!("http://0.0.0.0:8000/organizations/{org}/clients/web1/keys/default")
    );

    let body = serde_json::to_vec(&json!({
        "name": "alt",
        "public_key": support::PUBLIC_KEY,
        "expiration_date": "infinity",
    }))
    .unwrap();
    let (status, headers, created) = send_with_headers(
        &pg,
        "POST",
        &format!("/organizations/{org}/clients/web1/keys"),
        &body,
        "pivotal",
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let client_key_uri = format!("http://0.0.0.0:8000/organizations/{org}/clients/web1/keys/alt");
    assert_eq!(created, json!({ "uri": client_key_uri }));
    assert!(created.get("name").is_none());
    assert!(created.get("public_key").is_none());
    assert!(created.get("expiration_date").is_none());
    assert!(created.get("private_key").is_none());
    assert_eq!(
        headers.get(header::LOCATION).unwrap(),
        client_key_uri.as_str()
    );

    let fetched = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/clients/web1/keys/alt"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(fetched["uri"], client_key_uri);
    assert_eq!(fetched["name"], "alt");
    assert_eq!(fetched["public_key"], support::PUBLIC_KEY);
    assert_eq!(fetched["expiration_date"], "infinity");

    let (status, value) = send(
        &pg,
        "PUT",
        &format!("/organizations/{org}/clients/web1/keys/alt"),
        br#"{"expiration_date":"2050-12-24T21:00:00Z"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value, json!({ "expiration_date": "2050-12-24T21:00:00Z" }));

    let deleted = ok(
        &pg,
        "DELETE",
        &format!("/organizations/{org}/clients/web1/keys/alt"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(deleted["uri"], client_key_uri);
    assert_eq!(deleted["name"], "alt");
    assert_eq!(deleted["public_key"], support::PUBLIC_KEY);
    let (status, _) = send(
        &pg,
        "GET",
        &format!("/organizations/{org}/clients/web1/keys/alt"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn policy_and_policy_group_lifecycle_works_against_postgresql() {
    let Some(pg) = pg_app().await else {
        return;
    };
    let org = unique_name("pgpolicy");
    create_org(&pg, &org).await;

    let policies = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/policies"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(policies, json!({}));

    let revision = br#"{"name":"app","revision_id":"rev1","run_list":["recipe[app::default]"],"cookbook_locks":{"app":{"identifier":"abc123","version":"1.0.0"}}}"#;
    let created = ok(
        &pg,
        "POST",
        &format!("/organizations/{org}/policies/app/revisions"),
        revision,
        "pivotal",
    )
    .await;
    assert_eq!(created, serde_json::from_slice::<Value>(revision).unwrap());
    let (status, _) = send(
        &pg,
        "POST",
        &format!("/organizations/{org}/policies/app/revisions"),
        revision,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let listed = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/policies"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(
        listed["app"]["uri"],
        format!("http://0.0.0.0:8000/organizations/{org}/policies/app")
    );
    assert_eq!(listed["app"]["revisions"]["rev1"], json!({}));

    let policy = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/policies/app"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(policy["revisions"]["rev1"], json!({}));
    let fetched = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/policies/app/revisions/rev1"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(fetched["name"], "app");
    assert_eq!(fetched["revision_id"], "rev1");
    assert_eq!(fetched["policy_group_list"], json!([]));

    let group = ok(
        &pg,
        "POST",
        &format!("/organizations/{org}/policy_groups"),
        br#"{"name":"prod"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(
        group["uri"],
        format!("http://0.0.0.0:8000/organizations/{org}/policy_groups/prod")
    );

    let associated = ok(
        &pg,
        "PUT",
        &format!("/organizations/{org}/policy_groups/prod/policies/app"),
        revision,
        "pivotal",
    )
    .await;
    assert_eq!(
        associated,
        serde_json::from_slice::<Value>(revision).unwrap()
    );
    let groups = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/policy_groups"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(
        groups["prod"]["policies"]["app"]["revision_id"],
        json!("rev1")
    );
    let read_group = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/policy_groups/prod"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(read_group["policies"]["app"]["revision_id"], json!("rev1"));

    let fetched = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/policies/app/revisions/rev1"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(fetched["policy_group_list"], json!(["prod"]));

    // Remove the association, then the group and the policy.
    let removed = ok(
        &pg,
        "DELETE",
        &format!("/organizations/{org}/policy_groups/prod/policies/app"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(removed, serde_json::from_slice::<Value>(revision).unwrap());
    let deleted_group = ok(
        &pg,
        "DELETE",
        &format!("/organizations/{org}/policy_groups/prod"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(
        deleted_group,
        json!({
            "uri": format!("http://0.0.0.0:8000/organizations/{org}/policy_groups/prod"),
            "policies": {}
        })
    );
    let (status, _) = send(
        &pg,
        "GET",
        &format!("/organizations/{org}/policy_groups/prod"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let deleted_policy = ok(
        &pg,
        "DELETE",
        &format!("/organizations/{org}/policies/app"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(deleted_policy, json!({ "revisions": { "rev1": {} } }));
    let (status, _) = send(
        &pg,
        "GET",
        &format!("/organizations/{org}/policies/app"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn acl_read_write_matrix_works_against_postgresql() {
    let Some(pg) = pg_app().await else {
        return;
    };
    let org = unique_name("pgacl");
    let other = unique_name("pgother");
    let reader = unique_name("reader");
    create_org(&pg, &org).await;
    create_org(&pg, &other).await;
    create_user(&pg, &reader).await;
    associate_user(&pg, &org, &reader).await;

    // Materialise one of each ACL target through the HTTP API.
    let (status, value) = send(
        &pg,
        "POST",
        &format!("/organizations/{org}/nodes"),
        br#"{"name":"web1","chef_environment":"_default","run_list":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    let (status, value) = send(
        &pg,
        "POST",
        &format!("/organizations/{org}/environments"),
        br#"{"name":"acltest","cookbook_versions":{}}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    let manifest = json!({
        "name": "aclbook-1.0.0",
        "cookbook_name": "aclbook",
        "version": "1.0.0",
        "metadata": { "version": "1.0.0" },
        "all_files": [],
    });
    let (status, value) = put_manifest(&pg, &org, "aclbook", "1.0.0", "2", &manifest).await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    let (status, value) = send(
        &pg,
        "POST",
        &format!("/organizations/{org}/policies/app/revisions"),
        br#"{"name":"app","revision_id":"rev1","run_list":[],"cookbook_locks":{}}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    let (status, value) = send(
        &pg,
        "POST",
        &format!("/organizations/{org}/clients"),
        br#"{"name":"web1","validator":false,"create_key":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");

    // (acl path, object path) pairs. Both the ACL read/write and the
    // permission check that follows run against PostgreSQL.
    let targets = [
        (
            format!("/organizations/{org}/nodes/web1/_acl"),
            format!("/organizations/{org}/nodes/web1"),
        ),
        (
            format!("/organizations/{org}/environments/acltest/_acl"),
            format!("/organizations/{org}/environments/acltest"),
        ),
        (
            format!("/organizations/{org}/clients/web1/_acl"),
            format!("/organizations/{org}/clients/web1"),
        ),
        (
            format!("/organizations/{org}/cookbooks/aclbook/_acl"),
            format!("/organizations/{org}/cookbooks/aclbook/1.0.0"),
        ),
        (
            format!("/organizations/{org}/policies/app/_acl"),
            format!("/organizations/{org}/policies/app"),
        ),
    ];

    for (acl_path, object_path) in &targets {
        let body = format!(r#"{{"actors":["{reader}"],"groups":[]}}"#);
        let response = ok(
            &pg,
            "PUT",
            &format!("{acl_path}/read"),
            body.as_bytes(),
            "pivotal",
        )
        .await;
        assert_eq!(response, json!({}), "{acl_path}");

        let acl = ok(&pg, "GET", acl_path, b"", "pivotal").await;
        assert!(
            acl["read"]["actors"]
                .as_array()
                .unwrap()
                .contains(&json!(reader)),
            "{acl_path}: {acl}"
        );

        let (status, value) = send(&pg, "GET", object_path, b"", &reader).await;
        assert!(
            status.is_success(),
            "reader cannot read {object_path} after ACL grant: {status} {value}"
        );
    }

    // Granular detail exposes the split user/client buckets.
    let granular = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/nodes/web1/_acl?detail=granular"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(granular["read"]["actors"], json!([]));
    assert!(granular["read"]["users"]
        .as_array()
        .unwrap()
        .contains(&json!(reader)));

    // Non-local group ACEs survive the write/read round trip: one global
    // group and one group owned by a different organization.
    let body = format!(r#"{{"actors":[],"groups":["::server-admins","{other}::admins"]}}"#);
    let response = ok(
        &pg,
        "PUT",
        &format!("/organizations/{org}/nodes/web1/_acl/read"),
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(response, json!({}));
    let acl = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/nodes/web1/_acl"),
        b"",
        "pivotal",
    )
    .await;
    let groups = acl["read"]["groups"].as_array().unwrap();
    assert!(groups.contains(&json!("::server-admins")), "{acl}");
    assert!(groups.contains(&json!(format!("{other}::admins"))), "{acl}");
}

#[tokio::test]
async fn membership_invitation_and_org_read_flow_works_against_postgresql() {
    let Some(pg) = pg_app().await else {
        return;
    };
    let org = unique_name("pgmember");
    let bob = unique_name("bob");
    create_org(&pg, &org).await;
    create_user(&pg, &bob).await;

    let invitation = ok(
        &pg,
        "POST",
        &format!("/organizations/{org}/association_requests"),
        format!(r#"{{"user":"{bob}"}}"#).as_bytes(),
        "pivotal",
    )
    .await;
    let invitation_id = invitation["id"].as_str().unwrap().to_string();
    assert_eq!(invitation["org"], org);
    assert_eq!(invitation["user"], bob);

    let invitations = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/association_requests"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(invitations.as_array().unwrap().len(), 1);
    assert_eq!(invitations[0]["id"], invitation_id);

    let user_invitations = ok(
        &pg,
        "GET",
        &format!("/users/{bob}/association_requests"),
        b"",
        &bob,
    )
    .await;
    assert_eq!(user_invitations.as_array().unwrap().len(), 1);
    let count = ok(
        &pg,
        "GET",
        &format!("/users/{bob}/association_requests/count"),
        b"",
        &bob,
    )
    .await;
    assert_eq!(count, json!({"value": 1}));

    let accepted = ok(
        &pg,
        "PUT",
        &format!("/users/{bob}/association_requests/{invitation_id}"),
        br#"{"response":"accept"}"#,
        &bob,
    )
    .await;
    assert_eq!(accepted["id"], invitation_id);
    assert_eq!(accepted["org"], org);

    let count = ok(
        &pg,
        "GET",
        &format!("/users/{bob}/association_requests/count"),
        b"",
        &bob,
    )
    .await;
    assert_eq!(count, json!({"value": 0}));

    let users = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/users"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(
        users.get(&bob),
        Some(&json!(format!(
            "http://0.0.0.0:8000/organizations/{org}/users/{bob}"
        )))
    );
    assert!(users["pivotal"].is_string());

    // New members get read access to the organization and its user list.
    let detail = ok(
        &pg,
        "GET",
        &format!("/organizations/{org}/users/{bob}"),
        b"",
        &bob,
    )
    .await;
    assert_eq!(detail["username"], bob);
    let organizations = ok(
        &pg,
        "GET",
        &format!("/users/{bob}/organizations"),
        b"",
        &bob,
    )
    .await;
    assert_eq!(
        organizations[&org],
        format!("http://0.0.0.0:8000/organizations/{org}")
    );

    // Membership and default group assignment are visible in PostgreSQL.
    let org_id = pg.store_org_id(&org).await;
    let bob_row = pg.store.find_user(&bob).await.unwrap().expect("bob exists");
    assert!(pg.store.is_user_in_org(&org_id, &bob_row.id).await.unwrap());
    let users_group = pg
        .store
        .find_group(&org_id, "users")
        .await
        .unwrap()
        .expect("users group");
    assert!(pg
        .store
        .user_in_group(&users_group.authz_id, &bob_row.authz_id)
        .await
        .unwrap());

    // Removing the member revokes the association again.
    let removed = ok(
        &pg,
        "DELETE",
        &format!("/organizations/{org}/users/{bob}"),
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(removed, json!({}));
    let organizations = ok(
        &pg,
        "GET",
        &format!("/users/{bob}/organizations"),
        b"",
        &bob,
    )
    .await;
    assert_eq!(organizations, json!({}));
}

impl PgApp {
    /// Resolve an organization name to its PostgreSQL id without going
    /// through the API, so tests can assert on store-level state.
    async fn store_org_id(&self, name: &str) -> String {
        self.store
            .find_org(name)
            .await
            .expect("find organization")
            .unwrap_or_else(|| panic!("organization {name} is missing"))
            .id
    }
}
