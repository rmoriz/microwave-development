# Converge load test — implementation plan

**Spec:** `docs/superpowers/specs/2026-10-30-converge-load-test-design.md`
**Date:** 2026-10-30

## Conventions for every task

- TDD: write the failing test first, run it, watch it fail for the stated
  reason, then implement.
- One commit per task, imperative subject with a scope.
- Run the task's own test command before committing. `cargo fmt --all` before
  every Rust commit. Full `cargo test --workspace` before the last Rust task.
- Never leave an unused parameter or a `#[allow]` behind; the workspace runs
  `clippy --all-targets -- -D warnings`.
- Paths are workspace-relative. `crates/…` means `microwave-…`.

## Task 1 — `database.pool_size` setting

**Goal:** an operator setting, default `10`, `0` rejected, overridable by
`MW_DATABASE__POOL_SIZE`, reported by `check-config`, documented.

**Files**

- `crates/microwave-config/src/config.rs`
- `crates/microwave-config/tests/config_test.rs`
- `crates/microwave-ctl/src/diagnostics.rs`
- `packaging/microwave.toml.example`
- `docs/deployment.md`

**Steps**

1. Append to the env-override doc list on `Config::apply_env` (line ~186):
   `MW_DATABASE__POOL_SIZE` next to `MW_DATABASE__DRIVER`, `MW_DATABASE__URL`.
2. In `DatabaseConfig` (line ~748) add:

   ```rust
   /// PostgreSQL connection-pool size for this process. Each server process
   /// opens this many connections, so the sum over all processes must stay
   /// below PostgreSQL's `max_connections`.
   pub pool_size: usize,
   ```

   Set `pool_size: 10` in `impl Default for DatabaseConfig`.
3. In `apply_env`, after the `MW_DATABASE__URL` block:

   ```rust
   if let Ok(value) = std::env::var("MW_DATABASE__POOL_SIZE") {
       self.database.pool_size = parse_value("MW_DATABASE__POOL_SIZE", value)?;
   }
   ```
4. In `validate`, next to the `depsolver.max_concurrent == 0` check, reject
   `0` with `key: "database.pool_size"`.
5. `crates/microwave-ctl/src/diagnostics.rs`: in the `check_config` Postgres
   arm push `format!("database.pool_size {}", config.database.pool_size)`, and
   in the SQLite arm too (the value is inert but resolved). Add a zero check
   with message `database.pool_size must be greater than zero`; `validate()`
   already covers it, so the CLI check only guards configs loaded before
   validation.
6. `packaging/microwave.toml.example`: under `[database]` add a commented
   `# pool_size = 10` with the multiply-against-`max_connections` warning.
7. `docs/deployment.md` §"Paths, database and storage": add the settings row
   for `database.pool_size` / `MW_DATABASE__POOL_SIZE` (default `10`, each
   process opens that many connections), and replace the "Microwave does not
   expose a connection-pool size" sentence in the legacy-mapping table with
   `database.pool_size`.

**Tests** (`crates/microwave-config/tests/config_test.rs`, follow
`search_backend_and_url_can_be_overridden_from_env`)

- default is `10`;
- TOML `[database] pool_size = 32` parses;
- `MW_DATABASE__POOL_SIZE=7` overrides TOML;
- `MW_DATABASE__POOL_SIZE=0` fails `apply_env`;
- `pool_size = 0` in TOML fails `validate`.

**Verify:** `cargo test -p microwave-config` then
`cargo test -p microwave-ctl-bin --test cli_test check_config`.

**Commit:** `feat(config): add a database connection-pool size setting`

## Task 2 — carry the pool size into the PostgreSQL store

**Goal:** the server and `microwave-ctl` open the pool with the configured
size; existing callers keep `10`.

**Files**

- `crates/microwave-store-postgres/src/store.rs`
- `crates/microwave-store/src/backend.rs`
- `crates/microwave-store/src/lib.rs`
- `crates/microwave-store-postgres/src/health.rs`
- `crates/microwave-store-postgres/tests/postgres_integration.rs`
- `bin/microwave-server/src/main.rs`
- `crates/microwave-ctl/src/context.rs`

