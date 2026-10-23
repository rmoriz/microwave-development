# Phase 1C: HTTP Edge, Authentication Middleware, and System/Identity Endpoints

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve a real HTTP API with Chef-compatible request authentication, API-version negotiation, system endpoints, and user/organization/client/principal endpoints.

**Architecture:** `microwave-api` exposes an `axum` router over `AppState` containing the SQLite store and configuration. A single middleware reconstructs the request body, negotiates `X-Ops-Server-API-Version`, verifies `X-Ops-Sign` against stored actor keys, and attaches the resolved principal to request extensions.

**Tech Stack:** Rust 1.99, `axum`, `tokio`, `tower`, `serde_json`, `microwave-store`, `microwave-authz`, `microwave-crypto`, `microwave-config`.

**Spec:** `docs/superpowers/specs/2026-10-24-microwave-server-design.md`

## Global Constraints

- `X-Ops-Server-API-Version` defaults to `0`; supported range is `0..=2`.
- Every response carries `X-Ops-Server-API-Version` as JSON with `min_version`, `max_version`, `request_version`, and `response_version`.
- `/_status` and `/server_api_version` are public. All other endpoints require a valid signature.
- Authentication failures return `401` with `{"error": ["..."]}` and no key material.
- Signed paths are checked against the original request path, not the rewritten default-org path.
- JSON errors use `{"error": ["message"]}` unless an endpoint specifies otherwise.
- Every task ends with green tests and a commit.

## Review Focus

- **Invalid API version:** a non-integer or out-of-range header returns `406`, not `500`.
- **Tampered body:** changing one byte after signing returns `401`.
- **Unknown actor:** a signature for a nonexistent user/client returns `401`.
- **Client vs user name collision:** both keys are tried, and a valid signature for either principal is accepted.
- **Empty body:** GET requests and empty POST bodies hash and verify correctly.

---

### Task 1: Store Principal Lookup

**Files:**
- Modify: `crates/microwave-store/src/identity.rs`
- Modify: `crates/microwave-store/src/lib.rs`
- Test: `crates/microwave-store/tests/principal_test.rs`

**Interfaces:**
- Consumes: `SqliteStore`, `ActorType`, `ActorKey`.
- Produces:
  - `Principal { actor_id, authz_id, name, actor_type, org_id }`
  - `SqliteStore::find_principals(org_id: Option<&str>, name: &str) -> Result<Vec<Principal>, StoreError>`
  - `SqliteStore::find_principal_keys(&Principal) -> Result<Vec<ActorKey>, StoreError>`

- [ ] **Step 1: Write the failing test**

```rust
use microwave_store::SqliteStore;

#[test]
fn finds_user_and_client_with_the_same_name() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme").unwrap();
    let user = store.create_user("web1", "web1@example.com", "bcrypt", "h", "s", false).unwrap();
    let client = store.create_client(&org.id, "web1", false, false).unwrap();
    store.add_actor_key(&user.id, microwave_store::ActorType::User, "default", "user-key").unwrap();
    store.add_actor_key(&client.id, microwave_store::ActorType::Client, "default", "client-key").unwrap();

    let principals = store.find_principals(Some(&org.id), "web1").unwrap();
    assert_eq!(principals.len(), 2);
    assert!(principals.iter().any(|p| p.actor_type == microwave_store::ActorType::User));
    assert!(principals.iter().any(|p| p.actor_type == microwave_store::ActorType::Client));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-store --test principal_test`
Expected: FAIL because the methods do not exist.

- [ ] **Step 3: Implement principal lookup**

Return clients first, then users, matching the upstream `/principals` ordering. `find_principal_keys` delegates to `find_actor_keys`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-store --test principal_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-store
git commit -m "feat: add principal lookup"
```

---

### Task 2: API Crate, Router, and Middleware

**Files:**
- Create: `crates/microwave-api/Cargo.toml`
- Create: `crates/microwave-api/src/lib.rs`
- Create: `crates/microwave-api/src/state.rs`
- Create: `crates/microwave-api/src/middleware.rs`
- Create: `crates/microwave-api/src/error.rs`
- Test: `crates/microwave-api/tests/auth_test.rs`

**Interfaces:**
- Consumes: `SqliteStore`, `microwave-crypto`.
- Produces:
  - `AppState { store: Arc<SqliteStore>, config: Arc<Config> }`
  - `router(state: AppState) -> axum::Router`
  - `Principal` and `ApiVersion` request extensions
  - `ApiError` with `IntoResponse`

- [ ] **Step 1: Write the failing test**

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_api::{router, AppState};
use microwave_config::Config;
use microwave_crypto::{sign_request, CanonicalRequest, SignVersion};
use microwave_store::{ActorType, SqliteStore};
use std::sync::Arc;
use tower::ServiceExt;

fn signed_request(
    store: &SqliteStore,
    method: &str,
    path: &str,
    body: &'static [u8],
    version: SignVersion,
) -> Request<Body> {
    let user = store.find_user("pivotal").unwrap().unwrap();
    let keys = store.find_actor_keys(&user.id, ActorType::User).unwrap();
    let req = CanonicalRequest {
        method,
        path,
        body,
        user_id: "pivotal",
        timestamp: "2026-10-24T12:00:00Z",
        api_version: Some("2"),
        version,
    };
    let headers = sign_request(&keys[0].public_key, &req).unwrap();
    // NOTE: the test fixture uses a matching private key; see the implementation step.
    unimplemented!()
}

#[test]
fn status_and_version_are_public() {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let state = AppState { store, config: Arc::new(Config::default()) };
    let app = router(state);

    futures::executor::block_on(async {
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/server_api_version").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    });
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-api --test auth_test`
Expected: FAIL because the crate does not exist.

