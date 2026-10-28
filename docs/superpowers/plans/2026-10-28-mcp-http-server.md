# Experimental HTTP MCP Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve a per-organization, bearer-authenticated Streamable HTTP MCP endpoint at `/organizations/{organization}/mcp` with ACL-aware read tools for nodes, environments, cookbooks and run reports, plus write tools for nodes, environments and cookbook versions.

**Architecture:** Mount `rmcp` 3.8's `StreamableHttpService` inside the existing axum router. A per-request handler authenticates the org bearer token, resolves the configured Chef principal, builds an `McpContext`, and runs the rmcp service statelessly with `with_legacy_session_mode(false)` and `with_json_response(true)`. Read tools query the existing metadata store; write tools call shared `pub(crate)` mutation services extracted from the HTTP handlers so ACL checks, normalization, indexing and blob handling stay in one place.

**Tech Stack:** Rust, axum 0.8, `rmcp` 3.8 (`transport-streamable-http-server`), `schemars` 1, serde_json, chrono, rusqlite, sqlx/PostgreSQL.

**Spec:** `docs/superpowers/specs/2026-10-27-mcp-run-reports-design.md`

## Global Constraints

- The endpoint is mounted only when `mcp.enabled = true`; an org without an `[mcp.organizations.*]` entry answers 404.
- Every MCP request authenticates with `Authorization: Bearer <token>`; the token is bound to one organization and compared in constant time.
- A missing token configuration is 401, never open access. A missing principal user or missing org membership for a non-superuser is 500 plus a log line.
- The signed-API middleware bypasses exactly the `/organizations/{org}/mcp` path shape; no other route loses authentication.
- `writes = false` removes every write tool from `tools/list`; calling one then fails as an unknown tool.
- Every list tool clamps `limit` to `mcp.max_results`; a serialized result larger than `mcp.max_result_bytes` is an `isError` result, never truncated.
- `since` must parse as RFC 3339, otherwise the tool returns an invalid-argument error.
- Uploads accept at most 100 files and `mcp.max_upload_bytes` decoded bytes in total.
- Reports whose node no longer exists are visible to superusers only.
- Writes use the existing ACL, normalization, validation, indexing and blob paths; do not duplicate them in MCP.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets`, and `cargo test --workspace` must pass.

## Review Focus

These failure modes are most likely to bite an operator or an agent; each gets a test in the task that owns the code.

1. A token for organization A used on organization B's path must return 401, not beta data.
2. A non-superuser must not see a run report for a node it cannot read, nor a report whose node was deleted.
3. `writes = false` must hide the write tools and reject a direct call to one.
4. A retried cookbook upload with `frozen: true` must fail unless `force: true`, exactly like the HTTP API.
5. An oversized tool result must return `isError: true`, not a silently truncated JSON document.

---

### Task 1: MCP configuration and environment overrides

**Files:**
- Modify: `crates/microwave-config/src/config.rs`
- Modify: `crates/microwave-config/src/lib.rs`
- Test: `crates/microwave-config/tests/config_test.rs`

**Interfaces:**
- Produces: `pub struct McpConfig { enabled: bool, max_results: usize, max_result_bytes: usize, max_upload_bytes: usize, allowed_hosts: Vec<String>, allowed_origins: Vec<String>, organizations: std::collections::HashMap<String, McpOrgConfig> }`.
- Produces: `pub struct McpOrgConfig { token: Option<String>, principal: Option<String>, writes: bool }` with `writes` defaulting to `true`.
- Produces: `McpConfig::clamp_bounds(&mut self)`, `McpConfig::effective_allowed_hosts(&self, public_url: Option<&str>) -> Vec<String>`.
- Produces: `Config { pub mcp: McpConfig, .. }`.
- Consumes: nothing from earlier tasks.

- [ ] **Step 1: Write the failing config tests**

Add to `crates/microwave-config/tests/config_test.rs`:

```rust
use microwave_config::{McpConfig, McpOrgConfig};

#[test]
fn mcp_defaults_are_disabled_with_bounded_limits() {
    let c = Config::default();
    assert!(!c.mcp.enabled);
    assert_eq!(c.mcp.max_results, 100);
    assert_eq!(c.mcp.max_result_bytes, 1_048_576);
    assert_eq!(c.mcp.max_upload_bytes, 10_485_760);
    assert!(c.mcp.allowed_hosts.is_empty());
    assert!(c.mcp.allowed_origins.is_empty());
    assert!(c.mcp.organizations.is_empty());
}

#[test]
fn mcp_parses_per_org_tokens_principals_and_writes() {
    let c = Config::from_toml_str(
        r#"
        [mcp]
        enabled = true
        [mcp.organizations.acme]
        token = "a-token"
        principal = "mcp-agent"
        writes = false
        [mcp.organizations.beta]
    "#,
    )
    .unwrap();
    assert!(c.mcp.enabled);
    let acme = c.mcp.organizations.get("acme").unwrap();
    assert_eq!(acme.token.as_deref(), Some("a-token"));
    assert_eq!(acme.principal.as_deref(), Some("mcp-agent"));
    assert!(!acme.writes);
    assert!(c.mcp.organizations.get("beta").unwrap().writes);
}

#[test]
fn mcp_clamps_limits() {
    let c = Config::from_toml_str(
        r#"
        [mcp]
        max_results = 5000
        max_result_bytes = 1
        max_upload_bytes = 999999999999
    "#,
    )
    .unwrap();
    assert_eq!(c.mcp.max_results, 1000);
    assert_eq!(c.mcp.max_result_bytes, 1024);
    assert_eq!(c.mcp.max_upload_bytes, 250 * 1024 * 1024);
}

