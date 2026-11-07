# Per-Organization Object Store Buckets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let each organization be routed to its own S3 bucket, with an opt-in strict mode that forbids the shared bucket, all reloadable without a restart.

**Architecture:** A `RoutingObjectStore` decorator holds an `ArcSwap` snapshot of per-org stores keyed by organization id and picks one per call. Config carries an organization map; per-org credentials live in the metadata secrets table. A reloader rebuilds and swaps the snapshot on SIGHUP, on `microwave-ctl reload` and on a bounded periodic check, validating the whole snapshot first and keeping the old one on failure.

**Tech Stack:** Rust, axum, ureq, ArcSwap, SQLite/PostgreSQL store traits, clap-based `microwave-ctl`.

**Spec:** `docs/superpowers/specs/2026-11-07-per-org-object-store-buckets-design.md`

## Global Constraints

- Object key layout stays `organization-<org>/checksum-<md5>` in every bucket.
- Bucket names: lowercase ASCII letters, digits, `-` and `.` only, 3–63 chars, no leading or trailing `-`/`.`.
- No bucket creation, deletion or quota calls; no Garage admin API.
- Per-org credentials live in the secrets table as `object_store.<org>.access_key` and `object_store.<org>.secret_key`.
- The reload applies the `[object_store]` block, the org map and the org set only; nothing else in the config becomes live.
- The org map comes from the config file, plus the `MW_OBJECT_STORE__ORGANIZATIONS` JSON escape hatch; no per-org environment keys.
- Bucket probes: `HEAD <bucket>`, 1.5 s bound each, at most 8 in flight, skipped by `serve --check` and `check-config`.
- Reload status file `<data_dir>/objectstore.reload`: temporary file plus rename, mode `0640`, no secrets, carries boot id and request token.
- Run `cargo fmt --all` and `cargo clippy --workspace --all-targets` before every commit.

## Review Focus

- An unmapped org in strict mode must answer `500`, never `404`, or chef-clients re-upload into nowhere. Pinned in Task 3.
- A `set-secret`-only change must reach the server on interval-only installs. Pinned in Task 10.
- `microwave-ctl reload` must not report success for a rejected snapshot or a stale status. Pinned in Tasks 8 and 9.
- Two orgs on the same `(endpoint, bucket)`, or a bucket typo, must fail before traffic moves. Pinned in Tasks 1 and 7.
- `gather-logs` must emit parseable TOML with scalars before sub-tables and escaped org names. Pinned in Task 12.

---

### Task 1: Configuration surface and structural validation

**Files:**
- Modify: `crates/microwave-config/src/config.rs`
- Test: `crates/microwave-config/tests/config_test.rs`

**Interfaces:**
- Produces: `pub struct ObjectStoreOrgConfig { pub bucket: String, pub endpoint: Option<String>, pub region: Option<String>, pub path_style: Option<bool> }`; on `ObjectStoreConfig`: `organizations: BTreeMap<String, ObjectStoreOrgConfig>`, `require_org_bucket: bool`, `health_bucket: Option<String>`, `reload_interval_seconds: u64`, `verify_buckets: bool`; `pub fn valid_bucket_name(name: &str) -> bool`; `pub struct OrgCredentials { pub access_key: String, pub secret_key: String }`.

- [ ] **Step 1: Write the failing tests**