**Steps**

1. In `crates/microwave-store/src/backend.rs` add above `HealthRepository`:

   ```rust
   /// Live connection-pool counters, when the backend has a pool.
   #[derive(Debug, Clone, Copy, PartialEq, Eq)]
   pub struct PoolStats {
       pub idle: u32,
       pub in_use: u32,
       pub max: u32,
   }
   ```

   and to `HealthRepository`:

   ```rust
   /// Pool occupancy for operator gauges. Backends without a pool return
   /// `None`; the default keeps them from implementing anything.
   async fn pool_stats(&self) -> Option<PoolStats> {
       None
   }
   ```
2. Re-export `PoolStats` from `crates/microwave-store/src/lib.rs`.
3. `crates/microwave-store-postgres/src/health.rs`: implement `pool_stats`
   from `self.pool()` — `options().get_max_connections()`, `num_idle()`,
   `size()`, with `in_use = size.saturating_sub(idle)` and
   `u32::try_from(..).unwrap_or(u32::MAX)`.
4. Add `pub async fn connect_with_compression_and_pool(database_url: &str,
   compression: Compression, pool_size: usize)` to `PostgresStore`; move the
   body of `connect_with_compression` there with
   `.max_connections(pool_size)`. Make `connect_with_compression` forward
   `DEFAULT_POOL_SIZE` where `const DEFAULT_POOL_SIZE: usize = 10;`. Do the
   same split for `connect_existing_with_compression`
   (`connect_existing_with_compression_and_pool`) so the server-start variants
   and the ctl "no migrate" variant both honour the setting.
5. `bin/microwave-server/src/main.rs:124`: call
   `connect_with_compression_and_pool(&database_url, compression,
   config.database.pool_size)`.
6. `crates/microwave-ctl/src/context.rs:98` and `:101`: pass
   `self.config.database.pool_size`.
7. Leave `crates/microwave-migrate` on the default and note in the module doc
   of `crates/microwave-migrate/src/lib.rs` that the migrator keeps the
   default pool size.

**Tests** (`postgres_integration.rs`, skip-when-unset like its neighbours)

- `pool_stats_reports_the_configured_maximum`: connect with pool size `3`,
  assert `pool_stats().unwrap().max == 3`;
- `connect_with_compression_still_uses_ten`: assert `max == 10` from the
  compatibility wrapper.

**Verify:** `cargo test -p microwave-store-postgres` (with
`MICROWAVE_TEST_POSTGRES_URL`) and `cargo test --workspace` for the wiring.

**Commit:** `feat(store): open the PostgreSQL pool with the configured size`

## Task 3 — pool gauges on `/_stats`

**Goal:** a pool-bound run is distinguishable from a slow store.

**Files**

- `crates/microwave-api/src/lib.rs`
- `crates/microwave-api/tests/ops_test.rs`

**Steps**

1. In `crates/microwave-api/src/lib.rs`, after `let snapshot =
   state.metrics.snapshot();` add `let pool = state.store.pool_stats().await;`.
   In the text branch, when `Some`, emit

   ```
   # HELP microwave_server_db_pool_connections Database pool connections by state.
   # TYPE microwave_server_db_pool_connections gauge
   microwave_server_db_pool_connections{state="idle"} N
   microwave_server_db_pool_connections{state="in_use"} N
   # HELP microwave_server_db_pool_connections_max Configured pool size.
   # TYPE microwave_server_db_pool_connections_max gauge
   microwave_server_db_pool_connections_max N
   ```

   In the JSON branch add `"db_pool": {"idle": .., "in_use": .., "max": ..}`
   only when `Some`.
2. Keep the existing metric order; append the pool block after
   `microwave_server_nodes`.

**Tests**

- `ops_test.rs::stats_renders_prometheus_text`: the SQLite app has no pool, so
  assert the body does **not** contain `microwave_server_db_pool_connections`.
