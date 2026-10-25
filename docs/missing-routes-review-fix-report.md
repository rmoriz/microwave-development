# Missing `oc_erchef` routes — review/fix report

Branch: `missing-routes`  
Base: `ada0873`  
Initial implementation: `7fe004b feat(api): add missing oc_erchef compatibility routes`  
Fix round: working tree after `7fe004b`

## Round 1 — initial implementation

- `GET /organizations/:org/:type/:name/_identifiers` for node objects with
  the upstream `id`/`authz_id`/`org_id` shape, upstream 404 messages, and an
  organization-membership check.
- `ANY /organizations/:org/validate/*` with the signature path bound to the
  wildcard suffix, returning `requestor_name`/`requestor_type`.
- `ANY /organizations/:org/controls` returning the upstream `410 Gone`
  analytics deprecation result before authentication.
- `GET /organizations/:org/environments/:env/recipes` using the best matching
  cookbook version per environment constraint and sorted qualified names.
- `GET /organizations/:org/environments/:env/roles/:role`,
  `GET /organizations/:org/roles/:role/environments`, and
  `GET /organizations/:org/roles/:role/environments/:environment`.
- SQLite integration tests plus a dual-backend HTTP parity scenario.

## Round 2 — review fixes

### 1. `/validate` must bind the signature to the actual body — ADDRESSED

- Kept `validate_signed_path` so the signature is still checked against the
  wildcard path rather than the `/validate/...` request path.
- Removed the `X-Ops-Content-Hash` override. Authentication now always calls
  `verify_request`, which hashes the actual request body and rejects a
  mismatched `X-Ops-Content-Hash`.
- Deleted `canonical_request_with_content_hash` and
  `verify_request_with_content_hash` together with their exports and crypto
  test.
- Updated validation tests to sign the empty body actually sent, matching
  Pedant (`construct_request(..., test_body)` leaves `opts[:payload]` unset).
- Added `validate_rejects_a_signature_for_a_different_body`: a valid signature
  over `{"foo":"bar"}` with an empty validation body returns `401`. The same
  step is part of the dual-backend parity scenario.

### 2. 405 before 401 on unsupported methods — ADDRESSED

- Added a pre-authentication method gate in `middleware.rs` for:
  - `/organizations/:org/validate/*` — `GET, POST, PUT, DELETE`
  - `/organizations/:org/:type/:name/_identifiers` — `GET`
  - `/organizations/:org/roles/:role/environments[/:environment]` — `GET`
  - `/organizations/:org/environments/:environment/roles/:role` — `GET`
- Unsupported methods reach the router without a signature, so webmachine-like
  `405 Method Not Allowed` and the normalized `Allow` header are returned
  instead of `401`.
- Added `internal_read_only_routes_report_405_before_authentication`, which
  sends unsigned unsupported requests and asserts both `405` and `Allow`.

### 3. TODO overclaim for role schema validation — ADDRESSED

- Reverted the role JSON schema validation checkbox in `TODO.md`; only the
  environment run-list read routes remain marked done.
- Corrected the `/validate/*` TODO wording to say the signature covers the
  wildcard path and the actual request body.

### 4. Internal API exposure — ADDRESSED (documented)

- `internal_api.rs` now states that `/validate/*` and `_identifiers` are
  upstream-internal APIs, that Microwave exposes them on the main listener to
  authenticated organization members, and that deployments needing the
  upstream network split should block them at the reverse proxy.
- README HTTP API documentation carries the same note.
- No configuration flag was added; the documented proxy restriction keeps
  existing clients and tests working.

### 5. `qualify_recipe` basename behavior — KEPT

- Nested recipe paths continue to collapse to their basename before `.rb` is
  removed, matching upstream `filename:basename/2`.

## Verification

```sh
cargo fmt --all
CARGO_PROFILE_DEV_DEBUG=0 cargo test -p microwave-api -p microwave-crypto
CARGO_PROFILE_DEV_DEBUG=0 cargo clippy -p microwave-api -p microwave-crypto --all-targets -- -D warnings
PATH="/opt/homebrew/opt/libpq/bin:$PATH" ./scripts/run-postgres-tests.sh
```

Results:

- `microwave-api` + `microwave-crypto`: all test binaries pass (0 failures).
- Clippy: clean with `-D warnings`.
- PostgreSQL gate: passes, including the extended parity scenario with the
  body-mismatch validation step.
- Pedant:
  - `object-identifiers`: 3 examples, 0 failures.
  - `controls`: 2 examples, 0 failures.
  - `nodes`: 331 examples, 0 failures, 5 pending.
  - Validation examples (`authentication` tag): all 9 validate examples pass.
    The tag still reports 37 unrelated pre-existing failures in environment
    authentication-header and environment state assertions; the count is
    unchanged from before this fix round.

## Remaining gaps

- The broader Pedant `roles` and `environments` suites still have unrelated
  pre-existing role/environment CRUD, validation, and header-behavior
  failures. All environment-recipe and role-environment examples pass.
- Internal APIs remain reachable on the main listener by design; deployments
  that require the upstream internal-listener split must enforce it at the
  proxy.
