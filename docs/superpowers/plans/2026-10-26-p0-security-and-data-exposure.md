# P0 Security and Data-Exposure Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the verified P0 authorization, credential, session, search-ACL, and depsolver-ACL gaps before any P1/P2 compatibility work.

**Architecture:** Centralize privileged identity in `AuthContext`, enforce org authorization in the organization handlers, bind key self-service to actor id/type, remove in-band password-reset material by default, add session expiry + Secure cookies + same-origin form checks, provision the upstream OpenSearch mapping, apply the configurable search ACL post-filter to both backends, and enforce per-cookbook READ ACLs in the depsolver.

**Tech Stack:** Rust 2021, axum, tokio, rusqlite/sqlx, rustls, serde_json, existing `MetadataStore`/`AuthzRepository`.

**Spec:** This plan implements the verified P0 rows in the 2026-10-26 parity audit: org ACLs, configurable superuser, key self-service identity, password-reset disclosure, OAuth session/CSRF hardening, OpenSearch mapping + ACL filtering, depsolver cookbook ACLs.

## Global Constraints

- Do not change PostgreSQL/SQLite schema unless a task explicitly says so.
- Preserve existing drop-in responses and pedant-green behavior except where the audit says upstream differs.
- Never print private keys, reset tokens, session tokens, or secret values.
- Keep all new config keys opt-in with safe defaults.
- Run `cargo fmt --all`, targeted tests, and clippy `-D warnings` after each task.
- Commit each task separately on `main` (or a dedicated worktree branch merged after review).

## Review Focus

- A non-admin user cannot create/list/delete organizations or read an org they do not belong to.
- Renaming `auth.superuser_name` changes effective superuser everywhere; a user literally named `pivotal` is not privileged unless configured.
- A user whose name equals a client cannot manage that client's keys.
- Password reset never returns a usable token by default.
- Sessions expire, cookies are `Secure` under TLS, and form POSTs from another origin are rejected.
- A fresh OpenSearch cluster serves sorted/field searches after index creation, and strict ACL filtering hides unreadable rows when enabled.
- Depsolver returns 403 `unauthorized_cookbooks` instead of leaking cookbook metadata.

---

### Task 1: Config-aware superuser identity

**Files:**
- Modify: `crates/microwave-api/src/state.rs`
- Modify: `crates/microwave-api/src/middleware.rs`
- Modify: `crates/microwave-api/src/helpers.rs`
- Modify: `crates/microwave-api/src/membership.rs`
- Modify: `crates/microwave-api/src/users.rs`
- Modify: `crates/microwave-api/src/compliance_proxy.rs`
- Test: `crates/microwave-api/tests/auth_superuser_test.rs`

**Interfaces:**
- Produces: `AuthContext { principal, org, key_name, is_superuser: bool }`
- Produces: `helpers::is_superuser(auth: &AuthContext) -> bool` returning `auth.is_superuser`
- Produces: `helpers::require_superuser` unchanged signature.

- [ ] **Step 1: Write failing tests**
  - Configure `auth.superuser_name = "root"`.
  - Assert a signed request from `root` can call a superuser-only route.
  - Assert a signed request from a user named `pivotal` is rejected there.
  - Assert web impersonation verifies against `root`'s key, not a user named `pivotal`.
- [ ] **Step 2: Run tests, expect failure because identity is hard-coded.**
- [ ] **Step 3: Add `is_superuser` to `AuthContext`; set it in middleware from `state.config.auth.superuser_name` and `ActorType::User`.**
- [ ] **Step 4: Replace every `principal.name == "pivotal"` privileged check and `find_user("pivotal")` impersonation lookup with the configured name / `auth.is_superuser`.**
- [ ] **Step 5: Run targeted tests + clippy.**
- [ ] **Step 6: Commit.**

---

### Task 2: Organization authorization

**Files:**
- Modify: `crates/microwave-api/src/organizations.rs`
- Modify: `crates/microwave-api/src/helpers.rs` (add `is_server_admin`)
- Test: `crates/microwave-api/tests/organization_auth_test.rs`

**Interfaces:**
- Produces: `helpers::is_server_admin(state: &AppState, auth: &AuthContext) -> Result<bool, ApiError>` true for configured superuser or membership in global `server-admins`.
- Consumes: Task 1 `is_superuser`.