- `ops_test.rs::stats_json_omits_pool_without_a_pool`: assert
  `body.get("db_pool").is_none()`.
- Task 2 already covers the numbers.

**Verify:** `cargo test -p microwave-api --test ops_test --test
status_contract_test`

**Commit:** `feat(api): report PostgreSQL pool gauges on /_stats`

## Task 4 — derive cookbook dependencies on write

**Goal:** `cookbook_world` stops parsing manifests.

**Files**

- `crates/microwave-store/src/derivations.rs` (new) and `lib.rs`
- `crates/microwave-store/src/cookbooks.rs`
- `crates/microwave-store/src/migration.rs`
- `crates/microwave-store-postgres/src/cookbooks.rs`
- `crates/microwave-store/tests/` (SQLite-side test)
- `crates/microwave-api/src/depsolver.rs`

**Steps**

1. New module with one lenient helper — it re-serializes and never validates,
   because both write paths validate first:

   ```rust
   /// Compact JSON of `metadata.dependencies`, or `None` when the manifest has
   /// none or one entry is not a string.
   pub fn cookbook_dependencies(manifest: &str) -> Option<String>
   ```

   Return `None` for an empty map too.
2. `create_cookbook_version` (both backends): replace the hard-coded `'{}'`
   with a bound parameter fed by `cookbook_dependencies(manifest)`;
   `update_cookbook_version` (both backends): add `meta_deps = ?6`-style to the
   `UPDATE`.
3. `StoredCookbookVersion` gains `pub meta_deps: Option<String>`; extend the
   `SELECT`s in `find_cookbook_version` and `list_cookbook_versions` (both
   backends) with `cv.meta_deps` and fill the field.
4. `migration.rs` (both backends): where snapshot rows are decoded, set
   `meta_deps = cookbook_dependencies(manifest)` when it is empty or `{}`. This
   keeps restored snapshots on the fast path.
5. `crates/microwave-api/src/depsolver.rs`: replace the manifest parse in
   `cookbook_world` with

   ```rust
   fn stored_dependencies(stored: &StoredCookbookVersion) -> Result<Vec<(String, Constraint)>, ApiError>
   ```

   which uses `stored.meta_deps` when it parses to a non-empty map, and falls
   back to the current `/metadata/dependencies` parse when it is `None`, `""`
   or `"{}"`. Keep the existing `ApiError::internal()` on a non-string
   constraint.

**Tests**

- store test: create a version with `metadata.dependencies`, assert
  `find_cookbook_version(..).meta_deps` is `Some("{\"nginx\":\">= 0.0.0\"}")`;
- store test: create a version without dependencies, assert `None`;
- store test: update a version to add a dependency, assert the column changed;
- store test: an old-shaped row written with `meta_deps = '{}'` still resolves
  through the fallback (write the row with raw SQL in a `SqliteStore` test);
- api `depsolver_test.rs`: an environment constraint and a nested dependency
  still solve after a create, proving the stored form is used.

**Verify:** `cargo test -p microwave-store` and `cargo test -p microwave-api
--test depsolver_test --test cookbooks_test`

**Commit:** `feat(store): store cookbook dependencies on write`

## Task 5 — store environment constraints on write

**Goal:** the solver stops parsing environment bodies.

**Files**

- `crates/microwave-store/migrations/0001_initial.sql`,
  `crates/microwave-store/src/sqlite.rs`
- `crates/microwave-store-postgres/migrations/0001_initial.sql`
- `crates/microwave-store/src/objects.rs`,
  `crates/microwave-store-postgres/src/objects.rs`
- `crates/microwave-api/src/depsolver.rs`
- `crates/microwave-api/tests/environments_contract_test.rs`

**Steps**

1. Schema: `cookbook_constraints TEXT` on `environments` in both
   `0001_initial.sql` files; SQLite also gets
   `add_column_if_missing(connection, "environments", "cookbook_constraints",
   "TEXT")` in `migrate()`; PostgreSQL gets
   `ALTER TABLE environments ADD COLUMN IF NOT EXISTS cookbook_constraints
   TEXT;` after the `environments` table definition.
