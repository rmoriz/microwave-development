# State-machine gaps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce the `credential_rotation_required` marker in the server and CLI, and make OAuth authorization-code and refresh-token consumption atomic on both backends.

**Architecture:** A shared marker constant in `microwave-config` lets the request middleware return `503` (health exempt) and the CLI refuse every command but `rotate-shared-secrets`. Two new `OAuthRepository` methods do conditional `UPDATE … RETURNING` transitions in one statement or one transaction, so concurrent exchanges cannot double-mint tokens.

**Tech Stack:** Rust workspace, axum middleware, `rusqlite` (bundled SQLite), `sqlx`/PostgreSQL, the dual-backend matrix in `crates/microwave-store/tests/dual_backend.rs`.

**Spec:** `docs/superpowers/specs/2026-11-12-state-machine-gaps-design.md`

**Status:** implemented on `main` after `v0.6.2` (commits `debf3a7`…`ce80639`).
Verified 2026-11-13 with `cargo test -p microwave-api --test ops_test
credential_rotation_marker`, `cargo test -p microwave-api --test oauth_test`,
`cargo test -p microwave-store --test dual_backend oauth_atomicity` (SQLite and
PostgreSQL both pass), `cargo test -p microwave-ctl-bin --test p1c_compat_test
require_credential_rotation`, and `python3 scripts/check-claims.py` (exit 0).
Three follow-ups landed beyond the steps below: `edb4f05` keeps the read-only
and process commands usable while the marker exists, `b789e0b` covers expired
codes and the wrong-client no-burn path, `ce80639` completes the documented
command list.

## Global Constraints

- Every store change lands in both `crates/microwave-store` (SQLite) and `crates/microwave-store-postgres`.
- No schema change; the existing `revoked_at`/`refresh_token` columns carry the state.
- The marker stays `<data_dir>/credential_rotation_required`, one file per node.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` before every commit.
- One focused commit per task, imperative subject with a scope.
- `python3 scripts/check-claims.py` after docs edits; `docs/claims.toml` needles must keep matching.
- Do not touch the two untracked planning notes in `docs/superpowers/`.

## Review Focus

- **Relative `data_dir`.** The default is `./data`; the CLI and the server must resolve the marker the same way. Both read `config.paths.data_dir` verbatim, so the process working directory decides — verify the systemd unit and container set it consistently.
- **Stat failures.** A permission or I/O error on the marker must not make every request fail; `.exists()` returning false on error is acceptable and must be documented in the middleware comment.
- **Wrong client must not burn state.** A token request with the wrong client id/secret must leave a valid code or refresh token intact.
- **Expired codes stay burned.** Freshness is checked after the atomic consume, so an expired code cannot be exchanged later, even once.
- **No orphan successor.** A crash during refresh rotation must not consume the refresh token without inserting its replacement; the test asserts exactly one active token after concurrent rotation.

---

### Task 1: Server gate for the rotation marker

**Files:**
- Modify: `crates/microwave-config/src/config.rs:959` (near `PathsConfig`), `crates/microwave-config/src/lib.rs:6`
- Modify: `crates/microwave-api/src/middleware.rs:699` (before the maintenance gate)
- Test: `crates/microwave-api/tests/ops_test.rs`

**Interfaces:**
- Produces: `microwave_config::CREDENTIAL_ROTATION_MARKER: &str = "credential_rotation_required"`, re-exported from the crate root.
- Produces: middleware order rotation → maintenance → landing/auth.

- [x] **Step 1: Write the failing test**

Append to `ops_test.rs` (it already has `get`, `support::response_json`, and `Config` via `support`):

```rust
#[tokio::test]
async fn credential_rotation_marker_disables_api_traffic() {
    let data_dir = tempfile::tempdir().unwrap();
    let mut config = microwave_config::Config::default();
    config.paths.data_dir = data_dir.path().to_string_lossy().into_owned();
    let state = support::state_with_config(config);
    let app = support::app(state);

    let marker = data_dir
        .path()
        .join(microwave_config::CREDENTIAL_ROTATION_MARKER);
    std::fs::write(&marker, b"credential rotation required\n").unwrap();

    let response = get(&app, "/server_api_version").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response
        .headers()
        .get("x-ops-server-api-version")
        .is_some());
    let body = support::response_json(response).await;
    assert!(
        body["error"][0]
            .as_str()
            .unwrap()
            .contains("rotate-shared-secrets"),
        "{body}"
    );

    // Health stays reachable.
    let response = get(&app, "/_status").await;
    assert_ne!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    std::fs::remove_file(&marker).unwrap();
    let response = get(&app, "/server_api_version").await;
    assert_eq!(response.status(), StatusCode::OK);
}
```

`tempfile` is already a dev-dependency. The test mirrors
`maintenance_mode_returns_503_without_authentication` in the same file.

- [x] **Step 2: Run it and verify it fails**

Run: `cargo test -p microwave-api --test ops_test credential_rotation_marker`
Expected: compile error (`microwave_config::CREDENTIAL_ROTATION_MARKER` missing).

- [x] **Step 3: Add the constant and the middleware gate**

In `crates/microwave-config/src/config.rs`, above `PathsConfig`:

```rust
/// File name of the sentinel that disables the server until the shared
/// secrets have been rotated. Lives in `paths.data_dir`.
pub const CREDENTIAL_ROTATION_MARKER: &str = "credential_rotation_required";
```

Re-export it from `crates/microwave-config/src/lib.rs` next to the other
`pub use config::{...}` items.

In `middleware.rs`, immediately before the maintenance block:

```rust
    // A rotation sentinel disables the API until the operator rotates the
    // shared secrets. Like maintenance mode, it runs before authentication so
    // an unauthenticated request cannot mutate the store, and health stays
    // reachable. `.exists()` is false on a permission or I/O error, which
    // keeps a broken data directory from taking down every request.
    if !is_maintenance_exempt(path) {
        let marker = std::path::Path::new(&state.config.paths.data_dir)
            .join(microwave_config::CREDENTIAL_ROTATION_MARKER);
        if marker.exists() {
            return with_version_header(
                ApiError::service_unavailable(
                    "Server credentials must be rotated. Run 'microwave-ctl rotate-shared-secrets'.",
                )
                .into_response(),
                &requested_version.to_string(),
                &requested_version.to_string(),
            );
        }
    }