In `config_test.rs`: a valid map parses; an invalid bucket name (`"A_b"`, `"-x"`, `"ab"`, 64 chars) is rejected with key `object_store.organizations.acme.bucket`; `require_org_bucket` with `bucket` set is rejected; `require_org_bucket` without `health_bucket` is rejected; an invalid `health_bucket` is rejected; two entries with the same `(endpoint, bucket)` are rejected; `local` with entries is rejected; `local` with `require_org_bucket` is rejected; an entry with no `bucket` is rejected; `MW_OBJECT_STORE__ORGANIZATIONS` accepts a JSON object and rejects malformed JSON.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-config config_test`
Expected: FAIL — unknown fields and missing function.

- [ ] **Step 3: Implement**

Add the struct and fields with `#[serde(default)]` on the container. Defaults: `require_org_bucket: false`, `health_bucket: None`, `reload_interval_seconds: 0`, `verify_buckets: true`. Add the env overrides in the same style as the existing `MW_OBJECT_STORE__*` block. `valid_bucket_name` must also accept `_`-free names only, per the constraint. Extend `Config::validate` with the structural rules; parse `MW_OBJECT_STORE__ORGANIZATIONS` via `serde_json` and map errors to `ConfigError::InvalidValue`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-config config_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-config
git commit -m "feat(config): add per-org object store buckets"
```

---

### Task 2: Make `presigned_url` fallible

**Files:**
- Modify: `crates/microwave-objectstore/src/lib.rs:116`, `src/s3.rs:464`, `src/timed.rs:86`, `src/lib.rs:306` (local impl), `src/timed.rs` test double
- Modify: `crates/microwave-api/src/bookshelf.rs`, `src/cookbooks.rs`, `src/cookbook_artifacts.rs`, `src/depsolver.rs`, `src/sandboxes.rs`, `src/mcp/tools.rs`
- Modify: `crates/microwave-api/tests/status_test.rs` (two doubles)
- Test: `crates/microwave-api/tests/cookbooks_test.rs`

**Interfaces:**
- Produces: `fn presigned_url(&self, method: &str, organization_id: &str, checksum: &str) -> Result<String, ObjectStoreError>` on the trait and every impl.
- Produces: `pub(crate) fn add_download_urls(...) -> Result<(), ObjectStoreError>`, `fn annotate_files(...) -> Result<(), ObjectStoreError>`, `pub fn checksum_map(...) -> Result<serde_json::Value, ObjectStoreError>`.

- [ ] **Step 1: Write the failing test**

A cookbook GET on an org whose store returns `Err(NoOrgBucket)` from `presigned_url` answers `500`. Use the existing `status_test.rs` double pattern; add the double in that file if it is shared, otherwise inline in the test module.

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p microwave-api cookbook -- presigned`
Expected: compile error on the return type.

- [ ] **Step 3: Implement**

Change the signature and wrap every existing return in `Ok`. `TimedObjectStore` delegates with `?`. Propagate with `?` through `annotate_files`, `add_download_urls`, `render_manifest`. Restructure `sandboxes.rs`: build the map before the `json!` macro. In `mcp/tools.rs` map the error to `ApiError::internal()` where the caller already does.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-api && cargo test -p microwave-objectstore`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api crates/microwave-objectstore
git commit -m "refactor(objectstore): make presigned_url fallible"
```

---

### Task 3: `NoOrgBucket` and honest metadata answers

**Files:**
- Modify: `crates/microwave-objectstore/src/lib.rs` (error enum)
- Modify: `crates/microwave-api/src/bookshelf.rs:150-175` (`head`, `object_metadata`), `download`
- Test: `crates/microwave-api/tests/status_test.rs` or a new `crates/microwave-api/tests/bookshelf_missing_org_test.rs`

**Interfaces:**
- Produces: `ObjectStoreError::NoOrgBucket`, display `organization has no object store bucket`.
- Produces: `fn object_metadata(...) -> Result<Option<ObjectMetadata>, ObjectStoreError>`.

- [ ] **Step 1: Write the failing tests**