2. `StoredObject` gains `pub cookbook_constraints: Option<String>`.
3. Add `fn constraints_selection(table: &str) -> &'static str` to each backend
   returning `", cookbook_constraints"` for `"environments"` and `", NULL"`
   otherwise, and append it to the `SELECT`s of `find_named_object`,
   `list_named_objects`, `find_named_objects_by_ids`. Read index 6 in
   `stored_object_from_row` / the Postgres equivalents.
4. `create_environment` and `update_environment` (both backends) call
   `self.set_environment_constraints(&object.id, environment_constraints(body))`
   after the named-object call, where the helper is
   `fn set_environment_constraints(&self, id: &str, constraints: Option<String>)`
   running `UPDATE environments SET cookbook_constraints = ? WHERE id = ?`.
   `environment_constraints(body)` is the new lenient helper in
   `derivations.rs` reading `cookbook_versions`; it returns `Option<String>`
   and both callers here handle that directly (no `?`).
5. `depsolver.rs::environment_constraints` takes the stored form first and
   falls back to parsing `serialized_object` when it is `None`, `""` or
   `"{}"`.

Note in a comment on `set_environment_constraints` that the update is a second
statement, so a crash before it leaves `NULL` and the reader falls back.
`cookbook_constraints` deliberately stays out of `NamedObjectRow` and the
snapshot: the value is derivable from `serialized_object`, so a restore leaves
the column `NULL` and the reader parses. Do not thread it through
`create_named_object`, `upsert_named_object`, or the export SQL.

**Tests**

- `environments_contract_test.rs`: create an environment with a
  `cookbook_versions` constraint, `GET` it, then solve and assert the
  constraint applied (a version that violates it is rejected);
- store test: `find_environment` returns the stored JSON;
- store test: `update_environment` replaces the column;
- store test: an environment row with `cookbook_constraints` `NULL` still
  solves through the fallback;
- `crates/microwave-store-postgres/tests/postgres_migration_test.rs`: the
  column exists after migrating a fresh database and after migrating one
  created without it.

**Verify:** `cargo test -p microwave-store`, `cargo test -p microwave-api
--test environments_contract_test --test depsolver_test`, and
`cargo test -p microwave-store-postgres` with the URL set.

**Commit:** `feat(store): keep environment cookbook constraints on write`

## Task 6 — document the two derivations and their cost

**Goal:** operators learn that the solve path no longer parses, and which
fallback still does.

**Files:** `docs/deployment.md` (solver section)

**Steps**

1. In the "Search and dependency solver" prose, add a short paragraph: cookbook
   dependencies and environment constraints are stored at write time; a row
   written by an older release or restored from a snapshot carries `NULL` and
   is parsed until it is written again.
2. Claim in `docs/claims.toml`: `solver-inputs-stored-on-write`, area
   `depsolver`, `verify = { kind = "grep", path = "docs/deployment.md",
   pattern = "stored at write time" }`, surface the same needle.

The load-test guide (Task 13) repeats the point for its own audience; do not
touch `docs/load-test.md` here, it does not exist yet.

**Verify:** `python3 scripts/check-claims.py`

**Commit:** `docs(deployment): describe write-time solver inputs`

## Task 7 — crate skeleton, flags and fixture plan

**Goal:** a `microwave-load` binary with the seed/run/canary shape and a
deterministic fixture, no network yet.

**Files**

- `Cargo.toml` (workspace members)
- `crates/microwave-load/{Cargo.toml,src/lib.rs}`
- `crates/microwave-load/src/{cli.rs,fixture.rs,keys.rs}`
- `bin/microwave-load/{Cargo.toml,src/main.rs}`

**Steps**

1. Follow `bin/microwave-ctl/Cargo.toml`: a `*-bin` package with a `[[bin]]`
   named `microwave-load` depending on `microwave-load`.
