# Goiardi Importer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `microwave-migrate from-goiardi`, a read-only crawler that turns a flat goiardi API into a Microwave snapshot with the default organization policy, and make goiardi snapshots refuse a non-empty target.

**Architecture:** A new `goiardi` module reuses HTTP, collection parsing, deterministic ids and blob writing extracted from `from_api` into `crawl`. It writes the same snapshot envelope, marked `source.layout = "goiardi"`. Policy rows (containers, groups, ACL matrix, `_default`) are synthesized as data and pinned against `microwave-authz::create_organization` by a comparison test. Both import paths gain an emptiness preflight for goiardi snapshots.

**Tech Stack:** Rust, `ureq` 3 with `socks-proxy`, `axum` test fixtures, `rusqlite`/`sqlx`, `microwave-store`, `microwave-authz` (dev-only).

**Spec:** `docs/superpowers/specs/2026-11-02-goiardi-importer-design.md`

## Global Constraints

- No internal host names in code, docs or tests; examples use `chef.example.com`, `acme`, `127.0.0.1:1080`.
- Snapshot envelope stays `SNAPSHOT_VERSION = 1`; `source.kind` stays `legacy-api`; producer is `source.layout = Some("goiardi")`.
- Crawl is GET-only; never POST/PUT/DELETE against the source.
- Blob limits: 64 MiB per blob, 4 GiB per run.
- Empty target definition: `orgs == 0 && users <= 1`.
- Every task ends on its own test command; Task 10 runs `cargo fmt --all`,
  `cargo clippy --workspace --all-targets` and `cargo test --workspace` before
  its commit.
- Docs in English; placeholder URLs only.
- Follow the surrounding style: `MigrateError`, `write_snapshot`, `redacted_url`, `synthetic_id`.

## Review Focus

- **Non-empty target:** a goiardi snapshot into a target with an org or a second user must fail before any write, naming both counts.
- **Chef snapshot without `layout`:** must keep merging into an existing store, unchanged.
- **Checksum without `url`:** the crawler must derive `<base>/file_store/<checksum>` instead of dropping the blob.
- **`--skip-orphan-clients`:** must never drop `chef-validator` or `chef-webui`.
- **Bad-but-plausible records:** a missing `public_key`, a data-bag item without `id`, a node naming a missing environment warn and continue; structural failures abort.

---

### Task 1: Extract shared crawl plumbing

**Files:**
- Create: `crates/microwave-migrate/src/crawl.rs`
- Modify: `crates/microwave-migrate/src/from_api.rs` (HTTP, parsing, id and blob helpers move out, call sites delegate)
- Modify: `crates/microwave-migrate/src/lib.rs` (`mod crawl;`)
- Modify: `Cargo.toml` (add `socks-proxy` to `ureq` features)
- Test: `crates/microwave-migrate/tests/from_api_test.rs` (unchanged, must stay green)

**Interfaces:**
- Consumes: nothing.
- Produces (all `pub(crate)`):
  - `struct Fetcher`
  - `Fetcher::new(base_url: &str, client_name: &str, key_path: &Path, api_version: u32, socks5: Option<&str>) -> Result<Fetcher, MigrateError>`
  - `Fetcher::resolve(&self, reference: &str) -> String`
  - `Fetcher::signed_get(&self, url: &str) -> Result<(u16, String), MigrateError>`
  - `Fetcher::get_optional_raw(&self, url: &str) -> Result<Option<(Value, String)>, MigrateError>`
  - `Fetcher::get_optional_json(&self, url: &str) -> Result<Option<Value>, MigrateError>`
  - `Fetcher::get_collection(&self, path: &str) -> Result<Vec<CollectionEntry>, MigrateError>`
  - `Fetcher::get_unsigned_bytes(&self, url: &str, redacted: &str) -> Result<(u16, Vec<u8>), MigrateError>`
  - `synthetic_id(scope: &str) -> String`, `synthetic_i64(scope: &str) -> i64`, `encode_segment(segment: &str) -> String`, `is_absolute_url(value: &str) -> bool`, `redacted_url(value: &str) -> String`
  - `string_field(value: &Value, key: &str) -> Option<String>`, `bool_field(value: &Value, key: &str) -> Option<bool>`, `cookbook_files(value: &Value) -> Vec<(String, Option<String>)>`
  - `MAX_BLOB_BYTES`, `MAX_TOTAL_BLOB_BYTES`
  - `struct CollectionEntry { name: String, value: Value, url: Option<String> }`
  - `parse_collection(value: Value) -> (Vec<CollectionEntry>, Option<String>)`
  - `struct BlobSummary { pub downloaded: usize, pub skipped: usize }`
  - `store_blob(store: &LocalObjectStore, org_id: &str, checksum: &str, reference: &str, fetcher: &Fetcher, summary: &mut BlobSummary, downloaded_bytes: &mut u64) -> Result<(), MigrateError>`

- [ ] **Step 1: Add the feature and module**

Add `"socks-proxy"` to the workspace `ureq` features and `mod crawl;` next to `mod from_api;`.

- [ ] **Step 2: Move the helpers**