#[test]
fn mcp_allowed_hosts_fall_back_to_the_public_url() {
    let c = Config::from_toml_str(
        r#"
        [server]
        public_url = "https://chef.example.com"
        [mcp]
        enabled = true
    "#,
    )
    .unwrap();
    assert_eq!(
        c.mcp.effective_allowed_hosts(c.server.public_url.as_deref()),
        vec!["chef.example.com".to_string()]
    );

    let c = Config::from_toml_str(
        r#"
        [server]
        public_url = "https://chef.example.com:8443"
    "#,
    )
    .unwrap();
    assert_eq!(
        c.mcp.effective_allowed_hosts(c.server.public_url.as_deref()),
        vec![
            "chef.example.com".to_string(),
            "chef.example.com:8443".to_string()
        ]
    );
}

#[test]
fn mcp_scalar_settings_can_come_from_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_MCP__ENABLED", "true");
    std::env::set_var("MW_MCP__MAX_RESULTS", "7");
    std::env::set_var("MW_MCP__MAX_RESULT_BYTES", "2048");
    std::env::set_var("MW_MCP__MAX_UPLOAD_BYTES", "4096");
    std::env::set_var("MW_MCP__ALLOWED_HOSTS", "chef.example.com, localhost");
    std::env::set_var("MW_MCP__ALLOWED_ORIGINS", "https://app.example.com");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert!(c.mcp.enabled);
    assert_eq!(c.mcp.max_results, 7);
    assert_eq!(c.mcp.max_result_bytes, 2048);
    assert_eq!(c.mcp.max_upload_bytes, 4096);
    assert_eq!(
        c.mcp.allowed_hosts,
        vec!["chef.example.com".to_string(), "localhost".to_string()]
    );
    assert_eq!(
        c.mcp.allowed_origins,
        vec!["https://app.example.com".to_string()]
    );
    for key in [
        "MW_MCP__ENABLED",
        "MW_MCP__MAX_RESULTS",
        "MW_MCP__MAX_RESULT_BYTES",
        "MW_MCP__MAX_UPLOAD_BYTES",
        "MW_MCP__ALLOWED_HOSTS",
        "MW_MCP__ALLOWED_ORIGINS",
    ] {
        std::env::remove_var(key);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-config --test config_test mcp`
Expected: FAIL because `Config::mcp` and `McpConfig` do not exist.

- [ ] **Step 3: Implement the config types and bounds**

In `config.rs`:

- Add `pub mcp: McpConfig` to `Config`.
- Add the two structs and `Default` implementations: `McpConfig` defaults to `false, 100, 1_048_576, 10_485_760, vec![], vec![], HashMap::new()`; `McpOrgConfig::default()` sets `writes: true`.
- `McpOrgConfig` uses `#[serde(default)]` and `#[serde(default = "default_true")]` on `writes`.
- `clamp_bounds` clamps `max_results` to `1..=1000`, `max_result_bytes` to `1024..=16 * 1024 * 1024`, `max_upload_bytes` to `1..=250 * 1024 * 1024`.
- `effective_allowed_hosts` returns `allowed_hosts` when non-empty. Otherwise parse `public_url` with `url::Url`; push `host_str()` and, when `port()` is `Some`, `host:port`. Return an empty vector when no URL is configured.
- Call `config.mcp.clamp_bounds()` in `Config::from_toml_str` after deserialization and at the end of `Config::apply_env`.
- Parse `MW_MCP__ENABLED`, `MW_MCP__MAX_RESULTS`, `MW_MCP__MAX_RESULT_BYTES`, `MW_MCP__MAX_UPLOAD_BYTES` with the existing helpers; parse `MW_MCP__ALLOWED_HOSTS` and `MW_MCP__ALLOWED_ORIGINS` as comma-separated trimmed lists.
- Re-export `McpConfig` and `McpOrgConfig` from `crates/microwave-config/src/lib.rs`.
- Update the `apply_env` doc comment with the new keys.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-config --test config_test mcp`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-config/src/config.rs crates/microwave-config/src/lib.rs crates/microwave-config/tests/config_test.rs
git commit -m "feat(config): add MCP endpoint configuration"
```

---

### Task 2: Idempotent `ensure_checksums` in both stores

**Files:**
- Modify: `crates/microwave-store/src/backend.rs:536-547`
- Modify: `crates/microwave-store/src/sandboxes.rs`
- Modify: `crates/microwave-store/src/backend_sqlite.rs:800-810`
- Modify: `crates/microwave-store-postgres/src/sandboxes.rs`
- Test: `crates/microwave-store/tests/dual_backend.rs`

**Interfaces:**
- Produces: `SandboxRepository::ensure_checksums(&self, org_id: &str, checksums: &[String]) -> Result<(), StoreError>`.
- Consumes: nothing from earlier tasks.

- [ ] **Step 1: Write the failing dual-backend test**

Add a new suite function at the end of `mod suite` in `crates/microwave-store/tests/dual_backend.rs`, then register `checksums` in **both** `matrix!` lists:

```rust
    pub async fn checksums(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;
        let checksums = vec![checksum(), checksum()];

        store
            .ensure_checksums(&org.id, &checksums)
            .await
            .expect("ensure checksums");
        store
            .ensure_checksums(&org.id, &checksums)
            .await
            .expect("ensure checksums is idempotent");

        let mut expected = checksums.clone();
        expected.sort();
        let mut found = store
            .existing_checksums(&org.id, &checksums)
            .await
            .expect("existing checksums");
        found.sort();
        assert_eq!(found, expected);

        let other = store
            .create_org(&unique("matrix-other-org"), "Other Organization")
            .await
            .expect("create other org");
        assert!(!store
            .checksum_exists(&other.id, &checksums[0])
            .await
            .expect("checksum isolation"));

        cleanup_org(store.as_ref(), &org.id).await;
        cleanup_org(store.as_ref(), &other.id).await;
        Vec::new()
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p microwave-store --test dual_backend checksums`
Expected: FAIL to compile because `ensure_checksums` is not a method.

- [ ] **Step 3: Implement it in the trait and both backends**

Add to `SandboxRepository` in `backend.rs`:

```rust
/// Insert checksum rows that do not exist yet. Repeating the call is a no-op.
async fn ensure_checksums(&self, org_id: &str, checksums: &[String])
    -> Result<(), StoreError>;
```

SQLite (`sandboxes.rs`): for each checksum run `INSERT OR IGNORE INTO checksums (org_id, checksum, created_at) VALUES (?1, ?2, ?3)` with `crate::now()`; return early for an empty slice.

PostgreSQL (`sandboxes.rs`): for each checksum run `INSERT INTO checksums (org_id, checksum, created_at) VALUES ($1, $2, $3) ON CONFLICT (org_id, checksum) DO NOTHING`; return early for an empty slice.

Delegate both from `backend_sqlite.rs` and `backend_postgres` (the Postgres impl is on the trait directly in `crates/microwave-store-postgres/src/sandboxes.rs`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-store --test dual_backend checksums`
Expected: PASS on SQLite and on PostgreSQL when the test database is configured.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-store crates/microwave-store-postgres
git commit -m "feat(store): add idempotent ensure_checksums"
```

---

### Task 3: Allow arbitrary org names in the `mcp` credential group

**Files:**
- Modify: `crates/microwave-config/src/config.rs` (make the existing validator public)
- Modify: `crates/microwave-config/src/lib.rs` (re-export it)
- Modify: `crates/microwave-ctl/src/secrets.rs`
- Test: `crates/microwave-ctl/src/secrets.rs` (`#[cfg(test)]` module)

**Interfaces:**
- Produces: `set-secret mcp <org> <token>` and `show-secret mcp <org>` accept a valid organization name.
- Consumes: nothing from earlier tasks.

- [ ] **Step 1: Write the failing unit test**

Append to `crates/microwave-ctl/src/secrets.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::validate_credential;

    #[test]
    fn mcp_group_accepts_org_names_and_rejects_bad_names() {
        assert!(validate_credential("mcp", "acme").is_ok());
        assert!(validate_credential("mcp", "team-1").is_ok());
        assert!(validate_credential("mcp", "Acme").is_err());
        assert!(validate_credential("mcp", "").is_err());
        assert!(validate_credential("mcp", "with/slash").is_err());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p microwave-ctl mcp_group_accepts`
Expected: FAIL because the `mcp` group is unknown.

- [ ] **Step 3: Implement the group and its validator**

Add `("mcp", &[])` to `KNOWN_CREDENTIALS`. Make the existing `fn valid_org_name` in `crates/microwave-config/src/config.rs` public (`pub fn valid_org_name(name: &str) -> bool`) and re-export it from `crates/microwave-config/src/lib.rs`. In `validate_credential`, before the `names.contains(&name)` check:

```rust
if group == "mcp" {
    return microwave_config::valid_org_name(name).then_some(()).ok_or_else(|| {
        CtlError::message(format!(
            "invalid organization name '{name}' for the mcp credential group"
        ))
    });
}
```

Do not copy the rules into `microwave-ctl`; `microwave-ctl` already depends on `microwave-config`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p microwave-ctl mcp_group_accepts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-config/src/config.rs crates/microwave-config/src/lib.rs crates/microwave-ctl/src/secrets.rs
git commit -m "feat(ctl): allow mcp secrets per organization"
```

---

### Task 4: Mount the authenticated MCP endpoint

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/microwave-api/Cargo.toml`
- Create: `crates/microwave-api/src/mcp/mod.rs`
- Create: `crates/microwave-api/src/mcp/auth.rs`
- Create: `crates/microwave-api/src/mcp/context.rs`
- Create: `crates/microwave-api/src/mcp/tools.rs`
- Modify: `crates/microwave-api/src/lib.rs:51-101`
- Modify: `crates/microwave-api/src/middleware.rs:700-790`
- Test: `crates/microwave-api/tests/mcp_test.rs`

**Interfaces:**
- Produces: `pub struct McpContext { pub state: AppState, pub org: Org, pub principal: Principal, pub is_superuser: bool, pub writes: bool, pub max_results: usize, pub max_result_bytes: usize, pub max_upload_bytes: usize }` with `pub fn auth_context(&self) -> AuthContext` and `pub fn clamped_limit(&self, limit: Option<u32>) -> usize`.
- Produces: `pub async fn authenticate(state: &AppState, organization: &str, headers: &HeaderMap) -> Result<McpContext, axum::response::Response>`.
- Produces: `pub fn routes() -> Router<AppState>`.
- Produces: `crate::mcp::tools::McpServer` implementing `rmcp::ServerHandler` with an empty tool router for now.
- Consumes: `McpConfig`, `McpOrgConfig` from Task 1.

- [ ] **Step 1: Write the failing endpoint tests**

Create `crates/microwave-api/tests/mcp_test.rs` with the shared helpers below and the tests named in this step.

```rust
mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_api::AppState;
use microwave_config::{Config, McpOrgConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

fn mcp_config(orgs: &[(&str, &str)]) -> Config {
    let mut config = Config::default();
    config.mcp.enabled = true;
    for (org, token) in orgs {
        config.mcp.organizations.insert(
            (*org).to_string(),
            McpOrgConfig {
                token: Some((*token).to_string()),
                ..Default::default()
            },
        );
    }
    config
}

async fn mcp(
    app: &axum::Router,
    org: &str,
    token: &str,
    body: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/organizations/{org}/mcp"))
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("host", "localhost")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0" }
        }
    })
}

fn tools_list(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {} })
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
```

Tests:

- `mcp_endpoint_requires_a_token`: initialize without an `Authorization` header → 401 and `WWW-Authenticate: Bearer`.
- `mcp_token_is_bound_to_its_org`: config with `acme`/`a` and `beta`/`b`; POST to `/organizations/beta/mcp` with token `a` → 401.
- `unknown_or_unconfigured_org_is_404`: enabled config with only `acme`; POST to `/organizations/missing/mcp` and `/organizations/beta/mcp` → 404.
- `disabled_endpoint_is_404`: default config; POST to `/organizations/acme/mcp` → 404.
- `initialize_and_tools_list_work`: initialize → 200, JSON body with `result.serverInfo`; then `tools/list` → `result.tools == []`.
- `non_loopback_host_is_rejected_unless_configured`: initialize with `Host: evil.example.com` → 403; repeat with `config.mcp.allowed_hosts = vec!["evil.example.com".into()]` → 200.
- `oversized_request_body_is_413`: POST a 2 MiB JSON body; the derived limit (`max_upload_bytes * 4 / 3 + 1 MiB`) exceeds the body only when `max_upload_bytes` is raised, so set `config.mcp.max_upload_bytes = 1` in this test to keep the limit near 1 MiB and assert 413.

For the org fixtures, call `support::state_with_config(config)`, `support::add_org(&state, "acme")`, and `let app = support::app(state);`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test mcp_test`
Expected: FAIL because the route and module do not exist.

- [ ] **Step 3: Add the dependency and implement the module**

In the workspace `Cargo.toml`, add:

```toml
schemars = "1"
rmcp = { version = "3.8", features = ["transport-streamable-http-server"] }
```

In `crates/microwave-api/Cargo.toml`, add `rmcp.workspace = true`, `schemars.workspace = true`, and `http.workspace = true`.

`context.rs`:

```rust
#[derive(Clone)]
pub struct McpContext { /* fields from Interfaces */ }

impl McpContext {
    pub fn auth_context(&self) -> AuthContext {
        AuthContext {
            principal: self.principal.clone(),
            org: Some(self.org.clone()),
            key_name: None,
            is_superuser: self.is_superuser,
        }
    }

    pub fn clamped_limit(&self, limit: Option<u32>) -> usize {
        limit.map(|value| value as usize).unwrap_or(self.max_results).clamp(1, self.max_results)
    }
}
```

`auth.rs` implements the spec's request flow:

1. `state.store.find_org(organization)` → 404 on `None`.
2. `state.config.mcp.organizations.get(org.name)` → 404 on `None`.
3. Token: `entry.token` trimmed and non-empty, else `state.store.get_secret("mcp", &org.name)`. No token → 401 and `tracing::warn!`.
4. Extract `Authorization: Bearer <token>` case-insensitively; compare with `subtle::ConstantTimeEq`; mismatch → 401.
5. Principal name: `entry.principal` or `state.config.auth.superuser_name`. Build it with `state.store.find_principals(Some(&org.id), &name)` and select the `ActorType::User` entry. Missing → 500 plus `tracing::error!`.
6. `is_superuser = name == state.config.auth.superuser_name`. For a non-superuser require `state.store.is_user_in_org(&org.id, &principal.actor_id)`; failure → 500 plus `tracing::error!`.
7. Return `McpContext` with limits copied from `state.config.mcp`.

Unauthorized responses use `ApiError::unauthorized_with("Failed to authenticate").into_response()` and insert `WWW-Authenticate: Bearer`. Not-found uses `ApiError::not_found("not found")`; internal failures use `ApiError::internal()`.

`tools.rs` for this task:

```rust
#[derive(Clone)]
pub struct McpServer {
    pub(crate) ctx: McpContext,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    pub fn new(ctx: McpContext) -> Self {
        Self { ctx, tool_router: Self::tool_router() }
    }
}

#[tool_router(allow_empty)]
impl McpServer {}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Microwave Server organization tools")
    }
}
```

`mod.rs`:

```rust
pub fn routes() -> Router<AppState> {
    Router::new().route("/organizations/{organization}/mcp", any(handle))
}