2. `cli.rs` with `clap` derive:

   ```
   seed   --server-url --keys --data-dir [--nodes 20000] [--roles 1000]
          [--versions 600] [--targets 5000] [--canary 20] [--shared-key]
          [--postgres-url]
   run    --server-url --keys --out . [--concurrency 5000] [--ramp 0]
          [--canary 20] [--chef-client chef-client] [--upload]
          [--stats-user --stats-pass] [--superuser] [--postgres-url]
          [--fingerprint PATH] [--commit SHA]
   ```

   `seed` opens the store directly — PostgreSQL when `--postgres-url` is set,
   otherwise the SQLite file in `--data-dir` — and applies the snapshot through
   `import_snapshot`; `run` only speaks HTTP.
   `seed --canary N` creates the `canary-<n>` nodes and clients that Task 11
   runs; `run --canary N` must not exceed what the fixture holds.
   `run` takes `--stats-user/--stats-pass` (the `/_stats` credentials the
   harness writes), `--commit` (printed into the report) and `--fingerprint`
   (Task 10).
   `--superuser` makes every driver request sign as the manifest's `superuser`
   entry instead of the node's own client; ACLs are then only exercised by the
   canary, so the report notes the mode.
3. `fixture.rs` builds a `StoreSnapshot` for org `load`:
   - fixed-id rows for org, `nodes`/`environments`/`cookbooks`/`roles`
     containers, and the authz tree;
   - 20,000 `NodeRow`s with serialized bodies naming the three environments in
     rotation, and the first 5,000 each owned by their own client;
   - 1,000 `NamedObjectRow` roles; role `i` gets five recipes from the cookbook
     pool and, for every third role, `role[...]` on another higher-index role
     (depth two);
   - 200 `CookbookRow`s, 3 `CookbookVersionRow`s each with
     `metadata.dependencies` over later cookbooks, `meta_deps` filled by
     `microwave_store::cookbook_dependencies`, plus
     `cookbook_version_checksums`/`checksums` rows;
   - `load-default`, `load-constrained-a` (`load-cb-0001 = 1.0.0`),
     `load-constrained-b` (`load-cb-0002 ~> 1.0`);
   - one `load-clients` group with Read on every cookbook and the sandbox
     container, every target client in it, plus Read+Update on the client's own
     node and Read on the sandbox container;
   - `--canary N` extra `canary-<n>` clients and nodes, each node carrying a
     static run list over `load-cb-0001`..`0005` and the same ACLs, so the
     canary never writes fixture rows itself.
4. `keys.rs` generates one RSA-2048 keypair per target client via
   `microwave_crypto::generate_rsa_keypair`, bounded by
   `std::thread::available_parallelism`, and writes `actor_keys` rows. With
   `--shared-key` one keypair is reused for every client.
5. `seed` writes the manifest to `--keys` with `0600` (mode via
   `PermissionsExt`), one line per entry:
   `<kind> <name> <private-key-path>`, `kind` ∈ `superuser|client|canary`;
   private keys live in `<keys>.d/<name>.pem`.
6. `seed` writes one blob per cookbook version into
   `<data-dir>/bookshelf` via `microwave_objectstore::LocalObjectStore::new(
   root, "load", "")`, with the MD5 of the bytes as the checksum.

**Tests**

- fixture test: two builds of the same sizes are byte-equal after
  `serde_json` of the snapshot;
- fixture test: every target node's owning client has an `authz_acl_actor` row
  with `update` on that node;
- fixture test: `--nodes 10 --roles 4 --versions 6 --targets 4` yields 10
  nodes, 4 roles, 6 versions, 4 keys;
- key test: the manifest is mode `0600` and has one line per target.

**Verify:** `cargo test -p microwave-load`

**Commit:** `feat(load): add the converge load-test fixture and seed`

## Task 8 — signed request client

**Goal:** one signed request with retries, mirroring `chef-client`.

**Files:** `crates/microwave-load/src/{client.rs,retry.rs}`

**Steps**