- [ ] **Step 1: Write failing tests**
  - Normal user `POST /organizations` → 403.
  - Normal user `GET /organizations` → 403; server-admin → 200.
  - Non-member `GET /organizations/:org` → 403; member with READ → 200.
  - Non-admin org member `DELETE /organizations/:org` → 403; org admin with DELETE → 200.
- [ ] **Step 2: Run, expect failures.**
- [ ] **Step 3: Enforce `require_superuser` on create, `is_server_admin` on list, and `require_permission(READ/DELETE)` on get/delete using `org.authz_id`.**
- [ ] **Step 4: Run targeted tests + clippy.**
- [ ] **Step 5: Commit.**

---

### Task 3: Key self-service identity

**Files:**
- Modify: `crates/microwave-api/src/keys.rs`
- Test: `crates/microwave-api/tests/keys_identity_test.rs`

**Interfaces:**
- Consumes: `KeyTarget { actor_id, actor_type, actor_name, ... }`.

- [ ] **Step 1: Write failing test**
  - Create user `shared-name` and client `shared-name` in the same org.
  - User tries `POST/PUT/DELETE /organizations/:org/clients/shared-name/keys` → 403.
  - User manages own keys → 200.
- [ ] **Step 2: Run, expect failure.**
- [ ] **Step 3: Replace name comparison with `auth.principal.actor_id == target.actor_id && auth.principal.actor_type == target.actor_type`; keep non-validator client self-service and ACL fallback.**
- [ ] **Step 4: Run targeted tests + clippy.**
- [ ] **Step 5: Commit.**

---

### Task 4: Password reset must not disclose tokens by default

**Files:**
- Modify: `crates/microwave-config/src/config.rs`
- Modify: `crates/microwave-oauth/src/password_reset.rs`
- Modify: `docs/deployment.md`, `README.md`
- Test: `crates/microwave-oauth/tests/password_reset_test.rs`

**Interfaces:**
- Produces: `OAuthConfig { expose_reset_token: bool }` default `false`, config key `oauth.expose_reset_token`, env `MW_OAUTH__EXPOSE_RESET_TOKEN`.
- `create` response keeps `"status"`/`"notice"`; `reset_url` is absent unless the flag is true.
- HTML link is rendered only when the flag is true.

- [ ] **Step 1: Write failing tests** for default omission and explicit opt-in.
- [ ] **Step 2: Run, expect failure.**
- [ ] **Step 3: Add the config field and gate both JSON and HTML output; document it as development-only.**
- [ ] **Step 4: Run targeted tests + clippy.**
- [ ] **Step 5: Commit.**

---

### Task 5: Session expiry and Secure cookies

**Files:**
- Modify: `crates/microwave-oauth/src/util.rs`
- Modify: `crates/microwave-oauth/src/sessions.rs`
- Modify: `crates/microwave-store/src/oauth.rs`
- Test: `crates/microwave-oauth/tests/session_lifetime_test.rs`

**Interfaces:**
- Produces: `const SESSION_TTL_SECONDS: i64 = 86_400`.
- `cookie_header(token, secure: bool)` appends `; Secure` when `secure`.
- `find_id_session` returns `None` and deletes rows older than the TTL.
- `create_id_session` prunes expired sessions for that user.

- [ ] **Step 1: Write failing tests** for expired-session rejection, pruning, and `Secure` under TLS.
- [ ] **Step 2: Run, expect failure.**
- [ ] **Step 3: Implement TTL checks/cleanup and pass `secure = config.server.tls.is_enabled()` (or `X-Forwarded-Proto=https`).**
- [ ] **Step 4: Run targeted tests + clippy.**
- [ ] **Step 5: Commit.**

---

### Task 6: Same-origin CSRF check for form submissions

**Files:**
- Modify: `crates/microwave-oauth/src/util.rs`
- Modify: `crates/microwave-oauth/src/{sessions,profile,password_reset,applications}.rs`
- Test: `crates/microwave-oauth/tests/csrf_test.rs`

**Interfaces:**
- Produces: `pub fn require_same_origin(headers: &HeaderMap) -> Result<(), OAuthError>`.
- Applies only to form-encoded state-changing requests (`POST/PUT/PATCH/DELETE` with `application/x-www-form-urlencoded`/`multipart/form-data`); JSON/API requests are unchanged.
- Rejects when `Origin`/`Referer` host differs from `Host`; allows requests with neither header only when the session cookie is absent? No: reject form writes without either header.