async fn handle(
    State(state): State<AppState>,
    Path(organization): Path<String>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response {
    let ctx = match auth::authenticate(&state, &organization, &headers).await {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let config = server_config(&ctx);
    let service: StreamableHttpService<McpServer, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(McpServer::new(ctx.clone())), Default::default(), config);
    let response = service.oneshot(request).await.expect("rmcp service is infallible");
    response.map(Body::new)
}
```

`server_config` starts from `StreamableHttpServerConfig::default()` and applies:

```rust
.with_json_response(true)
.with_legacy_session_mode(false)
.with_sse_keep_alive(None)
.with_max_request_body_bytes(
    state.config.mcp.max_upload_bytes.saturating_mul(4) / 3 + 1024 * 1024,
)
```

Add allowed hosts only when `effective_allowed_hosts(state.config.server.public_url.as_deref())` is non-empty. Add allowed origins only when `mcp.allowed_origins` is non-empty.

Router and middleware wiring:

- In `lib.rs`, add `pub mod mcp;` and, after the data-collector branch, `if state.config.mcp.enabled { inner = inner.merge(mcp::routes()); }`.
- In `middleware.rs`, add `is_mcp_path` (exactly `organizations`, one non-empty org, `mcp`, nothing else) and an early branch that calls `next.run(request)` for that path, mirroring the data-collector bypass. The branch runs whether or not MCP is enabled, so a disabled endpoint returns the router's 404 instead of an authentication error.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --test mcp_test`
Expected: PASS for all six tests.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/microwave-api/Cargo.toml crates/microwave-api/src/lib.rs crates/microwave-api/src/middleware.rs crates/microwave-api/src/mcp crates/microwave-api/tests/mcp_test.rs
git commit -m "feat(mcp): mount authenticated streamable HTTP endpoint"
```

---

### Task 5: Read tools for nodes, environments, and cookbooks

**Files:**
- Modify: `crates/microwave-api/src/mcp/tools.rs`
- Modify: `crates/microwave-api/src/mcp/context.rs`
- Test: `crates/microwave-api/tests/mcp_test.rs`

**Interfaces:**
- Produces tools: `list_nodes`, `get_node`, `list_environments`, `get_environment`, `list_cookbooks`, `list_cookbook_versions`, `get_cookbook_version`.
- Produces: `McpContext::can_read(&self, authz_id: &str) -> Result<bool, ApiError>` and `McpContext::require_read(&self, authz_id: &str) -> Result<(), ApiError>`.
- Consumes: existing `MetadataStore` object/cookbook methods, `crate::helpers::require_permission`, `crate::cookbooks::render_manifest`.

- [ ] **Step 1: Write the failing read-tool tests**

In `mcp_test.rs`, add a fixture that creates a node and an environment through the signed API (reuse `support::signed_request`), then:

- `list_nodes_filters_by_query_and_environment`: create `web1` in `_default` and `db1` in `prod`; `list_nodes {}` returns both; `{"query":"web"}` returns `web1`; `{"environment":"prod"}` returns `db1`.
- `get_node_returns_the_full_object`: `get_node {"name":"web1"}` contains `chef_environment` and `run_list`.
- `list_and_get_environment_work`: `list_environments` contains `_default`; `get_environment {"name":"_default"}` returns the object.
- `list_cookbooks_groups_versions_and_newest`: upload two versions through the existing cookbook HTTP endpoint; `list_cookbooks {}` returns one entry with `versions` and `newest_version == "2.0.0"`; `list_cookbook_versions {"name":"apache"}` returns both versions; `get_cookbook_version {"name":"apache","version":"1.0.0"}` returns the rendered manifest. Before each PUT, insert the checksum row with `state.store.ensure_checksums(&org.id, &[checksum.clone()]).await.unwrap()` and write the blob with `state.object_store.put(&org.id, &checksum, b"contents").unwrap()`, so `validate_checksums` passes without a sandbox round-trip.
- `read_tools_filter_objects_the_principal_cannot_read`: configure a non-superuser principal with no ACLs; every list tool returns an empty array and `get_node` returns `isError: true` with `"forbidden"`.
- `list_tools_clamp_to_max_results`: set `config.mcp.max_results = 2`, create three nodes, and assert `result.nodes.len() == 2`.

Parse tool results with a helper:

```rust
async fn call_tool(app: &axum::Router, org: &str, token: &str, id: u64, name: &str, args: Value) -> Value {
    let response = mcp(app, org, token, json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": name, "arguments": args }
    })).await;
    let body = body_json(response).await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test mcp_test list_nodes -- --nocapture`
Expected: FAIL because the tool is unknown.

- [ ] **Step 3: Implement the tools**

Use one `#[tool_router(router = read_tool_router, vis = "pub(crate)")]` impl block with `Parameters<T>` argument structs deriving `Deserialize` and `schemars::JsonSchema`. Change `McpServer::new` to build `tool_router: Self::read_tool_router()`, and remove the `allow_empty` router from Task 4. Each tool returns `Result<CallToolResult, ErrorData>`; expected failures return `Ok(CallToolResult::error(vec![ContentBlock::text(...)]))`.