Move the listed items out of `from_api.rs` into `crawl.rs`. `store_blob` is
carved out of `Crawler::download_blobs`: resolve the reference, fetch it with
`get_unsigned_bytes` when it is absolute and with `signed_get_bytes` when it is
relative, reject a non-2xx status or empty body, verify the MD5, enforce
`MAX_TOTAL_BLOB_BYTES`, then `LocalObjectStore::put`. The relative branch stays
signed because `from_api_test` asserts it; goiardi ignores the headers when
signatures are disabled.

In `Fetcher::new`, build the agent and, when `socks5` is `Some(host_port)`,
split on the last `:`; a missing or non-numeric port is
`MigrateError::Config`, otherwise set
`proxy(Some(Proxy::new(&format!("socks5://{host_port}"))?))` and map the proxy
error to `MigrateError::Config`.

- [ ] **Step 3: Delegate from `from_api`**

Keep `Crawler`'s field names but replace its transport/parse/id calls with `self.fetcher.*` and the free helpers. `Crawler::new` calls `Fetcher::new(url, client_name, key_path, api_version, None)`.

- [ ] **Step 4: Verify**

Run: `cargo test -p microwave-migrate --test from_api_test`
Expected: PASS, byte-identical snapshot assertions to before the split.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/microwave-migrate/src/crawl.rs crates/microwave-migrate/src/from_api.rs crates/microwave-migrate/src/lib.rs
git commit -m "refactor(migrate): extract shared legacy crawl plumbing"
```

---

### Task 2: Snapshot layout marker and empty-target preflight

**Files:**
- Modify: `crates/microwave-migrate/src/lib.rs` (`SnapshotSource.layout`, `MigrateError::TargetNotEmpty`, guards in both import functions, the three `SnapshotSource` literals)
- Modify: `crates/microwave-store/src/migration.rs` (`ImportTargetCounts`, `SqliteStore::import_target_counts`)
- Modify: `crates/microwave-store-postgres/src/migration.rs` (`PostgresStore::import_target_counts`)
- Test: `crates/microwave-migrate/tests/import_preflight_test.rs`
- Test: `crates/microwave-store-postgres/tests/postgres_migration_test.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `SnapshotSource.layout: Option<String>` with `#[serde(default)]`
  - `MigrateError::TargetNotEmpty { orgs: i64, users: i64 }`, message `"target already contains data (orgs: {orgs}, users: {users}); goiardi imports do not merge, use a fresh data directory or database"`
  - `struct ImportTargetCounts { pub orgs: i64, pub users: i64 }`
  - `SqliteStore::import_target_counts(&self) -> Result<ImportTargetCounts, StoreError>`
  - `PostgresStore::import_target_counts(&self) -> Result<ImportTargetCounts, StoreError>`

- [ ] **Step 1: Write the failing test**

```rust
// tests/import_preflight_test.rs
fn goiardi_snapshot() -> Snapshot { /* minimal envelope with layout Some("goiardi"), one org, one user */ }

#[test]
fn rejects_non_empty_target_and_accepts_empty_one() {
    let empty = tempdir();
    import(empty.path(), &write(goiardi_snapshot())).unwrap();
    let error = import(empty.path(), &write(goiardi_snapshot())).unwrap_err();
    assert!(matches!(error, MigrateError::TargetNotEmpty { .. }));
    assert!(error.to_string().contains("do not merge"));
}

#[test]
fn chef_snapshot_still_merges() {
    // layout None, import twice, second succeeds
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p microwave-migrate --test import_preflight_test`
Expected: FAIL, no `TargetNotEmpty` and no `layout` field.

- [ ] **Step 3: Implement**

Add the field, error variant, counts struct and queries (`SELECT (SELECT COUNT(*) FROM orgs), (SELECT COUNT(*) FROM users)`). All three `SnapshotSource` literals keep `layout: None`; only the goiardi crawler sets it. Guard before `import_snapshot`: in `import_database_with_compression` after the store is opened, in `import_postgres_with_compression` after `connect_with_compression`. When `snapshot.source.layout.as_deref() == Some("goiardi")` and `orgs > 0 || users > 1`, return `TargetNotEmpty`. Opening alone still creates an empty database or connects, which is harmless; no row is written before the check.

- [ ] **Step 4: Postgres parity test**

In `postgres_migration_test.rs`, add a `#[tokio::test]` that opens a clean schema, asserts `{0,0}`, inserts one org, asserts `{1,0}`. Skip when `MICROWAVE_TEST_POSTGRES_URL` is unset, matching the file's existing guard.

- [ ] **Step 5: Verify and commit**

Run: `cargo test -p microwave-migrate --test import_preflight_test && cargo test -p microwave-migrate --test from_api_test`
Expected: PASS.

```bash
git add Cargo.toml crates/microwave-migrate/src/lib.rs crates/microwave-migrate/tests/import_preflight_test.rs crates/microwave-store/src/migration.rs crates/microwave-store-postgres/src/migration.rs crates/microwave-store-postgres/tests/postgres_migration_test.rs
git commit -m "feat(migrate): refuse non-empty targets for goiardi snapshots"
```

---

### Task 3: Goiardi options and identity crawl

**Files:**
- Create: `crates/microwave-migrate/src/goiardi.rs`
- Modify: `crates/microwave-migrate/src/lib.rs` (`mod goiardi;` and re-exports)
- Create: `crates/microwave-migrate/tests/common/mod.rs` (shared axum harness and `options` helper)
- Test: `crates/microwave-migrate/tests/goiardi_test.rs` (`mod common;`)

