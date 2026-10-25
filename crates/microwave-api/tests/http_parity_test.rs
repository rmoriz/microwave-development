//! HTTP-route parity matrix: identical signed requests against SQLite and
//! PostgreSQL.
//!
//! Every scenario below runs twice, once against an in-memory SQLite store and
//! once against the PostgreSQL store named by `MICROWAVE_TEST_POSTGRES_URL`.
//! The recorded status, selected headers and (normalized) response body of
//! every step must match. Only genuinely volatile values are normalized:
//! generated ids, timestamps, key material, OAuth secrets/tokens, and the
//! organization id/token embedded in bookshelf URLs.
//!
//! The PostgreSQL half skips only when the variable is unset. A configured but
//! unreachable database fails loudly. Names are unique per run and PostgreSQL
//! rows created by the harness are removed afterwards.

mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use microwave_api::AppState;
use microwave_config::{Config, DatabaseDriver};
use microwave_crypto::SignVersion;
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{ActorType, MetadataStore, SqliteStore};
use microwave_store_postgres::PostgresStore;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Once, OnceLock};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const BASE_URL: &str = "http://0.0.0.0:8000";
const HELLO_CHECKSUM: &str = "5d41402abc4b2a76b9719d911017c592";
const WORLD_CHECKSUM: &str = "7d793037a0760186574b0282f2f435e7";
const MISSING_CHECKSUM: &str = "00000000000000000000000000000000";
const ARTIFACT_ID: &str = "1111111111111111111111111111111111111111";

fn unique_run() -> String {
    format!("hp{}", &Uuid::new_v4().simple().to_string()[..10])
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    run: String,
    app: Router,
    store: Arc<dyn MetadataStore>,
    pivotal_key: Option<String>,
    _object_dir: TempDir,
}

impl Harness {
    async fn sqlite(run: String) -> Self {
        let sqlite = Arc::new(SqliteStore::open_in_memory().expect("open in-memory SQLite store"));
        let pivotal = sqlite
            .create_user("pivotal", "pivotal@example.com", "bcrypt", "h", "s", true)
            .expect("create pivotal");
        sqlite
            .add_actor_key(&pivotal.id, ActorType::User, "default", support::PUBLIC_KEY)
            .expect("add pivotal key");

        let mut config = base_config(DatabaseDriver::Sqlite);
        config.object_store.secret = Some("http-parity-secret".into());
        let object_dir = tempfile::tempdir().expect("create object-store temp dir");
        let object_store = object_store(&object_dir, &config);
        let state = AppState::from_sqlite(sqlite.clone(), Arc::new(config), object_store)
            .expect("build SQLite app state");
        let store: Arc<dyn MetadataStore> = sqlite;

        Self {
            run,
            app: microwave_api::router(state),
            store,
            pivotal_key: None,
            _object_dir: object_dir,
        }
    }

    async fn postgres(run: String) -> Option<Self> {
        let url = test_postgres_url()?;
        ensure_postgres_schema(&url);
        let postgres = PostgresStore::connect_existing(&url)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "MICROWAVE_TEST_POSTGRES_URL is set but connecting to the migrated \
                     test database failed: {error}"
                )
            });

        let store: Arc<dyn MetadataStore> = Arc::new(postgres.clone());
        let pivotal = prepare_pivotal(store.as_ref()).await;
        let keys = store
            .find_actor_keys(&pivotal.id, ActorType::User)
            .await
            .expect("list pivotal keys");
        let pivotal_key = if keys
            .iter()
            .any(|key| key.public_key.trim() == support::PUBLIC_KEY.trim())
        {
            None
        } else {
            let name = format!("parity-{run}");
            store
                .add_actor_key(&pivotal.id, ActorType::User, &name, support::PUBLIC_KEY)
                .await
                .expect("add pivotal parity key");
            Some(name)
        };

        let mut config = base_config(DatabaseDriver::Postgres);
        config.object_store.secret = Some("http-parity-secret".into());
        let object_dir = tempfile::tempdir().expect("create object-store temp dir");
        let object_store = object_store(&object_dir, &config);
        let state = AppState::new(store.clone(), Arc::new(config), object_store)
            .expect("build PostgreSQL app state");

        Some(Self {
            run,
            app: microwave_api::router(state),
            store,
            pivotal_key,
            _object_dir: object_dir,
        })
    }

    fn name(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.run)
    }

    fn org(&self) -> String {
        self.name("acme")
    }

    async fn send(&self, label: &str, method: &str, path: &str, body: &[u8], user: &str) -> Step {
        self.send_api(label, method, path, body, user, "2").await
    }

    async fn send_api(
        &self,
        label: &str,
        method: &str,
        path: &str,
        body: &[u8],
        user: &str,
        api_version: &str,
    ) -> Step {
        let response = self
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
            .expect("router handles signed request");
        Step::from_response(label, response).await
    }

    async fn send_anon(
        &self,
        label: &str,
        method: &str,
        path: &str,
        body: &[u8],
        content_type: &str,
        headers: &[(&str, String)],
    ) -> Step {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, content_type);
        for (name, value) in headers {
            builder = builder.header(*name, value);
        }
        let response = self
            .app
            .clone()
            .oneshot(builder.body(Body::from(body.to_vec())).unwrap())
            .await
            .expect("router handles anonymous request");
        Step::from_response(label, response).await
    }

    /// Unsigned bookshelf transfer (blob upload or download).
    async fn transfer(&self, label: &str, method: &str, path: &str, body: &[u8]) -> Step {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::from(body.to_vec()))
                    .unwrap(),
            )
            .await
            .expect("router handles bookshelf request");
        Step::from_response(label, response).await
    }

    async fn cleanup(&self) {
        let prefix = &self.run;

        if let Ok(orgs) = self.store.list_orgs().await {
            for org in orgs.into_iter().filter(|org| org.name.starts_with(prefix)) {
                let _ = self.store.delete_org(&org.id).await;
            }
        }
        if let Ok(users) = self.store.list_users().await {
            for user in users
                .into_iter()
                .filter(|user| user.username.starts_with(prefix))
            {
                let _ = self.store.delete_user(&user.username).await;
            }
        }
        if let Ok(applications) = self.store.list_oauth_applications().await {
            for application in applications
                .into_iter()
                .filter(|application| application.name.starts_with(prefix))
            {
                let _ = self.store.delete_oauth_application(application.id).await;
            }
        }
        if let Some(key_name) = &self.pivotal_key {
            if let Ok(Some(pivotal)) = self.store.find_user("pivotal").await {
                let _ = self
                    .store
                    .delete_actor_key(&pivotal.id, ActorType::User, key_name)
                    .await;
            }
        }
    }
}

fn base_config(driver: DatabaseDriver) -> Config {
    let mut config = Config::default();
    support::disable_keygen(&mut config);
    config.database.driver = driver;
    config
}

fn object_store(object_dir: &TempDir, config: &Config) -> Arc<dyn ObjectStore + Send + Sync> {
    let secret = config
        .object_store
        .secret
        .clone()
        .expect("object-store secret configured");
    Arc::new(LocalObjectStore::new(
        object_dir.path().join("bookshelf"),
        &secret,
        BASE_URL,
    ))
}

fn test_postgres_url() -> Option<String> {
    let Some(value) = std::env::var_os("MICROWAVE_TEST_POSTGRES_URL") else {
        static WARN: Once = Once::new();
        WARN.call_once(|| {
            eprintln!(
                "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; the SQLite half of \
                 the HTTP parity matrix still runs. Use scripts/run-postgres-tests.sh \
                 for PostgreSQL parity."
            );
        });
        return None;
    };
    let url = value
        .into_string()
        .expect("MICROWAVE_TEST_POSTGRES_URL must be valid UTF-8");
    assert!(
        !url.trim().is_empty(),
        "MICROWAVE_TEST_POSTGRES_URL is set but empty; unset it to skip PostgreSQL \
         or provide a connection URL"
    );
    Some(url)
}