Argument structs:

```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListNodesArgs { query: Option<String>, environment: Option<String>, limit: Option<u32> }
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetNodeArgs { name: String }
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListEnvironmentsArgs { limit: Option<u32> }
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetEnvironmentArgs { name: String }
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListCookbooksArgs { query: Option<String>, limit: Option<u32> }
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CookbookNameArgs { name: String }
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CookbookVersionArgs { name: String, version: String }
```

Behavior:

- `list_nodes`: `store.list_nodes(&org.id)`, keep nodes whose `can_read(authz_id)` is true, apply case-insensitive substring `query` on `name` and exact `environment`, take `clamped_limit`, return `{ "nodes": [ {name, environment, policy_name, policy_group, updated_at} ] }` ordered by name.
- `get_node`: `find_node`; missing → `"not found"` error; `require_read`; return the parsed `serialized_object`.
- `list_environments`: `list_environment_names`, then resolve each with `find_environment`, filter by `can_read`, take the limit, return `{ "environments": [ {name, updated_at} ] }`.
- `get_environment`: same not-found and ACL rules as `get_node`.
- `list_cookbooks`: group `list_cookbook_versions` by name, retain only versions the principal can read, apply the substring filter, sort versions descending with the existing `crate::cookbooks::sort_versions_descending` (make it `pub(crate)` if needed), take the limit, return `{ "cookbooks": [ {name, versions: [..], newest_version} ] }`.
- `list_cookbook_versions`: filter by name, keep readable versions, sort descending, return name/frozen/updated_at.
- `get_cookbook_version`: resolve `_latest`/`latest` through the same rule as the HTTP API; missing → `"not found"`; `require_read`; return `crate::cookbooks::render_manifest(&manifest, 2, Some(&state), &org.id)`.