1. `client.rs`: `struct NodeClient { http: reqwest::Client, server_url:
   String, user: String, key: String, api_version: &'static str }` with

   ```rust
   async fn send(&self, method: Method, path: &str, body: Option<Vec<u8>>) -> Result<Outcome, LoadError>
   ```

   that builds the canonical request with `microwave_crypto::sign_request`
   exactly as `crates/microwave-migrate/src/from_api.rs:1413` does
   (`X-Ops-Sign: algorithm=sha256;version=1.3;`, `X-Ops-Userid`, timestamp,
   content hash, `X-Ops-Server-API-Version: 2`, `X-Ops-Authorization-N` in
   60-byte chunks).
2. One `reqwest::Client` for the whole run, built once with
   `pool_max_idle_per_host` equal to `--concurrency` so keep-alive holds;
   `NodeClient` holds a clone. When the caller passes `--superuser`, the
   key and user come from the manifest's `superuser` entry for every request.
3. Presigned `/bookshelf` URLs are fetched **unsigned** — they carry their own
   token, and their query string is not part of any canonical path.
4. `retry.rs`: retry `503`, `502`, `504`, `429` and transport errors five
   times with `200ms * 2^attempt` plus ±20 % jitter, like
   `http_retry_count 5`; never retry `4xx` other than `429`; record
   `RetryCount` and, for `503` with `Dependency solver overloaded` in the body,
   a `SolverOverloaded` counter.

**Tests**

- retry test with a mock server (an `axum` router in-test): a `503` then
  `200` succeeds with one retry recorded;
- a `503` six times exhausts and returns the last `503`;
- a `412` is not retried;
- a `503` whose body is the solver error increments the solver counter.

**Verify:** `cargo test -p microwave-load`

**Commit:** `feat(load): sign and retry converge requests like chef-client`

## Task 9 — the converge sequence and latency accounting

**Goal:** the real per-node sequence with per-endpoint percentiles.

**Files:** `crates/microwave-load/src/{sequence.rs,metrics.rs}`

**Steps**

1. `sequence.rs`, in `chef-client` order against
   `/organizations/load`: `GET nodes/<node>` (expect `200`), `GET
   environments/<env>`, `POST environments/<env>/cookbook_versions` with
   `{"run_list":[<the node's roles and recipes>]}`, `GET` each `all_files` URL
   in the solve response, `PUT nodes/<node>` with the fetched body plus
   `normal.load_test.iteration += 1`, `POST data-collector` with a minimal
   `run_converge` payload. With `--upload`, also `PUT` one blob through its
   presigned URL.
2. A non-`2xx` after retries fails that node's sequence with the endpoint and
   status; the remaining endpoints are skipped, as `chef-client` does.
3. `metrics.rs`: `struct Endpoint { count: u64, statuses: BTreeMap<u16, u64>,
   retries: u64, solver_503: u64, latencies_us: Vec<u32> }` per endpoint plus
   a per-node outcome count. Percentiles are nearest-rank over the sorted
   micros; parse failures are reported as a status class `0`.
4. Node starts are spread by `--ramp` seconds with a `tokio::sync::Semaphore`
   of `--concurrency`.
5. While the run is in progress, a background task polls `/_stats?format=json`
   every second with the configured Basic credentials and appends the
   `db_pool` object (when present) to a sample vector. The samples go into the
   report as the pool occupancy range; the poll cadence stays fixed.

**Tests**

- sequence test against an in-test router: the request order and methods match
  the list above;
- a `403` on the node read stops the sequence;
- percentile test: `[1,2,3,4,5]` gives p50 3, p95 5, p99 5, max 5;
- solver `503` is counted, not folded into the error count;
- the stats poller records a `db_pool` sample and tolerates a `401` without
  failing the run.

**Verify:** `cargo test -p microwave-load`

**Commit:** `feat(load): run the chef-client converge sequence and record latency`

## Task 10 — fingerprint, report and exit status

**Goal:** a number is never separated from the machine.

**Files:** `crates/microwave-load/src/{fingerprint.rs,report.rs}`,
`crates/microwave-load/src/main.rs`, `crates/microwave-load/src/cli.rs`

