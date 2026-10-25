# Data Collector Actions and Compliance Proxy — Implementation Report

Branch: `dc-mutations`

- Initial implementation: `ecf4e50` — opt-in `oc_chef_action` mutation notifications and the Automate compliance profile proxy.
- Review fix round: this commit.

## Initial implementation summary

- `crates/microwave-api/src/actions.rs`: upstream-shaped `oc_chef_action`
  messages for successful node, role, environment, data bag/item, client, key,
  policy, cookbook, cookbook artifact, membership, ACL, user, and organization
  writes. One background worker per `AppState`, bounded 256-entry queue,
  configured timeout, failure logging, server-side token only, top-level secret
  redaction, and no collector contact unless configured.
- `crates/microwave-api/src/compliance_proxy.rs`: Automate profile proxy for
  both upstream route spellings, service-token forwarding, status/body
  preservation, 404 when disabled, 405 before auth for non-GET, and a 1 MiB
  response cap.
- Configuration, example file, deployment guide, README, TODO, design spec,
  and the `compliance_proxy` pedant harness tag were updated.
- Original verification: `cargo test -p microwave-api -p microwave-config`,
  clippy with `-D warnings`, and `scripts/run-pedant.sh compliance_proxy`
  (4 examples, 0 failures).

## Review fix round — per-finding disposition

### 1. HIGH — compliance-proxy path traversal / authenticated SSRF — ADDRESSED