```

- [x] **Step 4: Run the test**

Run: `cargo test -p microwave-api --test ops_test`
Expected: PASS, including the existing maintenance tests.

- [x] **Step 5: Commit**

```bash
git add crates/microwave-config crates/microwave-api
git commit -m "feat(api): refuse traffic while credential rotation is required"
```

---

### Task 2: CLI gate for the rotation marker

**Files:**
- Modify: `crates/microwave-ctl/src/commands.rs:830-860` (marker path and writer)
- Modify: `crates/microwave-ctl/src/lib.rs:146` (after `Context::load`)
- Modify: `crates/microwave-ctl/src/cli.rs:248-252` (doc comment)
- Test: `bin/microwave-ctl/tests/p1c_compat_test.rs:270`

**Interfaces:**
- Consumes: `microwave_config::CREDENTIAL_ROTATION_MARKER`.
- Produces: every command except `RotateSharedSecrets` fails while the marker exists; `Version`, `Pedant` and `Service` still run because they return before the context is loaded.

- [x] **Step 1: Extend the failing test**

In `require_credential_rotation_writes_and_rotate_shared_secrets_clears_the_marker`, between writing the marker and rotating, add:

```rust
    let blocked = run(dir.path(), &["user-create", "bob", "--password", "bob-password"]);
    assert!(!blocked.status.success());
    assert!(stderr(&blocked).contains("rotate-shared-secrets"), "{}", stderr(&blocked));

    // Configuration-free commands still run on a disabled install.
    let version = run(dir.path(), &["version"]);
    assert_success(&version);
