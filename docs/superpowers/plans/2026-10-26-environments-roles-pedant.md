# Environments/Roles Pedant Fix Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the vendored `environments` and `roles` Pedant suites green against `main` without regressing the already-green core/extended tags.

**Architecture:** Two server-side contract fixes (auth-header validation and PUT name semantics), one environment validation fix, and one harness fixture fix. Auth-header changes are global middleware and must be verified against the full `all` run.

**Tech Stack:** Rust/Axum middleware, chrono, existing Pedant harness.

**Spec:** Current failure output `/tmp/envrole-pedant-clean.log` (2026-10-26) and upstream specs:
- `chef-server/oc-chef-pedant/spec/api/environments/*`
- `chef-server/oc-chef-pedant/spec/api/roles/complete_endpoint_spec.rb`
- `chef-server/oc-chef-pedant/spec/api/auth_spec.rb`
- `chef-server/oc-chef-pedant/lib/pedant/rspec/auth_headers_util.rb`

## Current Findings

- `roles`: **78 examples, 2 failures**. Both are the same wrong behavior: PUT with a changed `name` returns 201/409; upstream expects 400 `Role name mismatch.` and no change.
- `environments`: **610 examples, 39 failures**:
  - 5 `NameError: undefined local variable or method 'client'` in `auth_spec.rb` (`:pending` contexts still evaluate the missing `let`).
  - ~30 auth-header assertions: missing/empty `X-Ops-Sign`/`X-Ops-Timestamp`/`X-Ops-Content-Hash` expect **400** but get 401; old/future timestamps expect **401** but are accepted (200/201).
  - PUT with deleted `name` expects `400 Field 'name' missing`.
  - `cookbook_versions` with `>= 1.2.9223372036854775849` expects `400 Invalid value '...' for cookbook_versions`, but is accepted.
- The earlier P1c “rename” feature is wrong for Pedant: both roles and environments must reject a PUT name change.

## Global Constraints

- Do not weaken P0 security; keep valid signed requests working.
- Missing/empty required headers are **400**, signature failures remain **401**.
- Timestamp skew uses `auth.skew_seconds` (default 900) for both past and future timestamps.
- Keep all existing core/extended Pedant tags green.
- Each task ends with targeted tests, fmt, clippy.

---

### Task 1: Correct auth-header status semantics

**Files:**
- Modify: `crates/microwave-api/src/middleware.rs`
- Test: `crates/microwave-api/tests/auth_header_contract_test.rs`

**Interfaces:**
- `required_header` returns `ApiError::bad_request` for absent/empty values.
- Unsupported/empty `X-Ops-Sign` version returns 400.
- Timestamp parsing/skew failure returns 401.
- Missing `X-Ops-Authorization-*` remains 401.

- [ ] **Step 1: Write failing tests**
  - POST `/organizations/acme/environments` with each of: missing `X-Ops-Sign`, empty `X-Ops-Sign`, `X-Ops-Sign: version=99`, missing/empty `X-Ops-Timestamp`, missing/empty `X-Ops-Content-Hash` → 400.
  - Old timestamp (`now - skew - 1`) and future timestamp (`now + skew + 1`) → 401.
  - Missing `X-Ops-Authorization-1` → 401.
  - A valid signed request still succeeds.
- [ ] **Step 2: Run tests, confirm failures match the log.**
- [ ] **Step 3: Implement the status mapping and timestamp skew check** using `config.auth.skew_seconds` and `chrono::DateTime::parse_from_rfc3339`.
- [ ] **Step 4: Run `scripts/run-pedant.sh headers` and the new tests; confirm no regression.**
- [ ] **Step 5: Commit.**

### Task 2: Restore PUT name validation for environments and roles

**Files:**
- Modify: `crates/microwave-api/src/environments.rs`
- Modify: `crates/microwave-api/src/roles.rs`
- Modify tests: `environments_contract_test.rs`, `roles_contract_test.rs`, `http_parity_test.rs`, `location_headers_test.rs`
- Optional: leave the store `update_named_object` rename helper unused, or remove it if clippy flags it.

**Interfaces:**
- PUT requires a string `name`; missing → `400 {"error":["Field 'name' missing"]}`.
- `name` different from the URL → `400 {"error":["Environment name mismatch."]}` / `400 {"error":["Role name mismatch."]}`.
- No rename, no 201/Location for a changed name; stored object unchanged.
- Same name → normal 200 update.
- DELETE/GET remain unchanged.

- [ ] **Step 1: Replace rename tests with failing rejection tests.**
- [ ] **Step 2: Run tests, confirm 201/409 failures.**
- [ ] **Step 3: Remove rename branching from the handlers; validate `name` before update.**
- [ ] **Step 4: Run the contract, location, parity, and depsolver suites.**
- [ ] **Step 5: Commit.**

### Task 3: Bound and validate environment `cookbook_versions`

**Files:**
- Modify: `crates/microwave-api/src/environments.rs`
- Test: `crates/microwave-api/tests/environments_contract_test.rs`

**Interfaces:**
- Reject any constraint whose version component overflows `u64` or has more than three dot components.
- Exact error: `400 {"error":["Invalid value '>= 1.2.9223372036854775849' for cookbook_versions"]}`.
- Preserve the accepted forms `=`, `>`, `<`, `>=`, `<=`, `~>` with three numeric components.

- [ ] **Step 1: Add failing cases for the huge component and the malformed forms Pedant lists (`>= 1.0.0.0`, `>= 1,0,0`, `>=1.0.0`, leading spaces, 1-/2-component versions).**
- [ ] **Step 2: Run, confirm accepted-then-201 failures.**
- [ ] **Step 3: Tighten the constraint parser and return the exact error.**
- [ ] **Step 4: Run the environment contract tests.**
- [ ] **Step 5: Commit.**

### Task 4: Fix the Pedant `client` fixture

**Files:**
- Modify: `chef-server/oc-chef-pedant/spec/api/auth_spec.rb`
- Optional: `scripts/run-pedant.sh` tag documentation.

**Interfaces:**
- Define `let(:client) { platform.validator_client }` (or `platform.admin_client` if that is the configured client accessor) in the auth spec so the `:pending` client contexts report pending instead of raising `NameError`.
- Do not change upstream assertions or mark real server failures pending.

- [ ] **Step 1: Run `scripts/run-pedant.sh environments` and record the 5 `NameError`s.**
- [ ] **Step 2: Add the missing `let` in the spec.**
- [ ] **Step 3: Re-run and confirm the `NameError`s become pending/skip.**
- [ ] **Step 4: Commit the harness fix separately and document it as a harness limitation.**

### Task 5: Verify and make the suites part of the gate

- [ ] Run `scripts/run-pedant.sh environments roles`; expected 0 failures (only Pedant’s own pending examples).
- [ ] Run `scripts/run-pedant.sh headers users organizations clients nodes keys acls data_bags cookbooks sandboxes search policies oauth license depsolver versioned_behaviors compliance_proxy` and the full `all` run; only the known plain-HTTP `headers` artifact may fail.
- [ ] Add `environments` and `roles` to an optional `strict`/`extra` tag set in `scripts/run-pedant.sh` (not the default core/extended set until stable in CI).
- [ ] Update `docs/pedant-extended-report.md` and `TODO.md` with the counts and the harness `client` note.

## Verification Gate

```sh
cargo fmt --all --check
cargo test -p microwave-api --test auth_header_contract_test --test environments_contract_test --test roles_contract_test --test http_parity_test
cargo clippy -p microwave-api --all-targets -- -D warnings
scripts/run-pedant.sh environments roles
scripts/run-pedant.sh all
```