A store that returns `Err(NoOrgBucket)` makes `HEAD /bookshelf/organizations/x/checksums/<md5>` and `GET` answer `500`, and a `NotFound` store makes them answer `404`.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-api --test bookshelf_missing_org_test`
Expected: FAIL, both answer `404`.

- [ ] **Step 3: Implement**

Add the variant. Return `Result` from `object_metadata`; map `Ok(None)` to the existing existence fallback, `Err(NotFound)` to `Ok(None)`, `Err(NoOrgBucket)` and other errors to `Err`. Update `head` and `download` to `?` the result. Keep the `exists` fallback for backends that cannot report metadata.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-api`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api crates/microwave-objectstore
git commit -m "feat(bookshelf): fail loudly for organizations without a bucket"
```

---

### Task 4: `RoutingObjectStore`

**Files:**
- Create: `crates/microwave-objectstore/src/routing.rs`
- Modify: `crates/microwave-objectstore/src/lib.rs` (module, re-exports)
- Test: `crates/microwave-objectstore/tests/routing_store.rs`

**Interfaces:**
- Consumes: `ObjectStore`, `ObjectStoreError::NoOrgBucket`.
- Produces: `pub struct TenantSnapshot { pub default: Option<Arc<dyn ObjectStore + Send + Sync>>, pub health: Option<Arc<dyn ObjectStore + Send + Sync>>, pub by_org: HashMap<String, Arc<dyn ObjectStore + Send + Sync>> }`; `pub struct RoutingObjectStore`; `pub fn new(snapshot: TenantSnapshot) -> Self`; `pub fn install(&self, snapshot: TenantSnapshot)`; `impl ObjectStore for RoutingObjectStore`.
- Produces: `fn bucket(&self) -> Option<&str> { None }` on `ObjectStore`, forwarded by `TimedObjectStore`, returning the bucket on `S3ObjectStore`.

- [ ] **Step 1: Write the failing tests**

Using a recording double: a mapped org routes `put`, `get`, `delete`, `metadata` and `presigned_url` to its store; an unmapped org routes to `default`; with `default: None` an unmapped org returns `NoOrgBucket` from `put`/`get`/`delete` and `false` from `exists`; `health_check` calls only the `health` store; after `install` a new mapping wins and the old one is gone; the presigned URL contains the mapped bucket host and path; a failing `put` logs one line containing the org id, `put` and the bucket, and no key.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-objectstore --test routing_store`
Expected: FAIL, module missing.

- [ ] **Step 3: Implement**

Hold `ArcSwap<TenantSnapshot>`. Every method: `let snapshot = self.snapshot.load(); let store = snapshot.by_org.get(org).cloned().or_else(|| snapshot.default.clone());` then drop the guard and call `store`. `health_check` uses `health`, else `default`, else `Err(InvalidConfig("health bucket is not configured"))`. `exists` maps any error to `false`. Log a failed operation at `tracing::error!` with the org id, the operation and `store.bucket().unwrap_or("-")`, never the checksum or keys.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-objectstore`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-objectstore
git commit -m "feat(objectstore): route blobs per organization"
```

---

### Task 5: Tenant map validation and secret names

**Files:**
- Create: `crates/microwave-config/src/tenant_map.rs`
- Modify: `crates/microwave-config/src/lib.rs` (module, re-exports)
- Modify: `crates/microwave-store/src/secrets.rs` (accept `.` in names — already allowed by `validate_component`, verify)
- Modify: `crates/microwave-ctl/src/secrets.rs` (special case for group `object_store`)
- Test: `crates/microwave-config/tests/tenant_map_test.rs`, `crates/microwave-ctl/src/secrets.rs` tests

**Interfaces:**
- Consumes: `ObjectStoreConfig`, `OrgCredentials`, `valid_org_name`.
- Produces: `pub enum TenantIssue { UnknownOrg(String), MissingEntry(String), MissingCredential(String, &'static str) }` (implements `Display`); `pub fn tenant_issues(config: &ObjectStoreConfig, orgs: &BTreeSet<String>, credentials: &BTreeMap<String, OrgCredentials>) -> Vec<TenantIssue>`.

- [ ] **Step 1: Write the failing tests**

`tenant_issues` reports `MissingEntry` and `MissingCredential` only with `require_org_bucket`, nothing for a complete map, `UnknownOrg` for an entry with no org, and no `MissingEntry` for an org that is only unknown-mapped. `set-secret object_store acme.access_key` succeeds, `acme.foo` and `ACME.access_key` fail, `object_store.secret` still succeeds.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-config tenant_map && cargo test -p microwave-ctl secrets`
Expected: FAIL.

- [ ] **Step 3: Implement**

In `validate_credential`, special-case `object_store`: `secret` is valid, otherwise split on the last `.` and require `access_key`/`secret_key` plus `valid_org_name(org)`. `tenant_issues` iterates orgs for coverage and entries for unknown names, sorted for stable output.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-config && cargo test -p microwave-ctl secrets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-config crates/microwave-ctl/src/secrets.rs
git commit -m "feat(config): validate tenant maps and per-org secret names"
```