```

and after the rotation:

```rust
    let allowed = run(dir.path(), &["user-create", "bob", "--password", "bob-password"]);
    assert_success(&allowed);
```

- [x] **Step 2: Run it and verify it fails**

Run: `cargo test -p microwave-ctl-bin --test p1c_compat_test require_credential_rotation`
Expected: FAIL — `user-create` succeeds while the marker exists.

- [x] **Step 3: Implement the gate**

`commands.rs`: replace the literal in `credential_rotation_marker` with
`context.data_dir.join(microwave_config::CREDENTIAL_ROTATION_MARKER)`.

`lib.rs`, directly after `let context = context::Context::load(...)?;`:

```rust
    // The rotation sentinel disables every store-touching command. The
    // configuration-free commands above already returned; keep them usable on
    // a broken install.
    if !matches!(cli.command, Command::RotateSharedSecrets)
        && commands::credential_rotation_marker(&context).is_file()
    {
        return Err(CtlError::message(
            "Server credentials must be rotated before running this command. \
             Run 'microwave-ctl rotate-shared-secrets' first.",
        ));
    }
```

`cli.rs`: amend the `require-credential-rotation` doc comment to say the server
answers every non-health request with `503` and the CLI refuses commands until
`rotate-shared-secrets` clears the marker; it still does not stop systemd.

- [x] **Step 4: Run the tests**

Run: `cargo test -p microwave-ctl-bin --test p1c_compat_test`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add crates/microwave-ctl bin/microwave-ctl
git commit -m "feat(ctl): block commands until credentials are rotated"
```

---

### Task 3: Atomic OAuth consumes in both stores

**Files:**
- Modify: `crates/microwave-store/src/backend.rs:834-866` (trait)
- Modify: `crates/microwave-store/src/oauth.rs:217`, `:329`
- Modify: `crates/microwave-store/src/backend_sqlite.rs` (delegation)
- Modify: `crates/microwave-store-postgres/src/oauth.rs:260`, `:382`
- Test: `crates/microwave-store/tests/dual_backend.rs` (new `oauth_atomicity` suite, both matrices)

**Interfaces:**
- Produces: `OAuthRepository::consume_oauth_access_grant(&self, token: &str) -> Result<Option<OAuthAccessGrant>, StoreError>`
- Produces: `OAuthRepository::rotate_oauth_access_token(&self, refresh_token: &str, expires_in: Option<i64>) -> Result<Option<OAuthAccessToken>, StoreError>`
- Both return `None` when the row is missing or already revoked/consumed.

- [x] **Step 1: Write the failing concurrency test**

Add to `dual_backend.rs` in `mod suite` and to both `matrix!` lists:

```rust
pub async fn oauth_atomicity(store: &Arc<dyn MetadataStore>) -> Vec<String> {
    let owner = create_user(store.as_ref(), "oauth-atomic-owner").await;
    let app = store
        .create_oauth_application(
            &unique("oauth-atomic-app"),
            "https://example.test/oauth/callback",
            "read",
            false,
        )
        .await
        .expect("create oauth app");

    // Authorization code: exactly one of eight concurrent consumes wins.
    let grant = store
        .create_oauth_access_grant(&owner.id, app.id, "https://example.test/oauth/callback", "read", 300)
        .await
        .expect("create grant");
    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let code = grant.token.clone();
        handles.push(tokio::spawn(async move {
            store
                .consume_oauth_access_grant(&code)
                .await
                .expect("consume grant")
                .is_some()
        }));
    }
    let mut winners = 0;
    for handle in handles {
        if handle.await.expect("join consume") {
            winners += 1;
        }
    }
    assert_eq!(winners, 1, "a code must be exchangeable exactly once");

    // Refresh token: exactly one rotation wins and leaves exactly one active token.
    let token = store
        .create_oauth_access_token(&owner.id, app.id, "read", Some(300), true)
        .await
        .expect("create token");
    let refresh = token.refresh_token.clone().expect("refresh token");
    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let refresh = refresh.clone();
        handles.push(tokio::spawn(async move {
            store
                .rotate_oauth_access_token(&refresh, Some(300))
                .await
                .expect("rotate token")
                .is_some()
        }));
    }
    let mut rotations = 0;
    for handle in handles {
        if handle.await.expect("join rotate") {
            rotations += 1;
        }
    }
    assert_eq!(rotations, 1, "a refresh token must rotate exactly once");
    let active = store
        .list_oauth_access_tokens_for_owner(&owner.id)
        .await
        .expect("active tokens");
    assert_eq!(active.len(), 1, "{active:?}");
    assert_ne!(active[0].token, token.token);
    Vec::new()
}
```