**Steps**

1. `fingerprint.rs::collect() -> Fingerprint`: cores and model from
   `/proc/cpuinfo`, `MemTotal` from `/proc/meminfo`, kernel from `uname(2)`
   (`libc`), filesystem and free bytes of `--out`, cgroup CPU/memory limits
   from `/sys/fs/cgroup/{cpu.max,memory.max}` when present, PostgreSQL
   `version()`, `max_connections`, `shared_buffers` from the `--postgres-url`
   pool, and the server commit and resolved pool size from `--commit` and the
   stats poller's `db_pool.max`.
2. `--fingerprint <path>` reads a JSON file written by the wrapper (Compose
   limits, image digests, host kernel/CPU/RAM) and merges it under the
   measured values; unreadable or malformed input is exit `3`.
3. `report.rs::write(out, fingerprint, config_json, results) -> PathBuf`
   writes `load-report-<UTC>.md` (fingerprint and resolved config first, then
   the endpoint table, then canary and node outcomes) plus the sibling
   `load-report-<UTC>.json`. Both use `0o644`.
4. Exit status: `0` when the run completed; `2` when the server was
   unreachable at start or the fixture is missing; `3` for fingerprint/report
   failures. A run with exhausted retries still exits `0` and prints "N nodes
   did not complete" in the report.

**Tests**

- report test: the Markdown contains the core count and `pool_size` above the
  first table row;
- report test: the JSON round-trips and carries the same fingerprint;
- exit-code test per branch `0`/`2`/`3`.

**Verify:** `cargo test -p microwave-load`

**Commit:** `feat(load): write a fingerprint-carrying report`

## Task 11 — canary

**Goal:** a fidelity bug in the driver is visible.

**Files:** `crates/microwave-load/src/canary.rs`

**Steps**

1. Spawn `--canary` `chef-client` processes with
   `--node-name canary-<n> --environment load-default --client-key
   <keys.d/canary-<n>.pem> --server-url <url> --chef-license accept`, bounded to
   4 at a time.
2. `seed` creates the `canary-<n>` nodes and clients (20 by default), gives each
   node a static run list, and lists them in the manifest as `canary` entries,
   so the canary needs no writes of its own.
3. Collect exit status and the last 20 stderr lines; report a separate canary
   line with `ok`/`failed`, and never change the exit status because of it.
4. When the binary is missing and `--canary > 0`, print
   `canary skipped: chef-client not found` and continue.

**Tests**

- canary test with a stub script on `PATH` that exits `0` then `1`, asserting
  one `ok`, one `failed` and exit `0` overall;
- missing-binary test asserts the skip line and exit `0`.

**Verify:** `cargo test -p microwave-load`

**Commit:** `feat(load): add a real chef-client canary`

## Task 12 — the Compose stack

**Goal:** the whole run is one reproducible stack.

**Files**

- `docker-load/docker-compose.yml`
- `docker-load/run.sh`
- `docker-load/load/Dockerfile`
- `docker-load/microwave/microwave.toml`
- `docker-load/README.md`

**Steps**

1. Services, in order: `postgres` (pinned image, named volume, healthcheck,
   `max_connections` from `MW_PG_MAX_CONNECTIONS` default `200`,
   `shared_buffers` from `MW_PG_SHARED_BUFFERS` default `256MB`), `seed`
   (load image, `depends_on` postgres healthy, runs `microwave-load seed`),
   `microwave` (repository `Dockerfile`, `depends_on` seed complete,
   healthcheck on `/_status`, `bind = "microwave:8000"`,
   `data_collector.store = local`, `auth.superuser_name = "load-superuser"`,
   `database.pool_size` from `MW_POOL_SIZE` default `10`,
   `depsolver.max_concurrent = 4`), `load` (`run`, `depends_on` microwave
   healthy, `profiles: [run]`).
   `docker-load/microwave/microwave.toml` is the configuration the harness
   measures and the only place these values live; it is also the file a
   bare-metal operator copies.