**Interfaces:**
- Consumes: `crawl::{Fetcher, CollectionEntry, synthetic_id}`.
- Produces:
  - `struct Crawler` with `fetcher`, `org`, `generated_at`, `data: StoreSnapshot`, `user_authz: HashMap<String, String>`, `client_authz: HashMap<String, String>`, `object_authz: Vec<(String, String)>`, `seen_authz: HashSet<String>`, `seen_checksums: HashSet<String>`, `pending_blobs: HashMap<(String, String), String>`, `blob_summary: BlobSummary`, `downloaded_blob_bytes: u64`, `environment_names: HashSet<String>` and `summary: GoiardiSummary`
  - `pub struct GoiardiOptions { pub url: String, pub client_name: String, pub key_path: PathBuf, pub output: PathBuf, pub org: String, pub org_full_name: Option<String>, pub bookshelf_dir: Option<PathBuf>, pub socks5: Option<String>, pub skip_orphan_clients: bool, pub dry_run: bool }` plus `pub fn new(url: impl Into<String>, client_name: impl Into<String>, key_path: impl Into<PathBuf>, output: impl Into<PathBuf>, org: impl Into<String>) -> Self`. `Fetcher` gets `api_version: 2`; goiardi ignores `X-Ops-Server-API-Version`.
  - `pub struct GoiardiSummary { pub users: usize, pub clients: usize, pub nodes: usize, pub roles: usize, pub environments: usize, pub data_bags: usize, pub data_bag_items: usize, pub cookbooks: usize, pub cookbook_versions: usize, pub checksums: usize, pub blobs_downloaded: usize, pub blobs_skipped: usize, pub warnings: Vec<String> }`
  - `pub fn crawl_from_goiardi(options: &GoiardiOptions) -> Result<(Snapshot, GoiardiSummary), MigrateError>`
  - `pub fn from_goiardi_with_options(options: &GoiardiOptions) -> Result<GoiardiSummary, MigrateError>`

- [ ] **Step 1: Write the fixture and failing test**

Copy the axum harness shape from `from_api_test.rs` into `tests/common/mod.rs`: the recording server, `json_route(Value)` and `raw_route(&'static str)` route helpers, `bytes_route`, and `blob_fixture(seed) -> (Vec<u8>, String, &'static str)` returning body, MD5 and the leaked `/file_store/<checksum>` path. Also add `options(url, dir) -> GoiardiOptions` with output `dir/snapshot.json` and org `acme`, so later tasks only override fields, and `mod common;` in `goiardi_test.rs`. Routes are flat. Fixture bodies:

```rust
use common::{options, start_server};

const USER: &str = r#"{"username":"admin","name":"admin","email":"ops@example.com","admin":true,"public_key":"<PUBLIC_KEY>"}"#;
const CLIENT: &str = r#"{"name":"web1","admin":false,"validator":false,"public_key":"<PUBLIC_KEY>"}"#;
```

Lists are name→URL maps: `{"/users": json!({"admin": "/users/admin"})}`, same for `/clients`.

```rust
#[test]
fn crawls_users_and_clients_with_keys_and_marks_the_layout() {
    let dir = tempdir();
    let dir_path = dir.path();
    let routes = vec![
        ("/users", json_route(json!({"admin": "/users/admin"}))),
        ("/users/admin", raw_route(USER)),
        ("/clients", json_route(json!({"web1": "/clients/web1"}))),
        ("/clients/web1", raw_route(CLIENT)),
    ];
    let (url, records) = start_server(routes);
    let (snapshot, summary) = crawl_from_goiardi(&options(&url, dir_path)).unwrap();
    assert_eq!(snapshot.source.layout.as_deref(), Some("goiardi"));
    assert_eq!(snapshot.data.users[0].username, "admin");
    assert!(snapshot.data.users[0].admin);
    assert_eq!(snapshot.data.users[0].password_hash, "");
    assert_eq!(snapshot.data.clients[0].name, "web1");
    assert_eq!(snapshot.data.actor_keys[0].public_key, PUBLIC_KEY);
    assert_eq!(summary.clients, 1);
    assert!(records.lock().unwrap().iter().all(|record| record.method == "GET"));
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p microwave-migrate --test goiardi_test`
Expected: FAIL, module missing.

- [ ] **Step 3: Implement the org and identity crawl**

Validate `org` against `[a-z0-9-]+` (`MigrateError::Config`). Build `OrgRow` with `synthetic_id("org:<org>")`, full name from `org_full_name` or the org, and an `authz_entities` row of type `actor`.

For every user add `UserRow { username: <username>, email, admin, password_hash: String::new(), password_salt: String::new(), hash_type: "bcrypt", display_name: <name or username>, .. }`, an authz `actor`, and an `ActorKeyRow` from `public_key` (`key_name = "default"`, `expires_at = "infinity"`); remember the authz id in a `user_authz` map. The user named `admin` also gets an `OrgUserAssociationRow` whose `last_updated_by` is `admin`. For every client add `ClientRow { name, validator, admin, .. }`, an authz `actor`, a key, and the client authz map. A missing `public_key` pushes a warning and the actor is still imported.