- [x] **Step 2: Run it and verify it fails**

Run: `cargo test -p microwave-store --test dual_backend oauth_atomicity`
Expected: compile error, no `consume_oauth_access_grant`.

- [x] **Step 3: Add the trait methods and SQLite implementation**

Trait additions after `revoke_oauth_access_grant` / `revoke_oauth_access_token_by_refresh`:

```rust
    async fn consume_oauth_access_grant(
        &self,
        token: &str,
    ) -> Result<Option<OAuthAccessGrant>, StoreError>;
    async fn rotate_oauth_access_token(
        &self,
        refresh_token: &str,
        expires_in: Option<i64>,
    ) -> Result<Option<OAuthAccessToken>, StoreError>;
```

SQLite, in `store/src/oauth.rs` (reuse `GRANT_COLUMNS`; `now()` comes from the
identity helpers already used in the file):

```rust
pub fn consume_oauth_access_grant(&self, token: &str) -> Result<Option<OAuthAccessGrant>, StoreError> {
    let connection = self.connection();
    Ok(connection
        .query_row(
            &format!(
                "UPDATE oauth_access_grants SET revoked_at = ?2
                 WHERE token = ?1 AND revoked_at IS NULL
                 RETURNING {GRANT_COLUMNS}"
            ),
            params![token, now()],
            grant_from_row,
        )
        .optional()?)
}
```

`rotate_oauth_access_token`: open a transaction; `UPDATE oauth_access_tokens
SET revoked_at = ?2 WHERE refresh_token = ?1 AND revoked_at IS NULL
RETURNING resource_owner_id, application_id, scopes`; on `None`, return
`Ok(None)`; otherwise insert a new row with fresh `random_token()` access and
refresh tokens, the same owner/application/scopes, and the passed `expires_in`,
commit, and return the constructed `OAuthAccessToken`.

Add the two delegating methods to `backend_sqlite.rs`, and mirror both in
`store-postgres/src/oauth.rs` with `$n` placeholders and `fetch_optional` inside
one `transaction`.

- [x] **Step 4: Run the tests**

Run: `cargo test -p microwave-store --test dual_backend oauth_atomicity`
Expected: PASS for SQLite; the PostgreSQL half skips without a URL.

Run: `MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS=1 scripts/run-postgres-tests.sh oauth_atomicity`
Expected: PASS on both backends.

- [x] **Step 5: Commit**

```bash
git add crates/microwave-store crates/microwave-store-postgres
git commit -m "feat(store): make oauth code and refresh-token use atomic"
```

---

### Task 4: Rewire the OAuth token endpoint

**Files:**
- Modify: `crates/microwave-oauth/src/oauth.rs:195-295`
- Test: `crates/microwave-api/tests/oauth_test.rs:372` (`authorization_code_token_refresh_and_revoke_flow`)

**Interfaces:**
- Consumes: `consume_oauth_access_grant`, `rotate_oauth_access_token` from Task 3.

- [x] **Step 1: Extend the failing flow test**

In `authorization_code_token_refresh_and_revoke_flow`, immediately after the
first successful exchange (after the `scope` assertion, before the bearer
check), add:

```rust
    // A code is single-use.
    let replay = format!(
        "grant_type=authorization_code&code={code}&client_id={}&client_secret={}&redirect_uri={}",
        application.uid, application.secret, "https://supermarket.example/cb"
    );
    let response = request(
        &app,
        "POST",
        "/id/oauth/token",
        &replay,
        "application/x-www-form-urlencoded",
        &[],
    )
    .await;
    assert_eq!(body_json(response).await["error"], "invalid_grant");
```

