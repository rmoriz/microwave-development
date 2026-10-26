# P1 API Compatibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the verified P1 drop-in compatibility gaps after P0, without changing intentional architecture differences.

**Architecture:** P1 is split into three independently testable waves. P1a fixes response shapes and route contracts (membership, environments/roles, OAuth, status/stats/version, cookbook order, default-org/trailing-slash). P1b fixes cross-cutting HTTP/bookshelf/search behavior (Location headers, JSON error envelope, bookshelf limits/headers, search grammar, client reindex). P1c fixes `chef-server-ctl` runbook compatibility.

**Tech Stack:** Rust 2021, axum, serde_json, rusqlite/sqlx, existing test support harness and vendored Pedant.

**Spec:** The 2026-10-26 parity audit (P1 rows).

## Global Constraints

- Preserve P0 security behavior.
- Do not change intentional differences (no Omnibus/runit, no DRBD, no telemetry sender, local bookshelf is not S3).
- Every task adds or updates a real test; run the affected Pedant tag where one exists.
- Keep existing pedant-green behavior; update harness tag sets only with a ledger ruling.
- `cargo fmt`, targeted tests, clippy `-D warnings` per task.

## Review Focus

- Membership/invitation bodies match upstream exactly and remain valid JSON arrays/objects.
- Environment/role updates without `name`, validation failures, and deletes behave like upstream.
- OAuth `PATCH`/`new`/authorized-app/token-info routes work and do not regress the existing flows.
- `/_status` returns 500 when unhealthy; `/_stats?format=text` and unknown formats behave upstream.
- Cookbook versions are newest-first.
- Default-org short paths work without breaking signature validation.

---

### P1a Task 1: Organization membership and invitation shapes

**Files:**
- Modify: `crates/microwave-api/src/membership.rs`
- Test: `crates/microwave-api/tests/membership_shapes_test.rs`

- `GET /organizations/:org/users` → `[{"user":{"username":"..."}}]`.
- `GET /users/:user/organizations` → `[{"organization":{"name","full_name","guid"}}]`.
- Org invitation list → `[{"id","username"}]`; user invitation list → `[{"id","orgname"}]`.
- Creation body includes `uri`, `organization_user`, `organization`, `user`; accept response includes the deprecated `organization.name`.
- Delete association returns the user document, not `{}`.
- Add exact-shape tests against synthetic users/orgs/invitations.

### P1a Task 2: Environment and role contracts

**Files:**
- Modify: `crates/microwave-api/src/environments.rs`
- Modify: `crates/microwave-api/src/roles.rs`
- Test: `crates/microwave-api/tests/environments_contract_test.rs`, `roles_contract_test.rs`

- Reject invalid names, `json_class`/`chef_type` mismatches, unknown fields, and wrong attribute types.
- PUT accepts a missing `name` and uses the URL name; a mismatched name returns upstream's validation error.
- DELETE returns the deleted object, not `{}`.
- `_default` environment PUT/DELETE returns 405 with an `Allow` header.
- Normalize role `run_list` and `env_run_lists` to `recipe[...]`/`role[...]` on write and read.
- Add tests for each behavior.

### P1a Task 3: OAuth route completeness

**Files:**
- Modify: `crates/microwave-oauth/src/lib.rs`, `applications.rs`, `profile.rs`, `password_reset.rs`, `sessions.rs`, `oauth.rs`
- Test: `crates/microwave-api/tests/oauth_routes_test.rs`

- Add `PATCH` aliases where Rails/Doorkeeper accepts PATCH (`/id/profile`, `/id/profile/password`, `/id/password-reset`, `/id/oauth/applications/:id`).
- Add `GET /id/password-reset/new`, `GET /id/sessions/new`, `GET /id/oauth/applications/new`, `DELETE /id/sessions/:id`, `DELETE /id/oauth/authorized_applications/:id`, `GET /id/oauth/token/info`.
- `GET /id/oauth/authorized_applications` lists the current user's grants; DELETE revokes one.
- `/id` returns a 301/308 redirect to `/id/` or documents why not.
- Add route/behavior tests; keep CSRF checks on state-changing form routes.

### P1a Task 4: Status, stats, version, cookbook order

**Files:**
- Modify: `crates/microwave-api/src/lib.rs`
- Modify: `crates/microwave-api/src/cookbooks.rs`
- Modify: `crates/microwave-config/src/config.rs` (stats basic-auth optional; default off)
- Test: `crates/microwave-api/tests/status_contract_test.rs`, `cookbooks_test.rs`

- `/_status` returns HTTP 500 when `dependencies.healthy()` is false, keeping the body.
- `/_stats?format=text` returns Prometheus/text; unknown `format` returns 406.
- Add optional basic auth for `/_stats` (`stats.username`/`stats.password` or a htpasswd-like secret) with default disabled; document.
- Add `GET /version` returning the version manifest/version string.
- `GET /cookbooks/:name` sorts versions newest-first.
- Tests for all.

### P1a Task 5: Default-org rewrite and trailing slashes

**Files:**
- Modify: `crates/microwave-config/src/config.rs` (`server.default_orgname`, optional)
- Modify: `crates/microwave-api/src/middleware.rs` (rewrite before signature validation? preserve signed path)
- Test: `crates/microwave-api/tests/routing_contract_test.rs`

- Add `server.default_orgname` (default empty/disabled).
- Rewrite top-level paths (`/nodes`, `/search`, `/cookbooks`, `/data`, `/roles`, `/sandboxes`, `/environments`, `/clients`, `/principals`, `/groups`, `/containers`) to `/organizations/{default_org}/...` when enabled.
- Accept one trailing slash on all routes; preserve the original path for signature verification.
- Tests for rewritten paths, signed requests, and trailing slashes.

---

### P1b (follow-up plan after P1a merges)

- Location headers on creates/renames.
- Malformed-JSON Chef error envelope.
- Bookshelf 250 MiB + streaming + ETag/Last-Modified/202/204.
- Search grammar (default OR, `&&`/`||`, wildcard fields, phrases, `~`/`^`), client update reindex, embedded `X_CHEF_*` fields.
- `strict_search_result_acls` batched lookup/scalability.

### P1c (follow-up plan after P1b merges)

- Top-level `chef-server-ctl` service aliases (`start/stop/restart/status/tail/hup/service-list`).
- `backup` default destination + upstream flags; `psql SERVICE`.
- `user-create`/`org-create` positional compatibility, validator client, default key output.
- `show-service-credentials`, `require-credential-rotation`.
- Orphan blob janitor / CLI org-delete blob+search cleanup.

## Verification Gate (P1a)

```sh
cargo fmt --all --check
cargo test -p microwave-api -p microwave-oauth -p microwave-config
cargo clippy -p microwave-api -p microwave-oauth -p microwave-config --all-targets -- -D warnings
scripts/run-pedant.sh users organizations clients nodes
```

Review the P1a diff, merge, then write/execute the P1b plan.