Add `McpContext::can_read` / `require_read`. `can_read` returns `true` for a superuser and otherwise `state.store.check_permission(...)`. `require_read` maps `false` to `ApiError::forbidden_with("missing read permission")`.

Serialize every success with a helper:

```rust
fn json_result<T: serde::Serialize>(&self, value: &T) -> CallToolResult
```

It serializes with `serde_json::to_string`, compares the byte length against `max_result_bytes`, and returns an `isError` result `"result exceeds max_result_bytes"` when the cap is exceeded.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --test mcp_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/mcp crates/microwave-api/tests/mcp_test.rs crates/microwave-api/src/cookbooks.rs
git commit -m "feat(mcp): add node, environment, and cookbook read tools"
```

---

### Task 6: Read tools for runs and converge errors

**Files:**
- Modify: `crates/microwave-api/src/mcp/tools.rs`
- Modify: `crates/microwave-api/src/mcp/context.rs`
- Test: `crates/microwave-api/tests/mcp_test.rs`

**Interfaces:**
- Produces tools: `list_runs`, `latest_run`, `get_run`, `list_converge_errors`.
- Produces: `McpContext::node_readable(&self, node_name: &str) -> Result<bool, ApiError>` returning `true` for superusers, `true` when the node exists and is readable, `false` when the node exists and is not readable, and `self.is_superuser` when the node is missing.
- Consumes: `ReportRepository`, `StoredNodeRun`.

- [ ] **Step 1: Write the failing report-tool tests**

Add a fixture that inserts runs directly through `state.store.store_node_run` with `NewNodeRun`, including one run with a top-level error and one with a resource error. Then:

- `list_runs_filters_and_orders`: three runs for `web1`; `{"node_name":"web1","failures_only":true}` returns only the failing run; `{"since":"2026-10-27T11:30:00Z"}` returns the newest run.
- `latest_run_includes_errors`: `{"node_name":"web1"}` returns the newest run plus `error` and `resource_errors`.
- `get_run_returns_the_full_report`: `{"node_name":"web1","run_id":"run-2"}` contains `resources`.
- `list_converge_errors_returns_top_level_and_resource_errors`: top-level error row has `resource_type == null` and `resource_name == null`; resource error row has the resource type/name, class and message.
- `invalid_since_is_an_invalid_argument_error`: `{"since":"yesterday"}` → `isError: true` with a message containing `"RFC 3339"`.
- `reports_of_deleted_nodes_are_hidden_from_non_superusers`: build a fixture whose `McpOrgConfig.principal` is a non-superuser user created with `support::add_user(&state, "agent", "agent@example.com", false)` and associated with the org via `support::associate`; insert a run, delete the node, then `list_runs {}` returns an empty `runs` array and `list_runs {"node_name":"web1"}` is an `isError` forbidden result.
- `reports_of_deleted_nodes_stay_visible_to_superusers`: the same run and node deletion, but the org entry uses the default superuser principal; `list_runs {"node_name":"web1"}` still returns the run.
- `oversized_get_run_is_an_error_not_truncated`: set `config.mcp.max_result_bytes = 1024`, insert a report with a 2048-byte field, and assert `isError: true`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test mcp_test list_runs`
Expected: FAIL because the tool is unknown.