The top-level function crawls users, then clients, builds the snapshot with `source.kind = LegacyApi`, `source.url = Some(url)`, `source.layout = Some("goiardi")`, and writes it unless `dry_run`.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p microwave-migrate --test goiardi_test`
Expected: PASS.

```bash
git add crates/microwave-migrate/src/goiardi.rs crates/microwave-migrate/src/lib.rs crates/microwave-migrate/tests/common/mod.rs crates/microwave-migrate/tests/goiardi_test.rs
git commit -m "feat(migrate): crawl goiardi users and clients"
```

---

### Task 4: Objects crawl

**Files:**
- Modify: `crates/microwave-migrate/src/goiardi.rs`
- Test: `crates/microwave-migrate/tests/goiardi_test.rs`

**Interfaces:**
- Consumes: the authz maps and summary from Task 3.
- Produces: `fn crawl_nodes(&mut self, org: &OrgRow) -> Result<(), MigrateError>`, `fn crawl_named_objects(&mut self, org: &OrgRow) -> Result<(), MigrateError>`, `fn crawl_data_bags(&mut self, org: &OrgRow) -> Result<(), MigrateError>`, `fn filter_orphan_clients(&mut self, node_names: &HashSet<String>)` and a `HashSet<String> environment_names` field.

- [ ] **Step 1: Write the failing test**

```rust
const NODE: &str = r#"{"name":"web1","chef_environment":"_default","normal":{},"run_list":[]}"#;
const ROLE: &str = r#"{"name":"web","run_list":[],"default_attributes":{},"override_attributes":{}}"#;
const ENV: &str = r#"{"name":"production","cookbook_versions":{},"default_attributes":{},"override_attributes":{}}"#;
const BAG_ITEM: &str = r#"{"id":"db","password":"x"}"#;

fn crawl_objects_routes() -> Vec<(&'static str, Route)> {
    vec![
        ("/nodes", json_route(json!({"web1": "/nodes/web1"}))),
        ("/nodes/web1", raw_route(NODE)),
        ("/roles", json_route(json!({"web": "/roles/web"}))),
        ("/roles/web", raw_route(ROLE)),
        ("/environments", json_route(json!({"production": "/environments/production"}))),
        ("/environments/production", raw_route(ENV)),
        ("/data", json_route(json!({"secrets": "/data/secrets"}))),
        ("/data/secrets", json_route(json!({"db": "/data/secrets/db"}))),
        ("/data/secrets/db", raw_route(BAG_ITEM)),
    ]
}

#[test]
fn crawls_objects_raw_and_skips_default_environment() {
    let dir = tempdir();
    let (url, _) = start_server(crawl_objects_routes());
    let (snapshot, summary) = crawl_from_goiardi(&options(&url, dir.path())).unwrap();
    assert_eq!(snapshot.data.nodes[0].name, "web1");
    assert_eq!(snapshot.data.nodes[0].serialized_object.as_deref(), Some(NODE));
    assert_eq!(snapshot.data.roles[0].name, "web");
    assert_eq!(snapshot.data.environments.len(), 1); // production only
    assert_eq!(snapshot.data.environments[0].name, "production");
    assert_eq!(snapshot.data.data_bags[0].name, "secrets");
    assert_eq!(snapshot.data.data_bag_items[0].item_name, "db");
    assert_eq!(summary.data_bag_items, 1);
}