/// Apply the embedded schema exactly once per test binary.
///
/// Each `#[tokio::test]` owns its runtime, so a shared pool cannot be reused
/// across tests. A process-wide `OnceLock` keeps migration DDL from running
/// concurrently with another test's DML, which can deadlock on PostgreSQL.
fn ensure_postgres_schema(url: &str) {
    static MIGRATED: OnceLock<Result<(), String>> = OnceLock::new();
    let result = MIGRATED.get_or_init(|| {
        let url = url.to_string();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("build migration runtime: {error}"))?;
            runtime.block_on(async move {
                PostgresStore::connect(&url)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        })
        .join()
        .unwrap_or_else(|_| Err("PostgreSQL migration thread panicked".to_string()))
    });
    if let Err(error) = result {
        panic!(
            "MICROWAVE_TEST_POSTGRES_URL is set but connecting or migrating the \
             test database failed: {error}"
        );
    }
}

async fn prepare_pivotal(store: &dyn MetadataStore) -> microwave_store::User {
    match store.find_user("pivotal").await.expect("find superuser") {
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
    }
}

// ---------------------------------------------------------------------------
// Recorded responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum RecordedBody {
    Json(Value),
    Text(String),
    Empty,
}

#[derive(Debug, Clone)]
struct Step {
    label: String,
    status: StatusCode,
    location: Option<String>,
    content_type: Option<String>,
    body: RecordedBody,
}

impl Step {
    async fn from_response(label: &str, response: axum::response::Response) -> Self {
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let body = if bytes.is_empty() {
            RecordedBody::Empty
        } else {
            match serde_json::from_slice(&bytes) {
                Ok(value) => RecordedBody::Json(value),
                Err(_) => RecordedBody::Text(String::from_utf8_lossy(&bytes).into_owned()),
            }
        };
        Self {
            label: label.to_string(),
            status,
            location,
            content_type,
            body,
        }
    }

    fn expect_status(self, expected: StatusCode) -> Self {
        assert_eq!(
            self.status, expected,
            "step '{}' returned {}: {:?}",
            self.label, self.status, self.body
        );
        self
    }

    fn expect_ok(self) -> Self {
        assert!(
            self.status.is_success(),
            "step '{}' returned {}: {:?}",
            self.label,
            self.status,
            self.body
        );
        self
    }

    fn json(&self) -> &Value {
        match &self.body {
            RecordedBody::Json(value) => value,
            other => panic!("step '{}' is not JSON: {other:?}", self.label),
        }
    }

    fn map_json(self, map: impl FnOnce(&Value) -> Value) -> Self {
        let body = match &self.body {
            RecordedBody::Json(value) => RecordedBody::Json(map(value)),
            other => panic!("step '{}' is not JSON: {other:?}", self.label),
        };
        Self { body, ..self }
    }

    fn normalized(&self) -> Value {
        let body = match &self.body {
            RecordedBody::Json(value) => normalize_value(value, None),
            RecordedBody::Text(text) if self.label.contains("prometheus") => {
                Value::String(mask_digits(&normalize_string(text)))
            }
            RecordedBody::Text(text) => Value::String(normalize_string(text)),
            RecordedBody::Empty => Value::Null,
        };
        json!({
            "status": self.status.as_u16(),
            "location": self.location.as_deref().map(normalize_string),
            "content_type": self.content_type,
            "body": body,
        })
    }
}

fn compare_transcripts(test: &str, sqlite: &[Step], postgres: &[Step]) {
    assert!(
        !sqlite.is_empty(),
        "{test}: SQLite half produced no observations"
    );
    assert_eq!(
        sqlite.len(),
        postgres.len(),
        "{test}: different number of observed HTTP steps"
    );
    for (left, right) in sqlite.iter().zip(postgres) {
        assert_eq!(
            left.label, right.label,
            "{test}: step ordering differs at '{}' vs '{}'",
            left.label, right.label
        );
        let normalized_left = left.normalized();
        let normalized_right = right.normalized();
        assert_eq!(
            normalized_left,
            normalized_right,
            "{test}: HTTP parity mismatch for step '{}'\nSQLite: {}\nPostgreSQL: {}",
            right.label,
            serde_json::to_string_pretty(&normalized_left).unwrap(),
            serde_json::to_string_pretty(&normalized_right).unwrap()
        );
    }
}

fn normalize_value(value: &Value, key: Option<&str>) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (name, value) in map {
                out.insert(name.clone(), normalize_value(value, Some(name)));
            }
            Value::Object(out)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| normalize_value(value, None))
                .collect(),
        ),
        Value::String(text) => Value::String(normalize_scalar(text, key)),
        Value::Number(_) if key == Some("id") => Value::String("<id>".into()),
        Value::Number(_)
            if matches!(
                key,
                Some("create_time") | Some("created_at") | Some("updated_at")
            ) =>
        {
            Value::String("<timestamp>".into())
        }
        other => other.clone(),
    }
}

fn normalize_scalar(text: &str, key: Option<&str>) -> String {
    match key {
        Some("private_key") if text.contains("BEGIN") => "<private_key>".into(),
        Some("public_key") if text.contains("BEGIN") => "<public_key>".into(),
        Some("secret") => "<secret>".into(),
        Some("uid") => "<uid>".into(),
        Some("access_token") | Some("refresh_token") => "<token>".into(),
        Some("create_time") | Some("created_at") | Some("updated_at") => "<timestamp>".into(),
        Some("sandbox_id") | Some("guid") => "<id>".into(),
        Some("id") | Some("name") if is_hex_id(text) => "<id>".into(),
        _ => normalize_string(text),
    }
}

fn normalize_string(text: &str) -> String {
    if text.contains("BEGIN PRIVATE KEY") {
        return "<private_key>".into();
    }
    let mut text = replace_path_id(text, "/bookshelf/organizations/");
    text = replace_path_id(&text, "/sandboxes/");
    text = replace_path_id(&text, "/association_requests/");
    text = replace_path_id(&text, "sandbox ");
    text = normalize_numeric_ids(&text);
    for name in [
        "token",
        "X-Amz-Signature",
        "X-Amz-Date",
        "X-Amz-Credential",
        "X-Amz-Security-Token",
    ] {
        text = replace_query_value(&text, name);
    }
    text
}