- [ ] **Step 3: Implement the tools**

Argument structs:

```rust
struct ListRunsArgs { node_name: Option<String>, status: Option<String>, failures_only: Option<bool>, since: Option<String>, limit: Option<u32> }
struct LatestRunArgs { node_name: String }
struct GetRunArgs { node_name: String, run_id: String }
struct ListConvergeErrorsArgs { node_name: Option<String>, since: Option<String>, limit: Option<u32> }
```

- Validate `since` with `chrono::DateTime::parse_from_rfc3339`; failure → `isError` with `"since must be an RFC 3339 timestamp"`.
- Build `NodeRunFilter { node_name, status, since, failures_only: unwrap_or(false), limit: clamped_limit }`.
- `list_runs`: call `store.list_node_runs`, retain rows where `ctx.node_readable(node_name)` is true, return `{runs:[{run_id,node_name,status,start_time,end_time,resource_count,updated_resource_count,error_count}]}`. When `node_name` is given and unreadable, return a forbidden error instead of an empty list.
- `latest_run`: `store.latest_node_run`; missing → `"not found"`; ACL as above; return the summary plus `error` and `resource_errors` parsed from `report`.
- `get_run`: `store.find_node_run`; missing → `"not found"`; ACL; return the parsed `report`.
- `list_converge_errors`: list and filter like `list_runs`, then for each row parse the report. Emit one entry per top-level `error` object (`resource_type: null`, `resource_name: null`) and one per `resources[]` entry with a non-null `error`. Each entry carries `run_id`, `node_name`, `start_time`, `resource_type`, `resource_name`, `error_class`, `error_message`.

Add a helper in `tools.rs`:

```rust
fn converge_errors(run: &StoredNodeRun) -> Vec<Value>
```