- Owner and profile segments are validated after HTTP path decoding. Segments
  equal to `.` or `..`, containing `/`, `\`, or control characters, or empty
  profile segments are rejected with `400` before any upstream request.
- URL construction now parses the configured base with `reqwest::Url`, appends
  `compliance/profiles/{owner}[/profile...]` through `path_segments_mut`, lets
  the URL parser percent-encode segments, preserves the base path prefix, and
  sets the query separately.
- Defense in depth: the final parsed path must start with
  `<base-path>/compliance/profiles/` or the request is rejected.
- Regression tests cover decoded `.`/`..`, `%2E%2E`/`%2e%2e`, encoded slashes
  (`%2F`), profile traversal, legacy-prefix traversal, base-path preservation,
  and the no-forwarding assertion.

### 2. MEDIUM — membership association notifications — ADDRESSED

- `username_from_data` accepts `username`, `user` as a string, `user` as an
  object with `username`, and `name` as a fallback, matching
  `membership::username_from_body` semantics.
- Unit tests cover all three body shapes; an integration test exercises a real
  `POST /organizations/:org/users` with `{"user":"alice"}`.

### 3. MEDIUM — reject action used the invitation UUID — ADDRESSED

- `DELETE /organizations/:org/association_requests/:id` no longer emits an
  action: the path segment is an invitation UUID, and the API provides no
  username without a body. The notification is omitted rather than reporting
  the UUID. A unit test asserts no payload is produced.
- Association acceptance/rejection paths that include the username continue to
  report the user entity.

### 4. MEDIUM — every `PUT 201` was classified as create — ADDRESSED

- Only `PUT /organizations/:org/cookbooks/:name/:version` with `201` is
  classified as `create`. Keys, policy-group associations, cookbook artifacts,
  and all other `PUT 201` responses are `update`.
- Unit tests cover cookbook create, key update, and policy-group association
  update.

### 5. MEDIUM — policy revisions misclassified as policies — ADDRESSED

- `/policies/:policy/revisions[/:revision]` writes are skipped entirely,
  matching upstream (revisions are not reported). Policy create/update/delete
  still report `entity_type: policy`.
- Unit tests cover revision create, revision delete, and policy delete.

### 6. MEDIUM — flaky cold-start tests — ADDRESSED

- Mock listeners now accept a warm-up TCP connection before tests proceed.
- Normal action/proxy timeouts were raised from 1 s to 10 s and wait deadlines
  from 5 s to 15 s.
- The deliberate slow-collector test was separated with a 5 s upstream delay,
  a 200 ms collector timeout, and a 2 s request-path bound. The timeout test
  uses a 1 s delay, a 100 ms timeout, and a 3 s bound.
- Negative assertions sleep 500 ms instead of 200 ms.
- The action, compliance, data-collector, and required-recipe suites were run
  three consecutive times, then the full `microwave-api`/`microwave-config`
  suite twice.

### 7. MEDIUM — notification work before the response — ADDRESSED

- Added a cheap `is_candidate(method, path)` prefilter in the auth middleware,
  so unsupported mutations never clone headers/body into the action path.
- `notify` now only captures request metadata and enqueues a job. JSON parsing,
  entity/task construction, and secret lookup run in the worker.
- The action token is cached in the worker: configured token first, otherwise a
  one-time read of the `data_collector.token` secret. The request path performs
  no secret database lookup.
- Tests still cover secret-token resolution and request-path timing.

### 8. LOW/MEDIUM — remaining correctness/security — ADDRESSED

- Legacy `/compliance/organizations/:org/...`: the auth middleware now extracts
  the organization from the legacy prefix, so client principals authenticate
  against the correct org. Middleware unit tests cover both spellings.
- `compliance_proxy.url`: validated in `Config::validate` (http/https and host
  required), not only in `check-config`.
- `compliance_proxy.token_header`: config validation rejects `Host`,
  `Content-Type`, `Content-Length`, connection/hop-by-hop headers, and invalid
  header-name syntax. The runtime path also falls back to the default header if
  a manually constructed config is invalid.
- `data_collector.actions_enabled` now defaults to **false**. `root_url` alone
  only enables client payload forwarding; mutations require an explicit
  `actions_enabled = true`. Config and integration tests cover both states.
- Audit `service_hostname` now uses `X-Forwarded-Host` before `Host`, matching
  the documented proxy behavior. A unit test covers the precedence.
- Queue drain on shutdown is documented as best-effort: dropping `AppState`
  closes the sender and the worker drains queued jobs, though abrupt process
  termination can truncate the drain. Queue-full and closed-worker drops log a
  cumulative dropped count.
- Action `data` redaction is recursive across objects and arrays for
  `password`, `private_key`, `secret`, `token`, and their `*_password`,
  `*_secret`, `*_token` variants. Nested values are tested.

### 9. Preserved behavior — VERIFIED

- Client `POST /organizations/:org/data-collector` forwarding is unchanged.
- Disabled data collector and compliance routes still return `404`.
- Non-GET on an enabled compliance proxy still returns `405` before auth.
- Data collector and compliance responses remain capped at 1 MiB.
- Neither forwarding path sends client Chef authorization headers.
- Server-side tokens still replace client-supplied tokens.

## Verification commands

```sh
cargo fmt --all
cargo fmt --all -- --check
CARGO_PROFILE_DEV_DEBUG=0 cargo test -p microwave-api -p microwave-config
CARGO_PROFILE_DEV_DEBUG=0 cargo clippy -p microwave-api -p microwave-config --all-targets -- -D warnings
CARGO_PROFILE_DEV_DEBUG=0 scripts/run-pedant.sh compliance_proxy
```

Results:

- Full suite run 1: all `microwave-api` and `microwave-config` test binaries
  passed (`75` lib tests, `9` action tests, `9` compliance tests, `13`
  data-collector tests, `24` config tests, and all existing suites).
- Full suite runs 2 and 3: repeated cold, all passed.
- Targeted action/compliance/data-collector/required-recipe suites: three
  consecutive passes.
- Clippy: clean with `-D warnings`.
- `scripts/run-pedant.sh compliance_proxy`: **4 examples, 0 failures**.
- No vendored `data_collector` pedant spec exists; data-collector forwarding
  and action notifications are covered by Rust integration tests.

## Remaining notes

- Delete action notifications still omit `data` because the DELETE request has
  no body.
- Request bodies over 256 KiB are reported without `data` to bound queue
  memory.
- The token cache is process-local and not invalidated when the
  `data_collector.token` secret changes; restart the process to pick up a new
  token. Configured `data_collector.token` is read at startup.
- No live PostgreSQL run was performed in this fix round; the change adds no
  schema or backend-specific code.