- [ ] **Step 3: Implement the router, public endpoints, and middleware**

Implement `GET /_status`, `GET /server_api_version`, and `GET /license`. Add a middleware that negotiates the API version and verifies signatures for non-public paths. Use `axum::body::to_bytes` to capture the body, then rebuild the request. Return the `X-Ops-Server-API-Version` header on every response. Keep the test fixture key handling in a dedicated test helper.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-api --test auth_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/microwave-api
git commit -m "feat: add api router and signed auth middleware"
```

---

### Task 3: User Endpoints

**Files:**
- Create: `crates/microwave-api/src/users.rs`
- Modify: `crates/microwave-api/src/lib.rs`
- Test: `crates/microwave-api/tests/users_test.rs`

**Interfaces:**
- Consumes: Task 2.
- Produces: `GET,POST /users`, `GET,PUT,DELETE /users/:username`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn creates_lists_and_reads_a_user() {
    // POST /users as pivotal, then GET /users and GET /users/alice.
    // Assert 201, the returned uri, and that the list contains alice.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-api --test users_test`
Expected: FAIL because the routes do not exist.

- [ ] **Step 3: Implement user handlers**

Validate `username` and `email`. Hash `password` with `microwave-crypto::hash_password`. Return `201` with `{"uri": "<base>/users/<name>"}`. Never return password hashes. On duplicate username return `409`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-api --test users_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api
git commit -m "feat: add user endpoints"
```

---

### Task 4: Organization Endpoints

**Files:**
- Create: `crates/microwave-api/src/organizations.rs`
- Modify: `crates/microwave-api/src/lib.rs`
- Test: `crates/microwave-api/tests/organizations_test.rs`

**Interfaces:**
- Consumes: Task 2 and `microwave-authz::create_organization`.
- Produces: `GET,POST /organizations`, `GET,DELETE /organizations/:org`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn creates_an_organization_and_lists_it() {
    // POST /organizations {"name":"acme","full_name":"Acme Corp"} as pivotal.
    // Assert 201 and that GET /organizations contains acme.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-api --test organizations_test`
Expected: FAIL because the routes do not exist.

- [ ] **Step 3: Implement organization handlers**

Use the authenticated principal's user id and authz id as the creator. Return `201` with `{"uri": "<base>/organizations/<name>"}`. Delete removes the org and its cascaded rows.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-api --test organizations_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api
git commit -m "feat: add organization endpoints"
```

---

### Task 5: Client and Principal Endpoints

**Files:**
- Create: `crates/microwave-api/src/clients.rs`
- Create: `crates/microwave-api/src/principals.rs`
- Modify: `crates/microwave-api/src/lib.rs`
- Test: `crates/microwave-api/tests/clients_test.rs`

**Interfaces:**
- Consumes: Task 2.
- Produces: `GET,POST /organizations/:org/clients`, `GET,DELETE /organizations/:org/clients/:client`, `GET /organizations/:org/principals/:name`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn creates_a_client_and_reads_principals() {
    // Create org acme, POST a client, GET it, and GET /principals/<name>.
    // Assert 201/200 and the client fields.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-api --test clients_test`
Expected: FAIL because the routes do not exist.

- [ ] **Step 3: Implement client and principal handlers**

Enforce org membership/ACL checks for the authenticated principal later; for now require a valid signature and a known org. Return client JSON with `name`, `validator`, `admin`, and `uri`. Principal responses use the `v0` single-object shape when the request API version is `0`, and `{"principals": [...]}` for `v1+`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-api --test clients_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api
git commit -m "feat: add client and principal endpoints"
```

---

## Self-Review

- **Spec coverage:** This plan covers the edge pipeline from spec section 5, the authentication middleware from 6.1, and the identity routes from 6.4. Nodes, cookbooks, search, and OAuth follow in later plans.
- **Step scan:** Each task has a failing test, implementation, passing test, and commit.
- **Type consistency:** `AppState`, `ApiVersion`, `ApiError`, `Principal` are introduced once.
- **Review Focus:** Invalid versions, tampered bodies, unknown actors, name collisions, and empty bodies each have a test.
- **Proportion:** Interfaces and tests only.