It parses `run.report` as `Value`; a parse failure yields an empty vector plus a `tracing::warn!` (the report was written by this server).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --test mcp_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/mcp crates/microwave-api/tests/mcp_test.rs
git commit -m "feat(mcp): add run and converge-error read tools"
```

---

### Task 7: Extract shared node, environment, and cookbook mutation services

**Files:**
- Modify: `crates/microwave-api/src/nodes.rs:125-242`
- Modify: `crates/microwave-api/src/environments.rs:85-265`
- Modify: `crates/microwave-api/src/cookbooks.rs:152-265`

**Interfaces:**
- Produces: `nodes::create_node_object(&AppState, &AuthContext, &Org, &mut Value) -> Result<StoredNode, ApiError>`.
- Produces: `nodes::update_node_object(&AppState, &AuthContext, &Org, &str, &mut Value) -> Result<StoredNode, ApiError>`.
- Produces: `nodes::delete_node_object(&AppState, &AuthContext, &Org, &str) -> Result<StoredNode, ApiError>`.
- Produces: `environments::create_environment_object(&AppState, &AuthContext, &Org, &mut Value) -> Result<StoredObject, ApiError>`.
- Produces: `environments::update_environment_object(&AppState, &AuthContext, &Org, &str, &mut Value) -> Result<(StoredObject, bool), ApiError>` where `bool` is `renamed`.
- Produces: `environments::delete_environment_object(&AppState, &AuthContext, &Org, &str) -> Result<StoredObject, ApiError>`.
- Produces: `cookbooks::put_cookbook_version_object(&AppState, &AuthContext, &Org, &str, &str, &Value, u8, bool) -> Result<(StatusCode, Value), ApiError>`.
- Produces: `cookbooks::delete_cookbook_version_object(&AppState, &AuthContext, &Org, &str, &str, u8) -> Result<Value, ApiError>`.
- Consumes: nothing new; preserves every existing HTTP behavior.

- [ ] **Step 1: Run the existing HTTP regression tests before the refactor**

Run: `cargo test -p microwave-api --test nodes_test --test environments_contract_test --test cookbooks_test --test location_headers_test`
Expected: PASS. Record the result in the task report.

- [ ] **Step 2: Move the logic into service functions**

Move the bodies of the existing handlers into the listed functions, keeping the same order of operations, error mapping, normalization, checksum validation, blob deletion and search indexing. Each function takes the already-resolved `Org`; the axum handler keeps `organization(&auth)?`, status codes, `Location` headers and API-version negotiation.

Details:

- `nodes`: handlers call the services and keep `StatusCode::CREATED`, `location_headers`, and `Json` wrapping. `update_node`/`delete_node` return the stored object as before.
- `environments`: the `_default` rejection moves into `update_environment_object` / `delete_environment_object` as `ApiError::method_not_allowed("The '_default' environment cannot be modified.")`. The axum handlers convert that error back to a response with `Allow: GET`; existing tests still see the header.
- `cookbooks`: `put_cookbook_version_object` returns `(StatusCode, response_body)`; `delete_cookbook_version_object` returns the rendered manifest. The handler keeps the `Location` header for creates.
- Make `normalize_node`, `normalize_environment`, `normalize_cookbook_document`, `validate_cookbook_name`, `validate_version`, `sort_versions_descending` reachable from the MCP module (`pub(crate)` where needed).

- [ ] **Step 3: Run the same regression tests**

Run: `cargo test -p microwave-api --test nodes_test --test environments_contract_test --test cookbooks_test --test location_headers_test`
Expected: PASS with no assertion changes.

- [ ] **Step 4: Commit**

```bash
git add crates/microwave-api/src/nodes.rs crates/microwave-api/src/environments.rs crates/microwave-api/src/cookbooks.rs
git commit -m "refactor(api): share node, environment, and cookbook mutations"
```

---

### Task 8: MCP write tools for nodes and environments

**Files:**
- Modify: `crates/microwave-api/src/mcp/tools.rs`
- Test: `crates/microwave-api/tests/mcp_test.rs`

**Interfaces:**
- Produces tools: `create_node`, `update_node`, `delete_node`, `create_environment`, `update_environment`, `delete_environment`.
- Consumes: the Task 7 services.
- Registration: `McpServer::new` builds `read_tool_router() + write_tool_router()` only when `ctx.writes`.

- [ ] **Step 1: Write the failing write-tool tests**

Add:

- `writes_false_hides_and_rejects_write_tools`: `writes: false`; `tools/list` contains no write tool; `create_node` returns an error containing `"unknown tool"` or `isError: true`.
- `create_update_and_delete_node`: create `web1` with `environment`, `run_list`, `normal`; `get_node` returns it; update with a full document and a changed `run_list`; delete; `get_node` → not found.
- `create_update_and_delete_environment`: create `prod`, update with a full document, rename to `staging`, delete `staging`; deleting `_default` is an error containing `"_default"`.
- `write_acl_denial_is_reported`: non-superuser principal with no node ACL; `create_node` → `isError: true` with `"forbidden"`.
- `write_changes_update_the_search_index`: after creating `web1` through MCP, a signed `GET /organizations/acme/search/node?q=name:web1` against the same app returns one hit; after deleting it through MCP the same search returns zero hits. `Config::default()` selects the embedded backend, so no mock is needed.

Argument structs:

```rust
struct CreateNodeArgs { name: String, environment: Option<String>, run_list: Option<Vec<String>>, normal: Option<Value>, policy_name: Option<String>, policy_group: Option<String> }
struct UpdateNodeArgs { name: String, node: Value }
struct DeleteNodeArgs { name: String }
struct CreateEnvironmentArgs { name: String, attributes: Option<Value>, cookbook_versions: Option<Value> }
struct UpdateEnvironmentArgs { name: String, environment: Value, new_name: Option<String> }
struct DeleteEnvironmentArgs { name: String }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test mcp_test writes_false`
Expected: FAIL because the tools are not registered.

- [ ] **Step 3: Implement the tools**

In a second `#[tool_router(router = write_tool_router, vis = "pub(crate)")]` impl block:

- `create_node`: build `{name, chef_environment, run_list, normal, policy_name, policy_group}` (omit `None`), call `create_node_object`, return `{name, environment, policy_name, policy_group}`.
- `update_node`: call `update_node_object(&name, &mut node.clone())`, return the same compact summary.
- `delete_node`: call `delete_node_object`, return `{name, deleted: true}`.
- `create_environment`: build `{name, default_attributes, override_attributes, cookbook_versions}` from `attributes` and `cookbook_versions`, call `create_environment_object`, return `{name, updated_at}`.
- `update_environment`: when `new_name` is set and differs, insert it into the document as `name`; call `update_environment_object`; return `{name, updated_at, renamed}`.
- `delete_environment`: call `delete_environment_object`; return `{name, deleted: true}`.

All service `ApiError`s map through one helper:

```rust
fn api_error_result(error: ApiError) -> CallToolResult
```

400/409/422 → `error.message`; 403 → `"forbidden"`; 404 → `"not found"`; 5xx → `tracing::error!` and `"internal error"`.

`McpServer::new` becomes:

```rust
let mut tool_router = Self::read_tool_router();
if ctx.writes {
    tool_router = tool_router + Self::write_tool_router();
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --test mcp_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/mcp crates/microwave-api/tests/mcp_test.rs
git commit -m "feat(mcp): add node and environment write tools"
```

---

### Task 9: MCP cookbook upload and delete

**Files:**
- Create: `crates/microwave-api/src/mcp/cookbook_upload.rs`
- Modify: `crates/microwave-api/src/mcp/mod.rs`
- Modify: `crates/microwave-api/src/mcp/tools.rs`
- Test: `crates/microwave-api/tests/mcp_test.rs`

**Interfaces:**
- Produces tools: `upload_cookbook_version`, `delete_cookbook_version`.
- Produces: `cookbook_upload::build_manifest(&McpContext, &UploadCookbookArgs) -> Result<(Value, Vec<(String, Vec<u8>)>), String>`, where the vector is `(checksum, bytes)` for blobs that must be written.
- Consumes: `ensure_checksums` (Task 2), `put_cookbook_version_object` / `delete_cookbook_version_object` (Task 7).