And in the refresh section, after `refreshed_token` is read and before the
revoke request, add:

```rust
    // The rotated refresh token is dead.
    let stale = format!(
        "grant_type=refresh_token&refresh_token={refresh_token}&client_id={}&client_secret={}",
        application.uid, application.secret
    );
    let response = request(
        &app,
        "POST",
        "/id/oauth/token",
        &stale,
        "application/x-www-form-urlencoded",
        &[],
    )
    .await;
    assert_eq!(body_json(response).await["error"], "invalid_grant");
```

- [x] **Step 2: Run it and verify it fails**

Run: `cargo test -p microwave-api --test oauth_test authorization_code_token_refresh_and_revoke_flow`
Expected: FAIL — the replayed code currently mints a second token.

- [x] **Step 3: Rewire the handler**

`authorization_code`: keep the read used for the application and
`redirect_uri` checks, then:

```rust
            let Some(grant) = state
                .store
                .consume_oauth_access_grant(&code)
                .await?
            else {
                return Err(OAuthError::invalid_grant(
                    "authorization code is expired or already used",
                ));
            };
            if !timestamp_is_fresh(&grant.created_at, grant.expires_in) {
                return Err(OAuthError::invalid_grant(
                    "authorization code is expired or already used",
                ));
            }
```

`refresh_token`: keep the read used for the `application_id` check, then:

```rust
            let Some(token) = state
                .store
                .rotate_oauth_access_token(&refresh_token, Some(ACCESS_TOKEN_TTL_SECONDS))
                .await?
            else {
                return Err(OAuthError::invalid_grant("refresh token was revoked"));
            };
            Ok(token_response(&token))
```

- [x] **Step 4: Run the tests**

Run: `cargo test -p microwave-api --test oauth_test && cargo test -p microwave-api --test http_parity_test oauth`
Expected: PASS; the parity suite's exchange/refresh/revoke flow still matches on both routers.

- [x] **Step 5: Commit**

```bash
git add crates/microwave-oauth crates/microwave-api
git commit -m "fix(oauth): consume codes and refresh tokens atomically"
```

---

### Task 5: Documentation and full verification

**Files:**
- Modify: `docs/deployment.md` (`require-credential-rotation` section)
- Modify: `docs/claims.toml` only if a needle moves

- [x] **Step 1: Update the operator docs**

In the `require-credential-rotation` section, state that the marker now makes
the server answer every non-health request with `503` and makes the CLI refuse
commands until `rotate-shared-secrets` clears it; a restart is still required
for the server to load the new secrets. Keep the existing literal needles that
`docs/claims.toml` references.

- [x] **Step 2: Verify docs and claims**

Run: `python3 scripts/check-claims.py`
Expected: exit 0.

- [x] **Step 3: Run the full gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS=1 scripts/run-postgres-tests.sh
scripts/run-pedant.sh oauth users
```

Expected: all green. The PostgreSQL script's pre-existing
`postgres_integration` deadlock flake may still appear; it reproduces at the
base commit and is not caused by this plan — rerun once and note it if so.

- [x] **Step 4: Commit**

```bash
git add docs
git commit -m "docs(security): document credential-rotation enforcement"
```

---

## Self-review notes

- Spec coverage: marker constant and server gate → Task 1; CLI gate → Task 2; atomic store transitions and concurrency tests → Task 3; handler rewiring and replay tests → Task 4; docs and gates → Task 5.
- Types match across tasks: `consume_oauth_access_grant`/`rotate_oauth_access_token` are introduced in Task 3 and consumed verbatim in Task 4.
- Review Focus items each have a test: relative `data_dir` (Task 1 uses a temp dir), stat failure (code comment, no test — `.exists()` semantics), wrong client (existing wrong-client tests in the parity suite), expired/burned code (Task 4), orphan successor (Task 3's active-token count).