2. `deploy.resources.limits.cpus` and `mem_limit` on `microwave` and `load`
   from `MW_SERVER_CPUS`/`MW_SERVER_MEM`/`MW_LOAD_CPUS`/`MW_LOAD_MEM`, default
   `2`/`4g` and `4`/`4g`.
3. `load/Dockerfile` is `debian:trixie-slim` plus `chef-client` (`Cinc`), the
   built `microwave-load`, `curl`; one target, four after touching it — verify
   with the compose build, not `docker build`.
4. `run.sh`: `docker compose build`, write `host-fingerprint.json` into a temp
   dir (cores, model, RAM, kernel, digests from
   `docker compose images --format`, the resolved limits), `docker compose run
   --rm load run --fingerprint /out/host-fingerprint.json --out /out --commit
   $(git rev-parse --short HEAD) …`, then print the report path and
   `docker compose down -v --rmi local` — always, also on failure (`trap`).
5. `smoke` profile shrinks the fixture to `--nodes 500 --roles 50 --versions
   60 --targets 200 --concurrency 200 --canary 2`.
6. `README.md`: one paragraph plus the two commands
   (`docker-load/run.sh`, `docker-load/run.sh smoke`), Linux host recommended,
   PostgreSQL only, no numbers.

**Verify:** `cd docker-load && docker compose config` and
`docker compose --profile smoke run --rm load seed --help`; then one smoke run
by hand, tearing the stack down in the same step (AGENTS rule). Note the run
duration in the commit body.

**Commit:** `feat(docker-load): package the load test as a Compose stack`

## Task 13 — `docs/load-test.md`

**Goal:** an operator can run it and read it.

**Files:** `docs/load-test.md`, `docs/index.md`, `website/docs/sidebars.ts`,
`website/docs/docusaurus.config.ts`, `docs/claims.toml`, `AGENTS.md`

**Steps**

1. The guide: what it measures and what it does not (bridge network, cgroup
   limits, one host, PostgreSQL), prerequisites (Linux host, Docker, ~16 GB),
   the two commands, the flags table, how to read the fingerprint and why it is
   above the results, what to compare between two runs (same fingerprint, one
   variable), the default-against-raised `MW_POOL_SIZE` procedure, the canary's
   meaning, `--superuser`'s caveat, and the `depsolver.max_concurrent = 4`
   `503` noise.
2. Link it from `docs/index.md`; add `load-test.md` to the Docusaurus
   `include` list and a sidebar entry under "Operator guides".
3. Claims, each verified against a sentence the guide states verbatim:
   `load-test-postgres-only` (guide line `This stack always uses PostgreSQL.`),
   `load-test-no-gate` (guide line `The harness is not a CI gate.`),
   `load-test-canary-not-a-gate` (guide line
   `A canary failure is reported but never fails the run.`).
4. `AGENTS.md`: add a `docker-load` bullet to the docker section — always
   build/tear down through `docker-load/docker-compose.yml`, always
   `down -v --rmi local`.

**Verify:** `cd website/docs && npm run build` and
`python3 scripts/check-claims.py`.

**Commit:** `docs(load-test): document running and reading the harness`

## Final check

- `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cd website/docs && npm run build`
- `python3 scripts/check-claims.py`
- `docker compose -f docker-load/docker-compose.yml config`
- `git status` clean of throwaway images/containers and temp files.

## Risks

- **`/_stats` shape** is asserted by `status_contract_test.rs` and the parity
  fixture; adding keys is additive, so only the SQLite absence assertions need
  care.
- **`StoredObject` field addition** touches both backends and every literal
  construction; the compiler finds them.
- **A second statement for constraints** is not transactional with the insert.
  `NULL` means the reader parses, so the failure mode is cost, not
  correctness.
- **`check_permission` per package** is five authorization queries per solve.
  That stays in both modes and is part of what the number describes.
- **RSA generation for 5,000 keys** dominates seed time. `--shared-key` is the
  escape hatch and the report must not be compared across the two modes.