---

### Task 6: Startup wiring

**Files:**
- Create: `bin/microwave-server/src/object_store.rs`
- Modify: `bin/microwave-server/src/main.rs` (module, `build_object_store`, `build_timed_object_store`), `src/bootstrap.rs`
- Test: `bin/microwave-server/tests/cli_test.rs` (or the module's unit tests)

**Interfaces:**
- Consumes: `Config`, `OrgCredentials`, `TenantSnapshot`, `RoutingObjectStore`, `S3Config`, `LocalObjectStore`.
- Produces: `pub async fn load_org_credentials(store: &dyn MetadataStore, orgs: &BTreeSet<String>) -> Result<BTreeMap<String, OrgCredentials>, StoreError>`; `pub fn build_snapshot(config: &ObjectStoreConfig, org_ids: &BTreeMap<String, String>, creds: &BTreeMap<String, OrgCredentials>) -> Result<TenantSnapshot, ObjectStoreError>`; `build_object_store` returns `Arc<RoutingObjectStore>`.

- [ ] **Step 1: Write the failing tests**

A strict config with one unmapped org fails startup with the org name in the message; a strict config with a mapped org and both secrets starts; a config with no map yields a router whose default is the shared store; `local` with `require_org_bucket` fails.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-server`
Expected: FAIL.

- [ ] **Step 3: Implement**

`build_snapshot` merges entry fields over the global config into `S3Config`, builds `S3ObjectStore` per entry, the default store (absent in strict mode) and the health store from `health_bucket`, and returns `TenantSnapshot`. The router is constructed once with an empty snapshot and installed. Startup order: `list_orgs`, `load_org_credentials`, `tenant_issues` (log `UnknownOrg`, fail on the rest), `build_snapshot`, install. Wrap the router in `TimedObjectStore` as today.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-server`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bin/microwave-server
git commit -m "feat(server): build per-org object store routes at startup"
```

---

### Task 7: Bucket probe

**Files:**
- Create: `crates/microwave-objectstore/src/probe.rs` (in the objectstore crate so `microwave-ctl` can reuse it)
- Modify: `crates/microwave-objectstore/src/lib.rs` (module, re-exports)
- Test: `crates/microwave-objectstore/tests/probe.rs` with a local HTTP server, mirroring `tests/s3_mock.rs`

**Interfaces:**
- Produces: `pub async fn verify_buckets(config: &ObjectStoreConfig, snapshot: &TenantSnapshot) -> Result<(), ObjectStoreError>`; `pub async fn probe_endpoint(endpoint: &str, bucket: &str, config: &S3Config) -> Result<(), ObjectStoreError>`.

- [ ] **Step 1: Write the failing tests**

A 404 for one bucket rejects; a 200 passes; with `verify_buckets = false` no request is made; 20 buckets never exceed 8 concurrent requests; the per-request timeout stays 1.5 s.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-objectstore --test probe`
Expected: FAIL.

- [ ] **Step 3: Implement**

Probe every entry in `by_org`, `default` and `health` (skip `health` only when it names the same bucket as `default`) with `health_check()` through `tokio::spawn` plus a semaphore of 8; count in-flight requests in the test double. `serve --check` skips the call because it returns before the server starts; keep `build_object_store` probe-free. Gate the whole probe on `config.verify_buckets` and return early.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-objectstore`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-objectstore
git commit -m "feat(objectstore): verify tenant buckets before use"
```

---

### Task 8: Reloader and status file

**Files:**
- Create: `bin/microwave-server/src/reload.rs`
- Modify: `bin/microwave-server/src/main.rs` (`spawn_sighup_handler` joins the reloader), `src/object_store.rs`
- Modify: `crates/microwave-store/src/backend.rs` and `crates/microwave-store-postgres/src/secrets.rs` (`secret_stamp`)
- Test: `bin/microwave-server/src/reload.rs` tests

**Interfaces:**
- Produces: `pub struct Reloader { pub fn new(config_path: Option<PathBuf>, store: Arc<dyn MetadataStore>, router: Arc<RoutingObjectStore>, data_dir: PathBuf) -> Self; pub async fn reload(&self, trigger: Trigger) -> Result<(), ReloadError> }`; `pub enum Trigger { Sighup, Interval, Manual }`; `async fn secret_stamp(&self, name_prefix: &str) -> Result<Option<String>, StoreError>` where the query binds `format!("secret:{name_prefix}%")`.
- Status JSON: `{ "boot_id": String, "token": Option<String>, "trigger": String, "at": String, "outcome": "ok"|"rejected", "message": Option<String> }`.

- [ ] **Step 1: Write the failing tests**

An accepted reload swaps routing; an invalid snapshot keeps the old routing and writes `outcome: "rejected"`; an unknown org logs and keeps going; the status file is written via rename with mode `0640` and the token echoed; `secret_stamp` returns the newest `updated_at` for the prefix and `None` when empty.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-server reload`
Expected: FAIL.

- [ ] **Step 3: Implement**

Reload: read the token file, re-run the same loader, `list_orgs`, `load_org_credentials`, both validation layers, `build_snapshot`, probes, `install`, write the status. Rejections keep the old snapshot. SIGHUP calls `reload(Trigger::Sighup)` after the TLS reload. `secret_stamp` is a `SELECT MAX(updated_at) ... WHERE key LIKE ?1` on both backends. Generate the boot id at start and write it into the status file.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-server && cargo test -p microwave-store`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bin/microwave-server crates/microwave-store crates/microwave-store-postgres
git commit -m "feat(server): reload tenant routes on SIGHUP"
```

---

### Task 9: `microwave-ctl reload`

**Files:**
- Modify: `crates/microwave-ctl/src/cli.rs`, `src/lib.rs`, `src/service.rs`
- Test: `crates/microwave-ctl/src/lib.rs` or `tests/` CLI parsing tests

**Interfaces:**
- Consumes: `tenant_issues`, `valid_bucket_name`, the status/request files.
- Produces: `Command::Reload`, exit codes 1 rejected, 2 timeout, 3 superseded, 4 service not running.

- [ ] **Step 1: Write the failing tests**

CLI parses `reload` alone and with `--unit`; a missing org aborts before signalling with exit 1; a status file with another boot id is ignored; a token match exits 0; a timeout exits 2; a second, newer status from a different token exits 3.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-ctl reload`
Expected: FAIL.

- [ ] **Step 3: Implement**

`reload` loads the config and store, runs `tenant_issues` (error for any issue), writes a random token to `<data_dir>/objectstore.reload.request`, runs `systemctl reload`, polls the status up to 10 s. `hup` stays a restart and gets a doc comment pointing at `reload`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-ctl`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-ctl
git commit -m "feat(ctl): add reload command"
```

---

### Task 10: Periodic reload

**Files:**
- Modify: `bin/microwave-server/src/main.rs` (`spawn_periodic_reloader` or a second task), `src/reload.rs`
- Test: `bin/microwave-server/src/reload.rs`

**Interfaces:**
- Produces: `struct Stamp { config_mtime, config_len, secrets, orgs }` and `fn stamp(&self, reloader: &Reloader, config_path: Option<&Path>) -> Result<Stamp, io::Error>`.

- [ ] **Step 1: Write the failing tests**

A `set-secret` (`secret_stamp` changes) triggers a reload; an `org-create` (`list_orgs` `updated_at` changes) triggers one; an unchanged stamp does not; `reload_interval_seconds = 0` disables the timer.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-server stamp`
Expected: FAIL.

- [ ] **Step 3: Implement**

Compute the stamp from the config file metadata plus `secret_stamp("secret:object_store:")`? — no, `secret_stamp` takes the name prefix, so pass `"object_store:"` and build `secret:{prefix}` inside — plus `list_orgs()?.iter().map(|o| o.updated_at).max()`. Compare with the previous stamp and reload only on change. Prime once at start.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-server`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bin/microwave-server
git commit -m "feat(server): reload tenant routes on a config stamp"
```

---

### Task 11: `org-create` guidance

**Files:**
- Modify: `crates/microwave-ctl/src/commands.rs:305-370`
- Test: `crates/microwave-ctl/src/commands.rs` or `tests/`

**Interfaces:**
- Consumes: `tenant_issues`.
- Produces: the org id and, in strict mode, a warning naming the missing snippet.

- [ ] **Step 1: Write the failing test**

An `org-create` on a strict config prints the org id and the TOML snippet `[object_store.organizations."<name>"]` plus both `set-secret` lines; a non-strict config prints the id without the warning.

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p microwave-ctl org_create`
Expected: FAIL.

- [ ] **Step 3: Implement**

Print `org.id` after creation, then the conditional block.

- [ ] **Step 4: Run the test**

Run: `cargo test -p microwave-ctl`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-ctl
git commit -m "feat(ctl): point org-create at the missing tenant mapping"
```

---

### Task 12: `gather-logs` and `backup --config-only`

**Files:**
- Modify: `crates/microwave-ctl/src/dump.rs:529-560`, `src/backup.rs:365`
- Test: `crates/microwave-ctl/src/dump.rs` tests, `src/backup.rs` tests

**Interfaces:**
- Consumes: `ObjectStoreConfig.organizations`, `valid_bucket_name`.
- Produces: TOML with `[object_store]` scalars first, then one quoted sub-table per org with `bucket` and non-empty optional fields; JSON with an `organizations` object plus `require_org_bucket`, `health_bucket`, `verify_buckets`, `reload_interval_seconds`, credentials injected from the store.

- [ ] **Step 1: Write the failing tests**

`gather-logs` output parses back with `toml`; an org called `acme-prod` appears as `[object_store.organizations."acme-prod"]`; the config backup JSON contains the map with both credentials.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-ctl dump backup`
Expected: FAIL.

- [ ] **Step 3: Implement**

Emit scalars before the loop, then the sub-tables. In the backup, extend the serialized config and fill credentials by reading the secrets.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-ctl`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-ctl
git commit -m "feat(ctl): carry tenant mappings through dump and backup"
```

---

### Task 13: Diagnostics

**Files:**
- Modify: `crates/microwave-ctl/src/diagnostics.rs`
- Test: `crates/microwave-ctl/src/diagnostics.rs` tests

**Interfaces:**
- Consumes: `tenant_issues`, `valid_bucket_name`.
- Produces: `check_config` reports unmapped orgs and missing credentials as problems; `diagnostics --probe` additionally `HEAD`s every configured bucket.

- [ ] **Step 1: Write the failing tests**

`check_config` fails on an unmapped org under strict mode and passes with a complete map; without `--probe` no request is made; with `--probe` a 404 is a problem.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p microwave-ctl diagnostics`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add the checks to the existing `problems`/`checks` structure, and the probe behind the new flag.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-ctl`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-ctl
git commit -m "feat(ctl): report tenant map problems in diagnostics"
```

---

### Task 14: Documentation

**Files:**
- Modify: `docs/deployment.md` (config table, tenant section, migration procedure, backup caveat), `packaging/microwave.toml.example`
- Modify: `docs/claims.toml` if the docs test needs an entry
- Test: `cargo test --workspace` (claims) and `cd website/docs && npm run build`

- [ ] **Step 1: Write the documentation**

Add the `object_store.organizations.*`, `require_org_bucket`, `health_bucket`, `reload_interval_seconds` and `verify_buckets` rows; the `set-secret` form; the strict-mode rules; the probe's `HeadBucket` requirement; the migration order with the copy and the drain; the re-creation id trap; the backup caveat; and a note that `rotate-all` does not cover per-org keys.

- [ ] **Step 2: Run the checks**

Run: `cargo test --workspace && (cd website/docs && npm run build)`
Expected: PASS. Add claim entries for any new asserted fact the claims test requires.

- [ ] **Step 3: Commit**

```bash
git add docs packaging
git commit -m "docs: document per-org object store buckets"
```