- [ ] **Step 1: Write failing tests** for same-origin allowed, cross-origin rejected, missing headers rejected.
- [ ] **Step 2: Run, expect failure.**
- [ ] **Step 3: Implement the helper and call it at the start of every form-mutating handler.**
- [ ] **Step 4: Update existing form tests to send `Origin`/`Host`.**
- [ ] **Step 5: Run targeted tests + clippy.**
- [ ] **Step 6: Commit.**

---

### Task 7: OpenSearch index mapping

**Files:**
- Modify: `crates/microwave-api/src/search/opensearch.rs`
- Modify: `crates/microwave-api/src/search/reindex.rs`
- Modify: `crates/microwave-api/src/state.rs` or `search/mod.rs` startup wiring
- Test: `crates/microwave-api/tests/opensearch_mapping_test.rs`
- Docs: `docs/deployment.md`

**Interfaces:**
- Produces: `async fn ensure_index(&self) -> Result<(), SearchError>` that issues `PUT /chef` with the upstream mapping and tolerates `resource_already_exists_exception`.
- Mapping: `content` uses `whitespace` analyzer; `X_CHEF_id_CHEF_X`, `X_CHEF_database_CHEF_X`, `X_CHEF_type_CHEF_X`, `data_bag` are `keyword`.
- Search 404 with a missing index returns an empty result, not 500.
- Startup, first write, and `reindex` call `ensure_index`.

- [ ] **Step 1: Write failing mock-server tests** asserting the `PUT /chef` mapping and empty-result 404 behavior.
- [ ] **Step 2: Run, expect failure.**
- [ ] **Step 3: Implement `ensure_index` and wire it into startup/reindex/write paths.**
- [ ] **Step 4: Run targeted tests + clippy.**
- [ ] **Step 5: Commit.**

---

### Task 8: Configurable search ACL post-filter for both backends

**Files:**
- Modify: `crates/microwave-config/src/config.rs` (`search.strict_result_acls`, default `false`, env `MW_SEARCH__STRICT_RESULT_ACLS`)
- Modify: `crates/microwave-api/src/search/backend.rs`
- Modify: `crates/microwave-api/src/search/mod.rs`
- Modify: `crates/microwave-api/src/search/opensearch.rs`
- Test: `crates/microwave-api/tests/search_acl_test.rs`
- Docs: `docs/deployment.md`

**Interfaces:**
- `SearchBackend` search receives `actor_authz_id`; when `strict_result_acls` is true, both embedded and OpenSearch filter rows by READ on each object's authz id.
- `total` remains the pre-filter upstream count when strict mode is enabled (documented).

- [ ] **Step 1: Write failing tests** for strict on/off and OpenSearch parity.
- [ ] **Step 2: Run, expect failure.**
- [ ] **Step 3: Implement the config flag and filtering; keep embedded behavior unchanged when off.**
- [ ] **Step 4: Run targeted tests + clippy.**
- [ ] **Step 5: Commit.**

---

### Task 9: Depsolver per-cookbook READ ACLs

**Files:**
- Modify: `crates/microwave-api/src/depsolver.rs`
- Test: `crates/microwave-api/tests/depsolver_acl_test.rs`

**Interfaces:**
- For every solved cookbook version, require `Permission::Read` on its `authz_id`.
- On failure return `403` with `{"error":["unauthorized_cookbooks"],"unauthorized_cookbooks":[{"name":...,"version":...}]}` (match upstream shape).

- [ ] **Step 1: Write failing test** where the container is readable but one cookbook is not.
- [ ] **Step 2: Run, expect failure.**
- [ ] **Step 3: Implement per-version checks and the upstream error body.**
- [ ] **Step 4: Run targeted tests + clippy.**
- [ ] **Step 5: Commit.**

---

## Verification Gate

After all tasks:

```sh
cargo fmt --all --check
cargo test -p microwave-api -p microwave-oauth -p microwave-store -p microwave-config
cargo clippy -p microwave-api -p microwave-oauth -p microwave-store -p microwave-config --all-targets -- -D warnings
scripts/run-pedant.sh users organizations clients nodes keys acls oauth search license depsolver
```

Expected: all targeted suites green; pedant tags green except the known plain-HTTP `headers` artifact (not in this tag set).

Then run a fresh reviewer against the full P0 diff before starting P1.