- [ ] **Step 1: Write the failing cookbook-write tests**

Add:

- `upload_cookbook_version_with_inline_files`: upload `nginx` 1.2.0 with a `recipes/default.rb` and a template; assert the summary has `name`, `version`, `frozen: false`; `get_cookbook_version` returns both files; `state.object_store.get(&org.id, &checksum)` equals the decoded content.
- `upload_rejects_missing_base64_and_oversized_payloads`: invalid base64 → `isError`; decode total greater than a 64-byte `max_upload_bytes` → `isError`; 101 files → `isError`.
- `upload_honors_frozen_and_force`: first upload with `frozen: true`; second upload without `force` → `isError`; with `force: true` → success and `frozen: true`.
- `delete_cookbook_version_removes_the_version`: upload then delete; `get_cookbook_version` → not found.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test mcp_test upload_cookbook`
Expected: FAIL because the tool is unknown.

- [ ] **Step 3: Implement the upload pipeline**

`UploadCookbookArgs`:

```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UploadCookbookArgs {
    name: String,
    version: String,
    metadata: Value,
    files: Vec<UploadFile>,
    #[serde(default)]
    frozen: bool,
    #[serde(default)]
    force: bool,
}
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UploadFile {
    path: String,
    content_base64: Option<String>,
    checksum: Option<String>,
    specificity: Option<String>,
}
```

`build_manifest`:

1. Validate name/version with the `pub(crate)` cookbook validators.
2. Reject more than 100 files.
3. For each file: if `content_base64` is present, decode with `base64::engine::general_purpose::STANDARD`, compute `microwave_objectstore::md5_hex`, and record `(checksum, bytes)`. Otherwise require a lowercase 32-hex `checksum`; accept it only if `store.checksum_exists(&org.id, &checksum)` is true.
4. Sum decoded lengths and reject when the total exceeds `ctx.max_upload_bytes`.
5. Build the v2 manifest:

```json
{
  "name": "<name>-<version>",
  "cookbook_name": "<name>",
  "version": "<version>",
  "json_class": "Chef::CookbookVersion",
  "chef_type": "cookbook_version",
  "frozen?": false,
  "metadata": { "...": "metadata with version injected" },
  "all_files": [
    { "name": "<path>", "path": "<path>", "checksum": "<md5>", "specificity": "default" }
  ]
}
```

`name` and `specificity` default to `path` and `"default"` when omitted.

The tool then:

1. Build the manifest and blob list.
2. `store.ensure_checksums(&org.id, &checksums)`.
3. For each blob call `state.object_store.put(&org.id, &checksum, &bytes)`; map failures to `"internal error"`.
4. Call `cookbooks::put_cookbook_version_object(&state, &auth, &org, &name, &version, &manifest, 2, force)`.
5. Return `{name, version, frozen}` from the returned response document. On a frozen conflict, return the `ApiError` message unchanged so an agent sees the HTTP API's wording.

`delete_cookbook_version` calls `delete_cookbook_version_object(..., api_version = 2)` and returns `{name, version, deleted: true}`.

Register both tools in the write router.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --test mcp_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/mcp crates/microwave-api/tests/mcp_test.rs
git commit -m "feat(mcp): add cookbook upload and delete tools"
```

---

### Task 10: Documentation and packaging

**Files:**
- Create: `docs/mcp.md`
- Modify: `packaging/microwave.toml.example`
- Modify: `docs/deployment.md`
- Modify: `README.md`
- Modify: `crates/microwave-config/tests/config_test.rs` (only if the example config needs a parse assertion)

**Interfaces:**
- Consumes: the final configuration and tool surface.

- [ ] **Step 1: Write `docs/mcp.md`**

Cover: enablement, per-org token setup with both the config file and `microwave-ctl set-secret mcp <org> <token>`, `allowed_hosts` for remote agents, an agent configuration example with the `Authorization` header, the complete tool table with read/write markers, the ACL/superuser warning, the destructive-tool warning, limitations (no audit table, no token expiry, `node_runs` excluded from snapshots and `microwave-migrate`, orphaned blobs), and the upload payload format from the spec.

- [ ] **Step 2: Update the example config and pointers**

- `packaging/microwave.toml.example`: document `[data_collector] store`/`keep_runs`, and add a commented `[mcp]` block with `enabled`, the four limits, `allowed_hosts`, `allowed_origins`, and two org examples (one config token, one secret-backed with `writes = false`).
- `docs/deployment.md`: add a short section linking to `docs/mcp.md`, noting that local storage is opt-in and that `writes = true` exposes a destructive surface.
- `README.md`: add the MCP status line and the deployment pointer.

- [ ] **Step 3: Verify the example config parses**

Run: `cargo test -p microwave-config --test config_test`
Expected: PASS. If the example config is not already parsed by a test, add one that reads `packaging/microwave.toml.example` and asserts `Config::from_toml_str(...)` is `Ok`.

- [ ] **Step 4: Commit**

```bash
git add docs/mcp.md docs/deployment.md README.md packaging/microwave.toml.example crates/microwave-config/tests/config_test.rs
git commit -m "docs: document the experimental MCP endpoint"
```

---

### Task 11: Whole-workspace verification

**Files:**
- No source changes expected.

- [ ] **Step 1: Run formatting**

Run: `cargo fmt --check`
Expected: no output, exit 0.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: exit 0.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test --workspace`
Expected: exit 0. Allow several minutes; do not interrupt with a short timeout.

- [ ] **Step 4: Confirm the worktree is clean and report**

Run: `git status --short --branch`
Expected: `## main...origin/main` with no modified files.

Include the exact command output for the three verification commands in the final report. Do not claim green from an earlier run or from a subagent report alone.

- [ ] **Step 5: Commit any verification-only fix**

Only if Steps 1–3 required a change:

```bash
git add -A
git commit -m "fix(mcp): address verification findings"
```