#[test]
fn warns_about_an_unknown_node_environment_and_keeps_the_node() {
    let dir = tempdir();
    let mut routes = crawl_objects_routes();
    routes[1].1 = raw_route(r#"{"name":"web1","chef_environment":"gone","normal":{},"run_list":[]}"#);
    let (url, _) = start_server(routes);
    let (snapshot, summary) = crawl_from_goiardi(&options(&url, dir.path())).unwrap();
    assert!(summary.warnings.iter().any(|warning| warning.contains("gone")));
    assert!(snapshot.data.nodes.iter().any(|node| node.name == "web1"));
}

#[test]
fn skip_orphan_clients_keeps_validator_and_webui() {
    let dir = tempdir();
    let mut routes = crawl_objects_routes();
    routes.extend([
        ("/clients", json_route(json!({
            "web1": "/clients/web1",
            "old1": "/clients/old1",
            "chef-validator": "/clients/chef-validator",
            "chef-webui": "/clients/chef-webui",
        }))),
        ("/clients/web1", raw_route(CLIENT)),
        ("/clients/old1", raw_route(r#"{"name":"old1","public_key":"<PUBLIC_KEY>"}"#)),
        ("/clients/chef-validator", raw_route(r#"{"name":"chef-validator","validator":true,"public_key":"<PUBLIC_KEY>"}"#)),
        ("/clients/chef-webui", raw_route(r#"{"name":"chef-webui","admin":true,"public_key":"<PUBLIC_KEY>"}"#)),
    ]);
    let (url, _) = start_server(routes);
    let mut options = options(&url, dir.path());
    options.skip_orphan_clients = true;
    let (snapshot, _) = crawl_from_goiardi(&options).unwrap();
    let mut names: Vec<_> = snapshot.data.clients.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["chef-validator", "chef-webui", "web1"]);
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p microwave-migrate --test goiardi_test crawls_objects`
Expected: FAIL.

- [ ] **Step 3: Implement**

Use `get_collection` on `/nodes`, `/roles`, `/environments`, `/data`, then `/{bag}` for items. Store raw bodies from the detail response, never re-serialize.

Rows: `NodeRow` (`environment` from `chef_environment`, else `_default`), `RoleRow`, `EnvironmentRow`, `NamedObjectRow` for the data bag (from the `/data` list name, not a body) and `DataBagItemRow` (`item_name` from `id`). Push an `authz_entities` row of type `object` per object and remember its authz id in an `object_authz: Vec<(String, String)>` of `(container_name, authz_id)` for Task 6.

Collect every environment name into `environment_names` first, then skip the `_default` row. A data-bag item without `id` warns and is skipped. A node environment not in `environment_names` warns once per node and the node is kept.

With `skip_orphan_clients`, call `filter_orphan_clients` after the node crawl: collect node names, drop clients that have none, always keep `chef-validator` and `chef-webui`, and drop the corresponding authz entity, key and client-authz entry so the later policy does not reference them.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p microwave-migrate --test goiardi_test`
Expected: PASS.

```bash
git commit -am "feat(migrate): crawl goiardi nodes, roles, environments and data bags"
```

---

### Task 5: Cookbooks, checksums and blobs

**Files:**
- Modify: `crates/microwave-migrate/src/goiardi.rs`
- Test: `crates/microwave-migrate/tests/goiardi_test.rs`

**Interfaces:**
- Consumes: `crawl::store_blob`, `MAX_BLOB_BYTES`/`MAX_TOTAL_BLOB_BYTES` (move those constants to `crawl.rs`).
- Produces: `fn crawl_cookbooks(&mut self, org: &OrgRow) -> Result<(), MigrateError>`, `fn push_file(&mut self, org: &OrgRow, version_id: &str, checksum: &str, url: Option<&str>)`, field `pending_blobs: HashMap<(String, String), String>`.
- Reuses `cookbook_files` from `crawl.rs` (moved there in Task 1) and `microwave_store::parse_version`.

- [ ] **Step 1: Write the failing tests**

```rust
fn cookbooks_routes(with_blob: bool) -> (Vec<(&'static str, Route)>, String) {
    let (body, checksum, path) = blob_fixture("blob");
    let cookbook = json!({
        "name": "app",
        "versions": [{"version": "1.2.3", "url": "/cookbooks/app/1.2.3"}],
    });
    let version = json!({
        "name": "app-1.2.3",
        "cookbook": "app",
        "version": "1.2.3",
        "frozen?": false,
        "metadata": {"name": "app", "version": "1.2.3"},
        "recipes": [{"name": "default.rb", "path": "recipes/default.rb", "checksum": checksum}],
    });
    let mut routes = vec![
        ("/cookbooks", json_route(json!({"app": "/cookbooks/app"}))),
        ("/cookbooks/app", json_route(cookbook)),
        ("/cookbooks/app/1.2.3", json_route(version)),
    ];
    if with_blob {
        routes.push((path, bytes_route(200, body)));
    }
    (routes, checksum)
}

#[test]
fn derives_a_missing_blob_url_from_base_and_copies_the_blob() {
    let out = tempdir();
    let bookshelf = tempdir();
    let (routes, checksum) = cookbooks_routes(true);
    let (url, _) = start_server(routes);
    let options = GoiardiOptions {
        bookshelf_dir: Some(bookshelf.path().to_path_buf()),
        ..options(&url, out.path())
    };
    let (snapshot, _) = crawl_from_goiardi(&options).unwrap();
    let org_id = &snapshot.data.orgs[0].id;
    assert!(bookshelf
        .path()
        .join(format!("organization-{org_id}/checksum-{checksum}"))
        .exists());
}

#[test]
fn a_checksum_without_a_blob_is_an_error_when_blobs_are_requested() {
    let out = tempdir();
    let bookshelf = tempdir();
    let (routes, _) = cookbooks_routes(false);
    let (url, _) = start_server(routes);
    let options = GoiardiOptions {
        bookshelf_dir: Some(bookshelf.path().to_path_buf()),
        ..options(&url, out.path())
    };
    assert!(crawl_from_goiardi(&options).is_err());
}

#[test]
fn dry_run_writes_no_snapshot_and_fetches_no_blob() {
    let dir = tempdir();
    let output = dir.path().join("snapshot.json");
    let (routes, _) = cookbooks_routes(true);
    let (url, records) = start_server(routes);
    let mut options = options(&url, dir.path());
    options.dry_run = true;
    let summary = from_goiardi_with_options(&options).unwrap();
    assert_eq!(summary.checksums, 1);
    assert!(!output.exists());
    assert!(records
        .lock()
        .unwrap()
        .iter()
        .all(|record| !record.path.starts_with("/file_store/")));
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p microwave-migrate --test goiardi_test derives_a_missing_blob_url`
Expected: FAIL.

- [ ] **Step 3: Implement**

Parse versions from the cookbook list entry (the same `cookbook_versions` helper shape as `from_api`). Reuse `push_cookbook_version` semantics: `parse_version`, `frozen?`, metadata columns, raw body, `ChecksumRow` and `CookbookVersionChecksumRow` per entry in every segment. `push_file` takes the `(checksum, url)` pairs from `cookbook_files`: dedupe the `ChecksumRow` through a seen set, always add the version link, and when a bookshelf is configured record the reference as `url` or, when absent, as `/file_store/<checksum>` (resolved against the base by `store_blob`).

Skip blob work in `dry_run`. After the crawl, sort pending blobs, skip files already present, and write the rest through `crawl::store_blob`. The "requested checksum without a blob" rule only applies when blobs were fetched, because `--dry-run` downloads nothing.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p microwave-migrate --test goiardi_test`
Expected: PASS.

```bash
git commit -am "feat(migrate): crawl goiardi cookbooks and copy blobs"
```

---

### Task 6: Default organization policy and ACLs

**Files:**
- Modify: `crates/microwave-migrate/src/goiardi.rs`
- Modify: `crates/microwave-migrate/src/lib.rs` (re-export `apply_default_policy`)
- Modify: `crates/microwave-migrate/Cargo.toml` (`microwave-authz` dev-dependency)
- Test: `crates/microwave-migrate/tests/goiardi_policy_test.rs`

**Interfaces:**
- Consumes: `object_authz`, user and client authz maps from Tasks 3–4.
- Produces: `pub fn apply_default_policy(data: &mut StoreSnapshot, org: &OrgRow, admin_authz: &str, client_authz: &[String], object_authz: &[(String, String)], generated_at: &str)` and private `fn copy_container_acl(data: &mut StoreSnapshot, container: &str, target: &str, creator: &str)`.

- [ ] **Step 1: Write the comparison test**

```rust
use microwave_authz::create_organization;
use microwave_migrate::apply_default_policy;
use microwave_store::{OrgRow, SqliteStore, StoreSnapshot};

const NOW: &str = "2026-11-02T00:00:00Z";

#[test]
fn builder_matches_create_organization() {
    let dir = tempdir();
    let store = SqliteStore::open(&dir.path().join("policy.db")).unwrap();
    let creator = store
        .create_user("admin", "ops@example.com", "bcrypt", "", "", true)
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let org = runtime
        .block_on(create_organization(
            &store, "acme", "Acme", &creator.id, &creator.authz_id,
        ))
        .unwrap();

    // The reference is the real server policy, canonicalized from uuids to names.
    let reference = canonical_grants(&store.export_snapshot().unwrap());

    let mut built = StoreSnapshot::default();
    built.orgs.push(OrgRow {
        id: org.id.clone(),
        authz_id: org.authz_id.clone(),
        name: "acme".to_string(),
        full_name: "Acme".to_string(),
        created_at: NOW.to_string(),
        updated_at: NOW.to_string(),
        ..Default::default()
    });
    apply_default_policy(&mut built, &built.orgs[0].clone(), &creator.authz_id, &[], &[], NOW);

    assert_eq!(canonical_grants(&built), reference);
    assert_eq!(built.containers.len(), 12);
    assert_eq!(built.groups.len(), 5);
    assert_eq!(built.environments.len(), 1);
    assert!(built.environments[0].name == "_default");
    // No object rows in this fixture, so no creator object grants yet.
    assert!(built.authz_acl_actor.iter().all(|row| {
        !built.containers.iter().any(|container| container.authz_id == row.target_authz_id)
            || row.actor_authz_id == creator.authz_id
    }));
}

#[test]
fn object_acls_copy_the_container_acl_and_add_the_creator() {
    // One node in object_authz; assert the node carries a copy of every nodes
    // container ACL row plus Grant/Read/Update/Delete/Create for the creator,
    // and no other container's rows.
}
```

`canonical_grants` maps every `target_authz_id`, `actor_authz_id` and `group_authz_id` to its row name and returns a sorted set of `(target, permission, kind, member)`.

- [ ] **Step 2: Run to fail**

Run: `cargo test -p microwave-migrate --test goiardi_policy_test`
Expected: FAIL.

- [ ] **Step 3: Implement**

Mirror `crates/microwave-authz/src/org.rs` in the same order:

1. 12 containers and 5 groups from `DEFAULT_CONTAINERS` / `DEFAULT_GROUPS`, plus `GlobalGroupRow`s `server-admins` and `<org>_read_access_group`.
2. `OrgUserAssociationRow` for `admin`; group members `admin`→`admins`/`users` and every client→`clients`; group children `read-access`→`admins`/`users`, `public_key_read_access`→`users`/`clients`.
3. Creator (`admin`) ALL on every container, group and the org; `admins` ALL on every container, on `admins`/`clients`/`users`, and on the org; `read-access` creator ALL; `public_key_read_access` `Read`+`Update` for `admins`.
4. The `users` grants exactly as in `org.rs`, including the container `Read` pair and org `Read`.
5. The `clients` grants exactly as in `org.rs` (`nodes` `Read`+`Create`, `Read` on the seven readable containers).
6. The `billing-admins` pair.
7. Only then the `_default` environment with the `org.rs` body, its own creator/admins/users/clients grants.
8. For every `(container, authz)` in `object_authz`: copy that container's ACL rows onto the object and add creator `ALL`.

Ids come from `synthetic_id("authz:…")`; `generated_at` fills every timestamp and `last_updated_by` is `admin`.

`crawl_from_goiardi` calls `apply_default_policy` after the object crawls and before the blob copy, so the object ACLs are in place when Task 7 validates and the snapshot is written.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p microwave-migrate --test goiardi_policy_test`
Expected: PASS.

```bash
git add crates/microwave-migrate/Cargo.toml crates/microwave-migrate/src/goiardi.rs crates/microwave-migrate/src/lib.rs crates/microwave-migrate/tests/goiardi_policy_test.rs
git commit -m "feat(migrate): synthesize the default org policy for goiardi imports"
```

---

### Task 7: Summary output and snapshot validation

**Files:**
- Modify: `crates/microwave-migrate/src/goiardi.rs`
- Modify: `crates/microwave-migrate/src/lib.rs` (re-export `validate_snapshot`)
- Test: `crates/microwave-migrate/tests/goiardi_test.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: `pub fn validate_snapshot(snapshot: &Snapshot, bookshelf_dir: Option<&Path>) -> Result<(), MigrateError>`.

- [ ] **Step 1: Write the failing tests**

```rust
use microwave_migrate::validate_snapshot;

#[test]
fn an_object_without_acl_is_rejected() {
    let dir = tempdir();
    let (url, _) = start_server(crawl_objects_routes());
    let (mut snapshot, _) = crawl_from_goiardi(&options(&url, dir.path())).unwrap();
    let node_authz = snapshot.data.nodes[0].authz_id.clone();
    snapshot
        .data
        .authz_acl_actor
        .retain(|row| row.target_authz_id != node_authz);
    snapshot
        .data
        .authz_acl_group
        .retain(|row| row.target_authz_id != node_authz);
    let error = validate_snapshot(&snapshot, None).unwrap_err();
    assert!(error.to_string().contains("web1"));
}

#[test]
fn dry_run_counts_checksums_but_writes_nothing() {
    let dir = tempdir();
    let output = dir.path().join("snapshot.json");
    let (routes, _) = cookbooks_routes(false);
    let (url, _) = start_server(routes);
    let mut options = options(&url, dir.path());
    options.dry_run = true;
    let summary = from_goiardi_with_options(&options).unwrap();
    assert_eq!(summary.checksums, 1);
    assert!(!output.exists());
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p microwave-migrate --test goiardi_test an_object_without_acl`
Expected: FAIL.

- [ ] **Step 3: Implement**

Validation: every `authz_entities` row of type `object` has at least one `authz_acl_actor` or `authz_acl_group` row for its authz id. When blobs were fetched, every pending checksum exists under `bookshelf_dir`, and a requested checksum with no blob is the same hard error. Print the count table and each warning to stderr; the snapshot path goes to stdout.

`dry_run` prints and returns without writing, without fetching blobs and without the blob-existence check.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p microwave-migrate --test goiardi_test`
Expected: PASS.

```bash
git add crates/microwave-migrate/src/goiardi.rs crates/microwave-migrate/src/lib.rs
git commit -m "feat(migrate): print the goiardi summary and validate snapshots"
```

---

### Task 8: CLI subcommand

**Files:**
- Modify: `bin/microwave-migrate/src/main.rs`
- Test: `bin/microwave-migrate/tests/cli_test.rs`

**Interfaces:**
- Consumes: `from_goiardi_with_options`, `GoiardiOptions`.
- Produces: `Command::FromGoiardi` with `--url`, `--client-name`, `--key`, `--org`, `--output`, `--org-full-name`, `--bookshelf-dir`, `--socks5`, `--skip-orphan-clients`, `--dry-run`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn from_goiardi_requires_url_key_org_and_output() {
    let output = run(&["from-goiardi"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--url"));
}

#[test]
fn from_goiardi_rejects_bad_org_and_socks() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("admin.pem");
    let snapshot = dir.path().join("snapshot.json");
    let key = key.to_str().unwrap();
    let snapshot = snapshot.to_str().unwrap();
    std::fs::write(key, PRIVATE_KEY).unwrap();
    let common = [
        "--url",
        "http://127.0.0.1:1",
        "--client-name",
        "admin",
        "--key",
        key,
        "--output",
        snapshot,
    ];

    let mut bad_org = common.to_vec();
    bad_org.extend(["--org", "Acme_1"]);
    let output = run(&bad_org);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("invalid organization"));

    let mut bad_socks = common.to_vec();
    bad_socks.extend(["--org", "acme", "--socks5", "localhost"]);
    let output = run(&bad_socks);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--socks5"));
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p microwave-migrate --bin microwave-migrate --test cli_test from_goiardi`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add the subcommand, build `GoiardiOptions`, call the crawler, print `Wrote goiardi snapshot to <path>` plus the blob counts on success. `--org` and `--socks5` validate through the crawler and `Fetcher::new`, not clap, so each error stays one message. `--socks5` is `value_name = "HOST:PORT"`.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p microwave-migrate --bin microwave-migrate --test cli_test`
Expected: PASS.

```bash
git add bin/microwave-migrate/src/main.rs bin/microwave-migrate/tests/cli_test.rs
git commit -m "feat(migrate): add the from-goiardi subcommand"
```

---

### Task 9: Documentation and claims

**Files:**
- Modify: `docs/deployment.md` (new subsection under § 11)
- Modify: `crates/microwave-migrate/README.md`
- Modify: `README.md` (migration paragraph)
- Modify: `docs/claims.toml`
- Modify: `docs/remaining-work.md` (rendered)

- [ ] **Step 1: Write the deployment section**

Cover: fresh empty target and the superuser check, the command example, `--socks5` and the 403 symptom, `--bookshelf-dir`, the password reset, the missing password hashes, `reindex`, and that goiardi snapshots do not merge. Placeholders only.

- [ ] **Step 2: Add the claim**

```toml
[[claim]]
id = "gap-goiardi-password-hashes"
summary = "The goiardi importer cannot carry password hashes; the imported admin needs a reset."
area = "migration"
status = "gap"
surfaces = [
  { path = "docs/deployment.md", needle = "from-goiardi" },
  { path = "crates/microwave-migrate/README.md", needle = "from-goiardi" },
]
verify = { kind = "grep", path = "docs/deployment.md", pattern = "from-goiardi" }
```

- [ ] **Step 3: Render and verify**

Run: `python3 scripts/render-remaining-work.py && python3 scripts/check-claims.py`
Expected: no stale or missing-surface errors.

- [ ] **Step 4: Commit**

```bash
git add docs/deployment.md docs/claims.toml docs/remaining-work.md README.md crates/microwave-migrate/README.md
git commit -m "docs(migrate): document the goiardi importer"
```

---

### Task 10: End-to-end import test and full gate

**Files:**
- Test: `crates/microwave-migrate/tests/goiardi_import_test.rs` (`mod common;`)

**Interfaces:**
- Consumes: `from_goiardi_with_options`, `import`, `SqliteStore`.

- [ ] **Step 1: Write the test**

Add `full_goiardi_routes()` to `tests/common/mod.rs`: the union of the fixture routes already used by `goiardi_test.rs` (one user `admin`, one client `web1`, one node `web1`, one role, one environment, one bag with one item and one cookbook version whose file has a checksum and no `url`), plus the `/file_store/<checksum>` blob. It returns `(routes, checksum)`.

```rust
use common::{full_goiardi_routes, options, start_server};
use microwave_migrate::import;
use microwave_store::{ActorType, Permission, SqliteStore};

#[test]
fn import_roundtrip_keeps_keys_acls_and_blobs_and_refuses_a_second_import() {
    let (routes, checksum) = full_goiardi_routes();
    let (url, records) = start_server(routes);
    let data = tempfile::tempdir().unwrap();
    let snapshot = data.path().join("snapshot.json");
    let bookshelf = data.path().join("bookshelf");
    let mut options = options(&url, data.path());
    options.bookshelf_dir = Some(bookshelf.clone());
    from_goiardi_with_options(&options).unwrap();
    import(data.path(), &snapshot).unwrap();

    let store = SqliteStore::open(&data.path().join("microwave.db")).unwrap();
    let org = store.find_org("acme").unwrap().unwrap();
    let admin = store.find_user("admin").unwrap().unwrap();
    let client = store.find_client(&org.id, "web1").unwrap().unwrap();
    assert!(store
        .find_actor_key(&client.id, ActorType::Client, "default")
        .unwrap()
        .is_some());
    assert!(store
        .check_permission(&admin.authz_id, &org.authz_id, Permission::Grant)
        .unwrap());
    let nodes = store.find_container(&org.id, "nodes").unwrap().unwrap();
    assert!(store
        .check_permission(&client.authz_id, &nodes.authz_id, Permission::Read)
        .unwrap());
    assert!(store.find_node(&org.id, "web1").unwrap().is_some());
    assert!(bookshelf
        .join(format!("organization-{}/checksum-{checksum}", org.id))
        .exists());

    let error = import(data.path(), &snapshot).unwrap_err();
    assert!(matches!(error, MigrateError::TargetNotEmpty { .. }));
    assert!(records.lock().unwrap().iter().all(|record| record.method == "GET"));
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p microwave-migrate --test goiardi_import_test`
Expected: FAIL until the previous tasks are in.

- [ ] **Step 3: Make it pass**

Adjust only the test harness or a real gap the test exposes.

- [ ] **Step 4: Full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets && cargo test --workspace`
Expected: clean, green.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-migrate/tests/goiardi_import_test.rs
git commit -m "test(migrate): cover the goiardi import roundtrip"
```

---

## Execution notes

- `from_api_test.rs` is the regression guard for Task 1; do not edit it there.
- The real source is only reachable through a tunnel. Capture one response per
  collection (`/users`, `/clients`, `/nodes`, `/roles`, `/environments`,
  `/data`, `/data/<bag>`, `/cookbooks`, `/cookbooks/<name>`, a version manifest
  and a `/file_store/<checksum>` blob) before Task 3, scrub the host name and
  keep them under `crates/microwave-migrate/tests/fixtures/goiardi/`. The
  harness in `goiardi_test.rs` serves exactly those shapes; if a captured
  shape contradicts the plan, fix the plan's mapping, not the fixture.
- Missing collections answer 404 and `get_optional_raw` already treats that as
  absent, so a test may only serve the routes it exercises.
- `MICROWAVE_TEST_POSTGRES_URL` gates the Postgres preflight test exactly like
  the existing PostgreSQL suites.