fn replace_path_id(text: &str, marker: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(index) = rest.find(marker) {
        let end = index + marker.len();
        out.push_str(&rest[..end]);
        let after = &rest[end..];
        let length = after
            .bytes()
            .take_while(|byte| byte.is_ascii_hexdigit())
            .count();
        if length == 32 {
            out.push_str("<id>");
            rest = &after[length..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn replace_query_value(text: &str, name: &str) -> String {
    let needle = format!("{name}=");
    let mut out = String::new();
    let mut rest = text;
    while let Some(index) = rest.find(&needle) {
        let end = index + needle.len();
        out.push_str(&rest[..end]);
        out.push_str("<value>");
        let after = &rest[end..];
        let length = after
            .bytes()
            .take_while(|byte| *byte != b'&' && *byte != b'#' && *byte != b',')
            .count();
        rest = &after[length..];
    }
    out.push_str(rest);
    out
}

/// Error messages embed numeric OAuth application ids (for example
/// `oauth application 6`); the concrete id is allocated by the backend.
fn normalize_numeric_ids(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(index) = rest.find("oauth application ") {
        let end = index + "oauth application ".len();
        out.push_str(&rest[..end]);
        let after = &rest[end..];
        let length = after
            .bytes()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if length > 0 {
            out.push_str("<id>");
            rest = &after[length..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn is_hex_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn mask_digits(text: &str) -> String {
    let mut out = String::new();
    let mut in_digits = false;
    for character in text.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                out.push_str("<n>");
                in_digits = true;
            }
        } else {
            in_digits = false;
            out.push(character);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Shared request helpers
// ---------------------------------------------------------------------------

async fn create_org(h: &Harness, org: &str) -> Step {
    let body = json!({ "name": org, "full_name": format!("Parity {org}") });
    let step = h
        .send(
            "create organization",
            "POST",
            "/organizations",
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(step.json()["name"], org);
    assert_eq!(step.json()["clientname"], format!("{org}-validator"));
    step
}

async fn create_user(h: &Harness, username: &str) -> Step {
    let body = json!({
        "username": username,
        "email": format!("{username}@example.test"),
        "display_name": username,
        "first_name": "Parity",
        "last_name": "User",
        "password": "secret",
        "public_key": support::PUBLIC_KEY,
    });
    let step = h
        .send(
            "create user",
            "POST",
            "/users",
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(step.json()["uri"], format!("{BASE_URL}/users/{username}"));
    step
}

async fn add_user_to_org(h: &Harness, org: &str, username: &str) -> Step {
    let body = json!({ "username": username });
    h.send(
        "add organization member",
        "POST",
        &format!("/organizations/{org}/users"),
        &serde_json::to_vec(&body).unwrap(),
        "pivotal",
    )
    .await
    .expect_status(StatusCode::CREATED)
}

async fn create_node(h: &Harness, org: &str, name: &str, environment: &str) -> Step {
    let body = json!({
        "name": name,
        "chef_environment": environment,
        "run_list": ["recipe[apache]"],
        "normal": { "role": "web", "platform": "ubuntu" },
        "automatic": { "os": "linux" },
    });
    let step = h
        .send(
            "create node",
            "POST",
            &format!("/organizations/{org}/nodes"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.json()["uri"],
        format!("{BASE_URL}/organizations/{org}/nodes/{name}")
    );
    step
}

async fn put_cookbook(h: &Harness, org: &str, name: &str, version: &str, manifest: Value) -> Step {
    h.send_api(
        "put cookbook",
        "PUT",
        &format!("/organizations/{org}/cookbooks/{name}/{version}"),
        &serde_json::to_vec(&manifest).unwrap(),
        "pivotal",
        "2",
    )
    .await
}

fn cookbook_manifest(name: &str, version: &str, dependencies: Value, files: Value) -> Value {
    json!({
        "name": format!("{name}-{version}"),
        "cookbook_name": name,
        "version": version,
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "metadata": {
            "name": name,
            "version": version,
            "description": "HTTP parity cookbook",
            "dependencies": dependencies,
        },
        "all_files": files,
    })
}

// ---------------------------------------------------------------------------
// Scenario: users, organizations, clients and keys
// ---------------------------------------------------------------------------

async fn scenario_identity(h: &Harness) -> Vec<Step> {
    let org = h.org();
    let alice = h.name("alice");
    let alice_renamed = h.name("alice2");
    let mut steps = Vec::new();

    let step = create_user(h, &alice).await;
    assert_eq!(
        step.json()["chef_key"]["public_key"]
            .as_str()
            .unwrap()
            .trim(),
        support::PUBLIC_KEY.trim()
    );
    steps.push(step);

    // The filtered user list only contains this run's user.
    let step = h
        .send(
            "list filtered users",
            "GET",
            &format!("/users?email={alice}@example.test"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json(),
        &json!({ alice.clone(): format!("{BASE_URL}/users/{alice}") })
    );
    steps.push(step);

    let step = h
        .send(
            "read user",
            "GET",
            &format!("/users/{alice}"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["username"], alice);
    assert_eq!(step.json()["first_name"], "Parity");
    steps.push(step);

    let body = json!({
        "username": alice_renamed,
        "email": format!("{alice}@example.test"),
        "display_name": "Alice Renamed",
        "first_name": "Alice",
        "last_name": "Liddell",
    });
    let step = h
        .send(
            "rename user",
            "PUT",
            &format!("/users/{alice}"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.location.as_deref(),
        Some(format!("{BASE_URL}/users/{alice_renamed}").as_str())
    );
    assert_eq!(
        step.json()["uri"],
        format!("{BASE_URL}/users/{alice_renamed}")
    );
    steps.push(step);

    let step = h
        .send(
            "read renamed-away user",
            "GET",
            &format!("/users/{alice}"),
            b"",
            "pivotal",
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    steps.push(step);

    let step = h
        .send(
            "read renamed user",
            "GET",
            &format!("/users/{alice_renamed}"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["username"], alice_renamed);
    assert_eq!(step.json()["display_name"], "Alice Renamed");
    steps.push(step);

    let body = json!({
        "name": "alt",
        "public_key": support::PUBLIC_KEY,
        "expiration_date": "infinity",
    });
    let step = h
        .send(
            "create user key",
            "POST",
            &format!("/users/{alice_renamed}/keys"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.location.as_deref(),
        Some(format!("{BASE_URL}/users/{alice_renamed}/keys/alt").as_str())
    );
    assert_eq!(
        step.json(),
        &json!({ "uri": format!("{BASE_URL}/users/{alice_renamed}/keys/alt") })
    );
    steps.push(step);

    let step = h
        .send(
            "read user key",
            "GET",
            &format!("/users/{alice_renamed}/keys/alt"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], "alt");
    assert_eq!(
        step.json()["public_key"].as_str().unwrap().trim(),
        support::PUBLIC_KEY.trim()
    );
    steps.push(step);

    let step = h
        .send(
            "list user keys",
            "GET",
            &format!("/users/{alice_renamed}/keys"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    let mut names: Vec<&str> = step
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["alt", "default"]);
    steps.push(step);

    let step = h
        .send(
            "rename user key",
            "PUT",
            &format!("/users/{alice_renamed}/keys/alt"),
            br#"{"name":"renamed","expiration_date":"2050-12-24T21:00:00Z"}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.location.as_deref(),
        Some(format!("{BASE_URL}/users/{alice_renamed}/keys/renamed").as_str())
    );
    assert_eq!(
        step.json(),
        &json!({ "name": "renamed", "expiration_date": "2050-12-24T21:00:00Z" })
    );
    steps.push(step);

    let step = h
        .send(
            "update user key expiration",
            "PUT",
            &format!("/users/{alice_renamed}/keys/renamed"),
            br#"{"expiration_date":"infinity"}"#,
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({ "expiration_date": "infinity" }));
    steps.push(step);

    let step = h
        .send(
            "delete user key",
            "DELETE",
            &format!("/users/{alice_renamed}/keys/renamed"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], "renamed");
    steps.push(step);

    // Organization CRUD and the provisioned validator client.
    steps.push(create_org(h, &org).await);

    let step = h
        .send(
            "read organization",
            "GET",
            &format!("/organizations/{org}"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], org);
    assert_eq!(step.json()["full_name"], format!("Parity {org}"));
    steps.push(step);

    let body = json!({ "name": org, "full_name": "Parity Organization Updated" });
    let step = h
        .send(
            "update organization",
            "PUT",
            &format!("/organizations/{org}"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &body);
    steps.push(step);

    let body = json!({
        "name": "web1",
        "validator": false,
        "public_key": support::PUBLIC_KEY,
    });
    let step = h
        .send(
            "create client",
            "POST",
            &format!("/organizations/{org}/clients"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.json()["uri"],
        format!("{BASE_URL}/organizations/{org}/clients/web1")
    );
    assert_eq!(
        step.json()["chef_key"]["public_key"]
            .as_str()
            .unwrap()
            .trim(),
        support::PUBLIC_KEY.trim()
    );
    steps.push(step);

    let step = h
        .send(
            "list clients",
            "GET",
            &format!("/organizations/{org}/clients"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    let clients = step.json().as_object().unwrap();
    assert!(clients.contains_key("web1"));
    assert!(clients.contains_key(&format!("{org}-validator")));
    steps.push(step);

    let step = h
        .send(
            "read client",
            "GET",
            &format!("/organizations/{org}/clients/web1"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], "web1");
    assert_eq!(step.json()["orgname"], org);
    steps.push(step);

    let body = json!({
        "name": "alt",
        "public_key": support::PUBLIC_KEY,
        "expiration_date": "infinity",
    });
    let step = h
        .send(
            "create client key",
            "POST",
            &format!("/organizations/{org}/clients/web1/keys"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.location.as_deref(),
        Some(format!("{BASE_URL}/organizations/{org}/clients/web1/keys/alt").as_str())
    );
    steps.push(step);

    let step = h
        .send(
            "list client keys",
            "GET",
            &format!("/organizations/{org}/clients/web1/keys"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json().as_array().unwrap().len(), 2);
    steps.push(step);

    let step = h
        .send(
            "delete client key",
            "DELETE",
            &format!("/organizations/{org}/clients/web1/keys/alt"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], "alt");
    steps.push(step);

    let step = h
        .send(
            "rename client",
            "PUT",
            &format!("/organizations/{org}/clients/web1"),
            br#"{"name":"web2","validator":false}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(step.json()["name"], "web2");
    assert_eq!(step.json()["private_key"], false);
    steps.push(step);

    let step = h
        .send(
            "read renamed-away client",
            "GET",
            &format!("/organizations/{org}/clients/web1"),
            b"",
            "pivotal",
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    steps.push(step);

    let step = h
        .send(
            "delete client",
            "DELETE",
            &format!("/organizations/{org}/clients/web2"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], "web2");
    steps.push(step);

    steps
}

// ---------------------------------------------------------------------------
// Scenario: nodes, roles, environments, data bags and search
// ---------------------------------------------------------------------------

async fn scenario_objects(h: &Harness) -> Vec<Step> {
    let org = h.org();
    let mut steps = vec![create_org(h, &org).await];

    // Nodes.
    steps.push(create_node(h, &org, "web1", "production").await);
    steps.push(create_node(h, &org, "web2", "staging").await);

    let step = h
        .send(
            "list nodes",
            "GET",
            &format!("/organizations/{org}/nodes"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json()["web1"],
        format!("{BASE_URL}/organizations/{org}/nodes/web1")
    );
    assert_eq!(
        step.json()["web2"],
        format!("{BASE_URL}/organizations/{org}/nodes/web2")
    );
    steps.push(step);

    let step = h
        .send(
            "read node",
            "GET",
            &format!("/organizations/{org}/nodes/web1"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["chef_environment"], "production");
    assert_eq!(step.json()["normal"]["role"], "web");
    steps.push(step);

    // Roles.
    let body = json!({
        "name": "web",
        "run_list": ["recipe[apache]"],
        "default_attributes": { "tier": "frontend" },
    });
    let step = h
        .send(
            "create role",
            "POST",
            &format!("/organizations/{org}/roles"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.json()["uri"],
        format!("{BASE_URL}/organizations/{org}/roles/web")
    );
    steps.push(step);

    let step = h
        .send(
            "read role",
            "GET",
            &format!("/organizations/{org}/roles/web"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["json_class"], "Chef::Role");
    assert_eq!(step.json()["run_list"], json!(["recipe[apache]"]));
    steps.push(step);

    let step = h
        .send(
            "list roles",
            "GET",
            &format!("/organizations/{org}/roles"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert!(step.json()["web"].is_string());
    steps.push(step);

    let step = h
        .send(
            "update role",
            "PUT",
            &format!("/organizations/{org}/roles/web"),
            br#"{"name":"web","run_list":["recipe[base]"]}"#,
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["run_list"], json!(["recipe[base]"]));
    steps.push(step);

    // Environments.
    let step = h
        .send(
            "create environment",
            "POST",
            &format!("/organizations/{org}/environments"),
            br#"{"name":"production","description":"prod","cookbook_versions":{}}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    steps.push(step);

    let step = h
        .send(
            "create staging environment",
            "POST",
            &format!("/organizations/{org}/environments"),
            br#"{"name":"staging","description":"staging","cookbook_versions":{}}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    steps.push(step);

    let step = h
        .send(
            "list environments",
            "GET",
            &format!("/organizations/{org}/environments"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert!(step.json()["_default"].is_string());
    assert!(step.json()["production"].is_string());
    steps.push(step);

    let step = h
        .send(
            "read environment",
            "GET",
            &format!("/organizations/{org}/environments/production"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["description"], "prod");
    steps.push(step);

    let step = h
        .send(
            "update environment",
            "PUT",
            &format!("/organizations/{org}/environments/production"),
            br#"{"name":"production","description":"updated","cookbook_versions":{}}"#,
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["description"], "updated");
    steps.push(step);

    let step = h
        .send(
            "update default environment",
            "PUT",
            &format!("/organizations/{org}/environments/_default"),
            br#"{"name":"_default"}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::METHOD_NOT_ALLOWED);
    steps.push(step);

    let step = h
        .send(
            "list environment nodes",
            "GET",
            &format!("/organizations/{org}/environments/production/nodes"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert!(step.json()["web1"].is_string());
    assert!(step.json().get("web2").is_none());
    steps.push(step);

    // Data bags and items.
    let step = h
        .send(
            "create data bag",
            "POST",
            &format!("/organizations/{org}/data"),
            br#"{"name":"secrets"}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.json()["uri"],
        format!("{BASE_URL}/organizations/{org}/data/secrets")
    );
    steps.push(step);

    let step = h
        .send(
            "create data bag item",
            "POST",
            &format!("/organizations/{org}/data/secrets"),
            br#"{"id":"database","password":"one"}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(step.json()["id"], "database");
    assert_eq!(step.json()["password"], "one");
    steps.push(step);

    let step = h
        .send(
            "list data bag items",
            "GET",
            &format!("/organizations/{org}/data/secrets"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json()["database"],
        format!("{BASE_URL}/organizations/{org}/data/secrets/database")
    );
    steps.push(step);

    let step = h
        .send(
            "read data bag item",
            "GET",
            &format!("/organizations/{org}/data/secrets/database"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["password"], "one");
    steps.push(step);

    let step = h
        .send(
            "update data bag item",
            "PUT",
            &format!("/organizations/{org}/data/secrets/database"),
            br#"{"id":"database","password":"two"}"#,
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["password"], "two");
    steps.push(step);

    // Search indexes and queries, while all objects exist.
    let step = h
        .send(
            "list search indexes",
            "GET",
            &format!("/organizations/{org}/search"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    for index in ["client", "environment", "node", "role", "secrets"] {
        assert!(
            step.json()[index].is_string(),
            "missing search index {index}"
        );
    }
    steps.push(step);

    let step = h
        .send(
            "search all nodes",
            "GET",
            &format!("/organizations/{org}/search/node?q=*:*"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["total"], 2);
    steps.push(step);

    let step = h
        .send(
            "search nodes by environment",
            "GET",
            &format!("/organizations/{org}/search/node?q=chef_environment:production"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["total"], 1);
    assert_eq!(step.json()["rows"][0]["name"], "web1");
    steps.push(step);

    let step = h
        .send(
            "search nodes by attribute",
            "GET",
            &format!("/organizations/{org}/search/node?q=platform:ubuntu"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["total"], 2);
    steps.push(step);

    let step = h
        .send(
            "search data bag items",
            "GET",
            &format!("/organizations/{org}/search/secrets?q=*:*"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["total"], 1);
    assert_eq!(step.json()["rows"][0]["raw_data"]["password"], "two");
    steps.push(step);

    let step = h
        .send(
            "partial search nodes",
            "POST",
            &format!("/organizations/{org}/search/node?q=*:*"),
            br#"{"name":["name"],"environment":["chef_environment"]}"#,
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["total"], 2);
    steps.push(step);

    // Updates and deletes.
    let step = h
        .send(
            "update node",
            "PUT",
            &format!("/organizations/{org}/nodes/web1"),
            br#"{"name":"web1","chef_environment":"staging","run_list":[],"normal":{"role":"db"}}"#,
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["chef_environment"], "staging");
    steps.push(step);

    let step = h
        .send(
            "list staging nodes",
            "GET",
            &format!("/organizations/{org}/environments/staging/nodes"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json().as_object().unwrap().len(), 2);
    steps.push(step);

    let step = h
        .send(
            "delete node",
            "DELETE",
            &format!("/organizations/{org}/nodes/web2"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], "web2");
    steps.push(step);

    let step = h
        .send(
            "read deleted node",
            "GET",
            &format!("/organizations/{org}/nodes/web2"),
            b"",
            "pivotal",
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    steps.push(step);

    let step = h
        .send(
            "delete role",
            "DELETE",
            &format!("/organizations/{org}/roles/web"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({}));
    steps.push(step);

    let step = h
        .send(
            "delete environment",
            "DELETE",
            &format!("/organizations/{org}/environments/production"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({}));
    steps.push(step);

    let step = h
        .send(
            "delete data bag item",
            "DELETE",
            &format!("/organizations/{org}/data/secrets/database"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], "data_bag_item_secrets_database");
    assert_eq!(step.json()["raw_data"]["password"], "two");
    steps.push(step);

    let step = h
        .send(
            "delete data bag",
            "DELETE",
            &format!("/organizations/{org}/data/secrets"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], "secrets");
    steps.push(step);

    let step = h
        .send(
            "read deleted data bag",
            "GET",
            &format!("/organizations/{org}/data/secrets"),
            b"",
            "pivotal",
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    steps.push(step);

    steps
}

// ---------------------------------------------------------------------------
// Scenario: sandboxes, cookbooks, artifacts and depsolver
// ---------------------------------------------------------------------------

async fn scenario_content(h: &Harness) -> Vec<Step> {
    let org = h.org();
    let mut steps = vec![create_org(h, &org).await];

    // Sandbox validation.
    let step = h
        .send(
            "create empty sandbox",
            "POST",
            &format!("/organizations/{org}/sandboxes"),
            br#"{}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        step.json(),
        &json!({"error": ["Field 'checksums' missing"]})
    );
    steps.push(step);

    let step = h
        .send(
            "create malformed sandbox",
            "POST",
            &format!("/organizations/{org}/sandboxes"),
            br#"{"checksums":{}}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::BAD_REQUEST);
    assert_eq!(step.json(), &json!({"error": ["Bad checksums!"]}));
    steps.push(step);

    let body = json!({
        "checksums": { HELLO_CHECKSUM: Value::Null, WORLD_CHECKSUM: Value::Null }
    });
    let created = h
        .send(
            "create sandbox",
            "POST",
            &format!("/organizations/{org}/sandboxes"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        created.json()["checksums"][HELLO_CHECKSUM]["needs_upload"],
        true
    );
    assert_eq!(
        created.json()["checksums"][WORLD_CHECKSUM]["needs_upload"],
        true
    );
    assert!(created.json()["checksums"][HELLO_CHECKSUM]["url"].is_string());
    let sandbox_id = created.json()["sandbox_id"].as_str().unwrap().to_string();
    assert_eq!(
        created.json()["uri"],
        format!("{BASE_URL}/organizations/{org}/sandboxes/{sandbox_id}")
    );
    let hello_url = created.json()["checksums"][HELLO_CHECKSUM]["url"]
        .as_str()
        .unwrap()
        .to_string();
    let world_url = created.json()["checksums"][WORLD_CHECKSUM]["url"]
        .as_str()
        .unwrap()
        .to_string();
    steps.push(created);

    let step = h
        .send(
            "commit unknown sandbox",
            "PUT",
            &format!("/organizations/{org}/sandboxes/{MISSING_CHECKSUM}"),
            br#"{"is_completed":true}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    steps.push(step);

    let step = h
        .send(
            "commit incomplete sandbox",
            "PUT",
            &format!("/organizations/{org}/sandboxes/{sandbox_id}"),
            br#"{"is_completed":true}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::SERVICE_UNAVAILABLE);
    let message = step.json()["error"][0].as_str().unwrap();
    assert!(message.contains(HELLO_CHECKSUM));
    assert!(message.contains(WORLD_CHECKSUM));
    steps.push(step);

    for (label, url, contents) in [
        ("upload hello blob", &hello_url, "hello"),
        ("upload world blob", &world_url, "world"),
    ] {
        let path = url.strip_prefix(BASE_URL).unwrap();
        let step = h
            .transfer(label, "PUT", path, contents.as_bytes())
            .await
            .expect_ok();
        steps.push(step);
    }

    let step = h
        .send(
            "commit sandbox",
            "PUT",
            &format!("/organizations/{org}/sandboxes/{sandbox_id}"),
            br#"{"is_completed":true}"#,
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["guid"], sandbox_id);
    assert_eq!(step.json()["is_completed"], true);
    assert_eq!(step.json()["checksums"].as_array().unwrap().len(), 2);
    steps.push(step);

    let step = h
        .send(
            "reuse committed checksums",
            "POST",
            &format!("/organizations/{org}/sandboxes"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.json()["checksums"][HELLO_CHECKSUM],
        json!({"needs_upload": false})
    );
    assert_eq!(
        step.json()["checksums"][WORLD_CHECKSUM],
        json!({"needs_upload": false})
    );
    steps.push(step);

    // Cookbooks, including a version with a real blob.
    let files = json!([{
        "name": "recipes/default.rb",
        "path": "recipes/default.rb",
        "checksum": HELLO_CHECKSUM,
        "specificity": "default",
    }]);
    let mut apache = cookbook_manifest("apache", "1.0.0", json!({ "dep": "~> 1.0" }), files);
    apache["metadata"]["description"] = json!("A fabulous new cookbook");

    let step = put_cookbook(h, &org, "apache", "1.0.0", apache)
        .await
        .expect_status(StatusCode::CREATED);
    steps.push(step);

    for version in ["1.0.0", "2.0.0"] {
        let manifest = cookbook_manifest("dep", version, json!({}), json!([]));
        let step = put_cookbook(h, &org, "dep", version, manifest)
            .await
            .expect_status(StatusCode::CREATED);
        steps.push(step);
    }

    let step = h
        .send(
            "list cookbooks",
            "GET",
            &format!("/organizations/{org}/cookbooks"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["apache"]["versions"][0]["version"], "1.0.0");
    assert_eq!(step.json()["dep"]["versions"].as_array().unwrap().len(), 1);
    assert_eq!(step.json()["dep"]["versions"][0]["version"], "2.0.0");
    steps.push(step);

    let step = h
        .send_api(
            "read cookbook v2",
            "GET",
            &format!("/organizations/{org}/cookbooks/apache/1.0.0"),
            b"",
            "pivotal",
            "2",
        )
        .await
        .expect_ok();
    let blob_url = step.json()["all_files"][0]["url"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(step.json()["all_files"][0]["checksum"], HELLO_CHECKSUM);
    steps.push(step);

    let step = h
        .send_api(
            "read cookbook v0",
            "GET",
            &format!("/organizations/{org}/cookbooks/apache/1.0.0"),
            b"",
            "pivotal",
            "0",
        )
        .await
        .expect_ok();
    assert!(step.json().get("all_files").is_none());
    assert!(step.json()["recipes"][0]["url"]
        .as_str()
        .unwrap()
        .contains("/bookshelf/organizations/"));
    steps.push(step);

    let path = blob_url.strip_prefix(BASE_URL).unwrap();
    let step = h
        .transfer("download cookbook blob", "GET", path, b"")
        .await
        .expect_ok();
    assert_eq!(step.body_text(), "hello");
    steps.push(step);

    let step = h
        .send(
            "cookbook universe",
            "GET",
            &format!("/organizations/{org}/universe"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json()["apache"]["1.0.0"]["dependencies"]["dep"],
        "~> 1.0"
    );
    steps.push(step);

    // Depsolver reads the cookbook graph through either backend.
    let step = h
        .send_api(
            "solve cookbook dependencies",
            "POST",
            &format!("/organizations/{org}/environments/_default/cookbook_versions"),
            br#"{"run_list":["recipe[apache]"]}"#,
            "pivotal",
            "2",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["apache"]["version"], "1.0.0");
    assert_eq!(step.json()["dep"]["version"], "1.0.0");
    steps.push(step);

    let step = h
        .send_api(
            "solve missing cookbook",
            "POST",
            &format!("/organizations/{org}/environments/_default/cookbook_versions"),
            br#"{"run_list":["recipe[missing]"]}"#,
            "pivotal",
            "2",
        )
        .await
        .expect_status(StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        step.json()["error"][0]["non_existent_cookbooks"],
        json!(["missing"])
    );
    steps.push(step);

    // Cookbook artifacts.
    let artifact = json!({
        "name": "parity_book",
        "identifier": ARTIFACT_ID,
        "version": "1.0.0",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "all_files": [],
        "metadata": { "version": "1.0.0", "name": "parity_book" },
    });
    let artifact_path =
        format!("/organizations/{org}/cookbook_artifacts/parity_book/{ARTIFACT_ID}");
    let step = h
        .send_api(
            "create cookbook artifact",
            "PUT",
            &artifact_path,
            &serde_json::to_vec(&artifact).unwrap(),
            "pivotal",
            "2",
        )
        .await
        .expect_status(StatusCode::CREATED);
    steps.push(step);

    let step = h
        .send_api(
            "list cookbook artifacts",
            "GET",
            &format!("/organizations/{org}/cookbook_artifacts"),
            b"",
            "pivotal",
            "2",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json()["parity_book"]["versions"][0]["identifier"],
        ARTIFACT_ID
    );
    steps.push(step);

    let step = h
        .send_api(
            "read cookbook artifact",
            "GET",
            &artifact_path,
            b"",
            "pivotal",
            "2",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["identifier"], ARTIFACT_ID);
    steps.push(step);

    let step = h
        .send_api(
            "delete cookbook artifact",
            "DELETE",
            &artifact_path,
            b"",
            "pivotal",
            "2",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["identifier"], ARTIFACT_ID);
    steps.push(step);

    let step = h
        .send_api(
            "read deleted cookbook artifact",
            "GET",
            &artifact_path,
            b"",
            "pivotal",
            "2",
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    steps.push(step);

    steps
}

impl Step {
    fn body_text(&self) -> &str {
        match &self.body {
            RecordedBody::Text(text) => text,
            other => panic!("step '{}' is not text: {other:?}", self.label),
        }
    }
}

// ---------------------------------------------------------------------------
// Scenario: policies, policy groups and ACLs
// ---------------------------------------------------------------------------

async fn scenario_policies_acls(h: &Harness) -> Vec<Step> {
    let org = h.org();
    let other = h.name("other");
    let reader = h.name("reader");
    let mut steps = vec![create_org(h, &org).await, create_org(h, &other).await];

    steps.push(create_user(h, &reader).await);
    steps.push(add_user_to_org(h, &org, &reader).await);

    steps.push(create_node(h, &org, "web1", "_default").await);

    let step = h
        .send(
            "create acl environment",
            "POST",
            &format!("/organizations/{org}/environments"),
            br#"{"name":"acltest","description":"acl","cookbook_versions":{}}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    steps.push(step);

    let step = h
        .send(
            "create acl client",
            "POST",
            &format!("/organizations/{org}/clients"),
            &serde_json::to_vec(&json!({
                "name": "worker",
                "validator": false,
                "public_key": support::PUBLIC_KEY,
            }))
            .unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    steps.push(step);

    let step = put_cookbook(
        h,
        &org,
        "aclbook",
        "1.0.0",
        cookbook_manifest("aclbook", "1.0.0", json!({}), json!([])),
    )
    .await
    .expect_status(StatusCode::CREATED);
    steps.push(step);

    let revision = br#"{"name":"app","revision_id":"rev1","run_list":["recipe[app::default]"],"cookbook_locks":{"app":{"identifier":"abc123","version":"1.0.0"}}}"#;
    let step = h
        .send(
            "create policy revision",
            "POST",
            &format!("/organizations/{org}/policies/app/revisions"),
            revision,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.json(),
        &serde_json::from_slice::<Value>(revision).unwrap()
    );
    steps.push(step);

    let step = h
        .send(
            "duplicate policy revision",
            "POST",
            &format!("/organizations/{org}/policies/app/revisions"),
            revision,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CONFLICT);
    steps.push(step);

    let step = h
        .send(
            "list policies",
            "GET",
            &format!("/organizations/{org}/policies"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json()["app"]["uri"],
        format!("{BASE_URL}/organizations/{org}/policies/app")
    );
    assert_eq!(step.json()["app"]["revisions"]["rev1"], json!({}));
    steps.push(step);

    let step = h
        .send(
            "read policy",
            "GET",
            &format!("/organizations/{org}/policies/app"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["revisions"]["rev1"], json!({}));
    steps.push(step);

    let step = h
        .send(
            "read policy revision",
            "GET",
            &format!("/organizations/{org}/policies/app/revisions/rev1"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["policy_group_list"], json!([]));
    steps.push(step);

    let step = h
        .send(
            "create policy group",
            "POST",
            &format!("/organizations/{org}/policy_groups"),
            br#"{"name":"prod"}"#,
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(
        step.json()["uri"],
        format!("{BASE_URL}/organizations/{org}/policy_groups/prod")
    );
    steps.push(step);

    let step = h
        .send(
            "associate policy group",
            "PUT",
            &format!("/organizations/{org}/policy_groups/prod/policies/app"),
            revision,
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json(),
        &serde_json::from_slice::<Value>(revision).unwrap()
    );
    steps.push(step);

    let step = h
        .send(
            "list policy groups",
            "GET",
            &format!("/organizations/{org}/policy_groups"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json()["prod"]["policies"]["app"]["revision_id"],
        "rev1"
    );
    steps.push(step);

    let step = h
        .send(
            "read policy group",
            "GET",
            &format!("/organizations/{org}/policy_groups/prod"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["policies"]["app"]["revision_id"], "rev1");
    steps.push(step);

    let step = h
        .send(
            "read policy revision with group",
            "GET",
            &format!("/organizations/{org}/policies/app/revisions/rev1"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["policy_group_list"], json!(["prod"]));
    steps.push(step);

    // One ACL target per object type. The reader must gain read access.
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
            format!("/organizations/{org}/clients/worker/_acl"),
            format!("/organizations/{org}/clients/worker"),
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
        let body = json!({ "actors": [reader], "groups": [] });
        let step = h
            .send(
                "write acl",
                "PUT",
                &format!("{acl_path}/read"),
                &serde_json::to_vec(&body).unwrap(),
                "pivotal",
            )
            .await
            .expect_ok();
        assert_eq!(step.json(), &json!({}));
        steps.push(step);

        let step = h
            .send("read acl", "GET", acl_path, b"", "pivotal")
            .await
            .expect_ok();
        assert!(
            step.json()["read"]["actors"]
                .as_array()
                .unwrap()
                .contains(&json!(reader)),
            "{} does not contain reader: {}",
            acl_path,
            step.json()
        );
        steps.push(step);

        let step = h
            .send(
                "read acl-protected object",
                "GET",
                object_path,
                b"",
                &reader,
            )
            .await
            .expect_ok();
        steps.push(step);
    }

    let step = h
        .send(
            "read granular acl",
            "GET",
            &format!("/organizations/{org}/nodes/web1/_acl?detail=granular"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["read"]["actors"], json!([]));
    assert!(step.json()["read"]["users"]
        .as_array()
        .unwrap()
        .contains(&json!(reader)));
    steps.push(step);

    // Non-local group ACEs survive the round trip through either backend.
    let body = json!({
        "actors": [],
        "groups": ["::server-admins", format!("{other}::admins")],
    });
    let step = h
        .send(
            "write group acl",
            "PUT",
            &format!("/organizations/{org}/nodes/web1/_acl/read"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_ok();
    steps.push(step);

    let step = h
        .send(
            "read group acl",
            "GET",
            &format!("/organizations/{org}/nodes/web1/_acl"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    let groups = step.json()["read"]["groups"].as_array().unwrap();
    assert!(groups.contains(&json!("::server-admins")));
    assert!(groups.contains(&json!(format!("{other}::admins"))));
    steps.push(step);

    // Policy group teardown.
    let step = h
        .send(
            "remove policy group association",
            "DELETE",
            &format!("/organizations/{org}/policy_groups/prod/policies/app"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json(),
        &serde_json::from_slice::<Value>(revision).unwrap()
    );
    steps.push(step);

    let step = h
        .send(
            "delete policy group",
            "DELETE",
            &format!("/organizations/{org}/policy_groups/prod"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(
        step.json(),
        &json!({
            "uri": format!("{BASE_URL}/organizations/{org}/policy_groups/prod"),
            "policies": {}
        })
    );
    steps.push(step);

    let step = h
        .send(
            "read deleted policy group",
            "GET",
            &format!("/organizations/{org}/policy_groups/prod"),
            b"",
            "pivotal",
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    steps.push(step);

    let step = h
        .send(
            "delete policy",
            "DELETE",
            &format!("/organizations/{org}/policies/app"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({ "revisions": { "rev1": {} } }));
    steps.push(step);

    let step = h
        .send(
            "read deleted policy",
            "GET",
            &format!("/organizations/{org}/policies/app"),
            b"",
            "pivotal",
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    steps.push(step);

    steps
}

// ---------------------------------------------------------------------------
// Scenario: membership and invitations
// ---------------------------------------------------------------------------

async fn scenario_membership(h: &Harness) -> Vec<Step> {
    let org = h.org();
    let bob = h.name("bob");
    let mut steps = vec![create_org(h, &org).await];

    steps.push(create_user(h, &bob).await);

    let body = json!({ "user": bob });
    let step = h
        .send(
            "create invitation",
            "POST",
            &format!("/organizations/{org}/association_requests"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(step.json()["org"], org);
    assert_eq!(step.json()["user"], bob);
    let invitation_id = step.json()["id"].as_str().unwrap().to_string();
    steps.push(step);

    let step = h
        .send(
            "list organization invitations",
            "GET",
            &format!("/organizations/{org}/association_requests"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json().as_array().unwrap().len(), 1);
    steps.push(step);

    let step = h
        .send(
            "list user invitations",
            "GET",
            &format!("/users/{bob}/association_requests"),
            b"",
            &bob,
        )
        .await
        .expect_ok();
    assert_eq!(step.json().as_array().unwrap().len(), 1);
    steps.push(step);

    let step = h
        .send(
            "count user invitations",
            "GET",
            &format!("/users/{bob}/association_requests/count"),
            b"",
            &bob,
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({ "value": 1 }));
    steps.push(step);

    let step = h
        .send(
            "accept invitation",
            "PUT",
            &format!("/users/{bob}/association_requests/{invitation_id}"),
            br#"{"response":"accept"}"#,
            &bob,
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["org"], org);
    steps.push(step);

    let step = h
        .send(
            "count accepted invitations",
            "GET",
            &format!("/users/{bob}/association_requests/count"),
            b"",
            &bob,
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({ "value": 0 }));
    steps.push(step);

    let step = h
        .send(
            "list organization members",
            "GET",
            &format!("/organizations/{org}/users"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert!(step.json()["pivotal"].is_string());
    assert!(step.json()[&bob].is_string());
    steps.push(step);

    let step = h
        .send(
            "read member",
            "GET",
            &format!("/organizations/{org}/users/{bob}"),
            b"",
            &bob,
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["username"], bob);
    steps.push(step);

    let step = h
        .send(
            "list member organizations",
            "GET",
            &format!("/users/{bob}/organizations"),
            b"",
            &bob,
        )
        .await
        .expect_ok();
    assert_eq!(step.json()[&org], format!("{BASE_URL}/organizations/{org}"));
    steps.push(step);

    let step = h
        .send(
            "remove member",
            "DELETE",
            &format!("/organizations/{org}/users/{bob}"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({}));
    steps.push(step);

    let step = h
        .send(
            "list organizations after removal",
            "GET",
            &format!("/users/{bob}/organizations"),
            b"",
            &bob,
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({}));
    steps.push(step);

    // Re-invite, then delete the invitation instead of accepting it.
    let step = h
        .send(
            "create second invitation",
            "POST",
            &format!("/organizations/{org}/association_requests"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    let second_id = step.json()["id"].as_str().unwrap().to_string();
    steps.push(step);

    let step = h
        .send(
            "delete invitation",
            "DELETE",
            &format!("/organizations/{org}/association_requests/{second_id}"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({}));
    steps.push(step);

    let step = h
        .send(
            "count invitations after delete",
            "GET",
            &format!("/users/{bob}/association_requests/count"),
            b"",
            &bob,
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({ "value": 0 }));
    steps.push(step);

    // Direct membership add, duplicate conflict and removal.
    let body = json!({ "username": bob });
    let step = h
        .send(
            "add member directly",
            "POST",
            &format!("/organizations/{org}/users"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CREATED);
    steps.push(step);

    let step = h
        .send(
            "add duplicate member",
            "POST",
            &format!("/organizations/{org}/users"),
            &serde_json::to_vec(&body).unwrap(),
            "pivotal",
        )
        .await
        .expect_status(StatusCode::CONFLICT);
    assert_eq!(
        step.json(),
        &json!({"error": "The association already exists."})
    );
    steps.push(step);

    let step = h
        .send(
            "remove direct member",
            "DELETE",
            &format!("/organizations/{org}/users/{bob}"),
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    steps.push(step);

    steps
}

// ---------------------------------------------------------------------------
// Scenario: OAuth identity surface
// ---------------------------------------------------------------------------

async fn scenario_oauth(h: &Harness) -> Vec<Step> {
    let mut steps = Vec::new();
    let redirect = "https://parity.example/callback";

    let step = h
        .send_anon("oauth status", "GET", "/id/v1/status", b"", "", &[])
        .await
        .expect_ok();
    assert_eq!(
        step.json(),
        &json!({
            "status": "ok",
            "erchef": { "status": "reachable" },
            "postgres": { "status": "reachable" }
        })
    );
    steps.push(step);

    let step = h
        .send_anon("oauth me unauthenticated", "GET", "/id/v1/me", b"", "", &[])
        .await
        .expect_status(StatusCode::UNAUTHORIZED);
    steps.push(step);

    let step = h
        .send_anon(
            "oauth applications unauthenticated",
            "GET",
            "/id/oauth/applications",
            b"",
            "",
            &[],
        )
        .await
        .expect_status(StatusCode::UNAUTHORIZED);
    steps.push(step);

    let step = h
        .send_anon(
            "oauth authorize unknown client",
            "GET",
            "/id/oauth/authorize?client_id=missing&redirect_uri=https%3A%2F%2Fparity.example%2Fcallback&response_type=code",
            b"",
            "",
            &[],
        )
        .await
        .expect_status(StatusCode::UNAUTHORIZED);
    assert_eq!(step.json()["error"], "invalid_client");
    steps.push(step);

    let step = h
        .send_anon(
            "oauth token missing credentials",
            "POST",
            "/id/oauth/token",
            b"grant_type=authorization_code",
            "application/x-www-form-urlencoded",
            &[],
        )
        .await
        .expect_status(StatusCode::UNAUTHORIZED);
    steps.push(step);

    // Seed one application and an admin bearer token through the store; the
    // requests below still exercise the real OAuth HTTP surface.
    let application = h
        .store
        .create_oauth_application(&format!("{}-bootstrap", h.run), redirect, "read", true)
        .await
        .expect("create bootstrap oauth application");
    let pivotal = h
        .store
        .find_user("pivotal")
        .await
        .expect("find pivotal")
        .expect("pivotal exists");
    let admin_token = h
        .store
        .create_oauth_access_token(&pivotal.id, application.id, "read", Some(3600), false)
        .await
        .expect("create admin token");
    let bearer = format!("Bearer {}", admin_token.token);

    let step = h
        .send_anon(
            "oauth token unknown client",
            "POST",
            "/id/oauth/token",
            b"grant_type=authorization_code&client_id=missing&client_secret=nope",
            "application/x-www-form-urlencoded",
            &[],
        )
        .await
        .expect_status(StatusCode::UNAUTHORIZED);
    assert_eq!(step.json()["error"], "invalid_client");
    steps.push(step);

    let step = h
        .send_anon(
            "oauth token bad secret",
            "POST",
            "/id/oauth/token",
            format!(
                "grant_type=authorization_code&client_id={}&client_secret=wrong",
                application.uid
            )
            .as_bytes(),
            "application/x-www-form-urlencoded",
            &[],
        )
        .await
        .expect_status(StatusCode::UNAUTHORIZED);
    assert_eq!(step.json()["error"], "invalid_client");
    steps.push(step);

    let step = h
        .send_anon(
            "oauth token unsupported grant",
            "POST",
            "/id/oauth/token",
            format!(
                "grant_type=password&client_id={}&client_secret={}",
                application.uid, application.secret
            )
            .as_bytes(),
            "application/x-www-form-urlencoded",
            &[],
        )
        .await
        .expect_status(StatusCode::BAD_REQUEST);
    assert_eq!(step.json()["error"], "unsupported_grant_type");
    steps.push(step);

    // Application CRUD through the admin bearer token.
    let body = json!({
        "name": format!("{}-supermarket", h.run),
        "redirect_uri": "https://supermarket.example/cb",
        "scopes": "read",
    });
    let step = h
        .send_anon(
            "create oauth application",
            "POST",
            "/id/oauth/applications",
            &serde_json::to_vec(&body).unwrap(),
            "application/json",
            &[
                (header::AUTHORIZATION.as_str(), bearer.clone()),
                (header::ACCEPT.as_str(), "application/json".into()),
            ],
        )
        .await
        .expect_status(StatusCode::CREATED);
    assert_eq!(step.json()["name"], body["name"]);
    assert_eq!(step.json()["confidential"], true);
    let application_id = step.json()["id"].as_i64().unwrap();
    steps.push(step);

    let step = h
        .send_anon(
            "list oauth applications",
            "GET",
            "/id/oauth/applications",
            b"",
            "",
            &[
                (header::AUTHORIZATION.as_str(), bearer.clone()),
                (header::ACCEPT.as_str(), "application/json".into()),
            ],
        )
        .await
        .expect_ok();
    let filtered = filter_oauth_applications(step.json(), &h.run);
    assert_eq!(filtered.as_array().unwrap().len(), 2);
    steps.push(step.map_json(|_| filtered));

    let step = h
        .send_anon(
            "read oauth application",
            "GET",
            &format!("/id/oauth/applications/{application_id}"),
            b"",
            "",
            &[
                (header::AUTHORIZATION.as_str(), bearer.clone()),
                (header::ACCEPT.as_str(), "application/json".into()),
            ],
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["id"], application_id);
    steps.push(step);

    let step = h
        .send_anon(
            "update oauth application",
            "PUT",
            &format!("/id/oauth/applications/{application_id}"),
            &serde_json::to_vec(&json!({ "name": format!("{}-renamed", h.run) })).unwrap(),
            "application/json",
            &[
                (header::AUTHORIZATION.as_str(), bearer.clone()),
                (header::ACCEPT.as_str(), "application/json".into()),
            ],
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["name"], format!("{}-renamed", h.run));
    steps.push(step);

    let step = h
        .send_anon(
            "delete oauth application",
            "DELETE",
            &format!("/id/oauth/applications/{application_id}"),
            b"",
            "",
            &[
                (header::AUTHORIZATION.as_str(), bearer.clone()),
                (header::ACCEPT.as_str(), "application/json".into()),
            ],
        )
        .await
        .expect_ok();
    assert_eq!(step.json(), &json!({}));
    steps.push(step);

    let step = h
        .send_anon(
            "read deleted oauth application",
            "GET",
            &format!("/id/oauth/applications/{application_id}"),
            b"",
            "",
            &[
                (header::AUTHORIZATION.as_str(), bearer.clone()),
                (header::ACCEPT.as_str(), "application/json".into()),
            ],
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    steps.push(step);

    // Full authorization-code exchange and refresh through the token route.
    let grant = h
        .store
        .create_oauth_access_grant(&pivotal.id, application.id, redirect, "read", 300)
        .await
        .expect("create oauth grant");
    let step = h
        .send_anon(
            "oauth exchange authorization code",
            "POST",
            "/id/oauth/token",
            format!(
                "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&client_secret={}",
                grant.token, redirect, application.uid, application.secret
            )
            .as_bytes(),
            "application/x-www-form-urlencoded",
            &[],
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["token_type"], "bearer");
    assert_eq!(step.json()["scope"], "read");
    let refresh_token = step.json()["refresh_token"].as_str().unwrap().to_string();
    steps.push(step);

    let step = h
        .send_anon(
            "oauth refresh access token",
            "POST",
            "/id/oauth/token",
            format!(
                "grant_type=refresh_token&refresh_token={refresh_token}&client_id={}&client_secret={}",
                application.uid, application.secret
            )
            .as_bytes(),
            "application/x-www-form-urlencoded",
            &[],
        )
        .await
        .expect_ok();
    assert_eq!(step.json()["token_type"], "bearer");
    steps.push(step);

    let step = h
        .send_anon(
            "oauth revoke refresh token",
            "POST",
            "/id/oauth/revoke",
            format!(
                "token={refresh_token}&client_id={}&client_secret={}",
                application.uid, application.secret
            )
            .as_bytes(),
            "application/x-www-form-urlencoded",
            &[],
        )
        .await
        .expect_ok();
    steps.push(step);

    let step = h
        .send_anon(
            "oauth reuse revoked refresh token",
            "POST",
            "/id/oauth/token",
            format!(
                "grant_type=refresh_token&refresh_token={refresh_token}&client_id={}&client_secret={}",
                application.uid, application.secret
            )
            .as_bytes(),
            "application/x-www-form-urlencoded",
            &[],
        )
        .await
        .expect_status(StatusCode::BAD_REQUEST);
    assert_eq!(step.json()["error"], "invalid_grant");
    steps.push(step);

    steps
}

fn filter_oauth_applications(value: &Value, prefix: &str) -> Value {
    let mut applications: Vec<Value> = value
        .as_array()
        .expect("oauth application list")
        .iter()
        .filter(|application| {
            application["name"]
                .as_str()
                .is_some_and(|name| name.starts_with(prefix))
        })
        .cloned()
        .collect();
    applications.sort_by_key(|application| application["name"].as_str().unwrap_or("").to_string());
    Value::Array(applications)
}

// ---------------------------------------------------------------------------
// Scenario: license, status and stats
// ---------------------------------------------------------------------------

async fn scenario_ops(h: &Harness) -> Vec<Step> {
    let org = h.org();
    let mut steps = Vec::new();

    let before_license = h
        .send("license baseline", "GET", "/license", b"", "pivotal")
        .await
        .expect_ok();
    let before_license_nodes = before_license.json()["node_count"].as_u64().unwrap();

    let before_stats = h
        .send(
            "stats baseline",
            "GET",
            "/_stats?format=json",
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    let before_stats_nodes = before_stats.json()["nodes"].as_u64().unwrap();
    assert!(before_stats.json()["uptime_seconds"].is_u64());

    steps.push(create_org(h, &org).await);
    for node in ["web1", "web2", "web3"] {
        steps.push(create_node(h, &org, node, "_default").await);
    }

    let step = h
        .send("license after nodes", "GET", "/license", b"", "pivotal")
        .await
        .expect_ok();
    let after_license_nodes = step.json()["node_count"].as_u64().unwrap();
    assert_eq!(after_license_nodes, before_license_nodes + 3);
    let node_license = step.json()["node_license"].as_u64().unwrap();
    let limit_exceeded = after_license_nodes > node_license;
    assert_eq!(step.json()["limit_exceeded"], limit_exceeded);
    let after_license_nodes_u64 = after_license_nodes;
    steps.push(step.map_json(|value| {
        json!({
            "limit_exceeded": limit_exceeded,
            "node_license": value["node_license"],
            "node_count_delta": after_license_nodes_u64 - before_license_nodes,
            "upgrade_url": value["upgrade_url"],
        })
    }));

    let step = h
        .send(
            "stats after nodes",
            "GET",
            "/_stats?format=json",
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    let after_stats_nodes = step.json()["nodes"].as_u64().unwrap();
    assert_eq!(after_stats_nodes, before_stats_nodes + 3);
    assert!(step.json()["uptime_seconds"].is_u64());
    assert!(step.json()["requests"]["total"].is_u64());
    steps.push(step.map_json(|value| {
        json!({
            "uptime_seconds": "<seconds>",
            "requests": {
                "total": "<count>",
                "responses": {
                    "2xx": "<count>",
                    "4xx": "<count>",
                    "5xx": "<count>",
                },
                "authentication_failures": "<count>",
            },
            "maintenance": value["maintenance"],
            "nodes_delta": after_stats_nodes - before_stats_nodes,
        })
    }));

    let step = h
        .send(
            "stats prometheus",
            "GET",
            "/_stats?format=prometheus",
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    assert!(step.body_text().contains("microwave_server_nodes"));
    steps.push(step);

    let step = h
        .send("server status", "GET", "/_status", b"", "pivotal")
        .await
        .expect_ok();
    assert_eq!(step.json()["status"], "pong");
    assert_eq!(step.json()["indexing"]["mode"], "embedded");
    steps.push(step);

    let step = h
        .send(
            "server api version",
            "GET",
            "/server_api_version",
            b"",
            "pivotal",
        )
        .await
        .expect_ok();
    steps.push(step);

    steps
}

// ---------------------------------------------------------------------------
// Test registration
// ---------------------------------------------------------------------------

/// Scenarios run one at a time so global observability endpoints such as
/// `/license` and `/_stats` see a stable node count while they are compared.
static PARITY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

macro_rules! parity_test {
    ($test:ident, $scenario:ident) => {
        #[tokio::test]
        async fn $test() {
            let _guard = PARITY_LOCK.lock().await;
            let run = unique_run();
            let sqlite = Harness::sqlite(run.clone()).await;
            let sqlite_steps = $scenario(&sqlite).await;

            let Some(postgres) = Harness::postgres(run).await else {
                eprintln!(
                    "SKIP: PostgreSQL half of {}; SQLite scenario passed",
                    stringify!($test)
                );
                sqlite.cleanup().await;
                return;
            };
            let postgres_steps = $scenario(&postgres).await;
            compare_transcripts(stringify!($test), &sqlite_steps, &postgres_steps);

            sqlite.cleanup().await;
            postgres.cleanup().await;
        }
    };
}

parity_test!(identity_users_orgs_clients_keys_match, scenario_identity);
parity_test!(
    objects_nodes_roles_environments_data_bags_search_match,
    scenario_objects
);
parity_test!(
    content_sandboxes_cookbooks_artifacts_depsolver_match,
    scenario_content
);
parity_test!(policies_policy_groups_acls_match, scenario_policies_acls);
parity_test!(membership_invitations_match, scenario_membership);
parity_test!(oauth_surface_matches, scenario_oauth);
parity_test!(license_status_stats_match, scenario_ops);
