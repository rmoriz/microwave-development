# Per-organization object store buckets

## Intent

Offerings that host several tenants want a bucket per tenant; internal
multi-org installations want one shared bucket. Today the server knows exactly
one bucket for everybody. The choice must become a per-organization option.

The server stays on plain S3 primitives. Provisioning, quotas and lifecycle are
an external tool's job, so nothing here depends on Garage.

## Goals

- A per-organization entry may name its own bucket, with optional `endpoint`,
  `region` and `path_style`; anything omitted is inherited from the global
  `[object_store]` block.
- Per-org credentials live in the metadata secrets table, not in the config.
- The global bucket stays the default for every organization without an entry.
- The object key layout stays `organization-<org>/checksum-<md5>`, inside every
  bucket.
- The organisation id is still the only handle the store layer receives.
- The map, and newly created organizations with it, apply without a restart,
  via SIGHUP and `microwave-ctl reload`.
- An opt-in strict mode forbids the shared bucket: every organization must
  then have its own entry and its own credentials, and a dedicated
  `health_bucket` carries the status probe.

## Non-goals

- No bucket creation, deletion or quota setting by the server.
- No Garage admin API, no provider-specific calls.
- No per-org buckets for the `local` backend.
- No general config reload. The reload applies the `[object_store]` block
  (including its global key and backend), the organization map and the org
  set; everything else still needs a restart.
- No per-org presign expiry, and no per-org `secret`.

## Anti-goals

- An incomplete org entry silently falling back to the shared bucket, so a
  tenant's blobs land in the wrong place.
- A second, parallel config path next to `[object_store]`.
- Different key layouts per bucket, which would make moving an org a rewrite.

## Constraints

- `ObjectStore` methods receive the organization *id*, the config is written
  with organization *names*. Resolution therefore goes through a snapshot of
  `id -> name` refreshed on every reload. The request path stays a pure
  in-memory lookup because the store is async and `ObjectStore` is sync.
- `TimedObjectStore` wraps the new router, not the other way round, so the
  existing histogram keeps every call.
- `/_status` keeps probing one bucket only; per-org buckets do not multiply
  the live health check.
- The config snapshot that carries the map lives in two artifacts:
  `backup --config-only` writes `microwave-config.json` and must reproduce it
  for a restore, while `gather-logs` writes redacted TOML (`dump.rs`) and must
  emit `[object_store]` scalars before the
  `[object_store.organizations."<org>"]` sub-tables, because a parent key after
  a sub-table is invalid TOML. JSON has no ordering problem.
- The `MW_*` environment keeps covering `[object_store]`, but not the
  organization map as individual keys: org names may contain `_`, which
  collides with the `__` nesting separator. The map comes from the file, with
  one JSON escape hatch for container deployments (see Configuration).
- SIGHUP is already installed before database bootstrap and today reloads TLS
  only (`spawn_sighup_handler` in `bin/microwave-server/src/main.rs`); the unit
  already carries `ExecReload=/bin/kill -HUP $MAINPID`. The tenant reloader
  joins that handler instead of adding a second one.
- `AppState.config` is an `Arc<Config>` read live by every handler. Making it
  swappable is its own project, which is why the reloader rebuilds exactly the
  part it owns.
- `microwave-ctl` talks to the service through systemd, not HTTP. The reload
  command uses `systemctl reload` and learns the outcome from a status file,
  because the signal itself cannot report a validation failure.
- S3 bucket names allow lowercase letters, digits, `-` and `.` only, 3–63
  characters, no leading or trailing `-`/`.`; org names additionally allow
  `_`. Bucket names need their own validation, not the org-name rule.
- `Context::load` runs `Config::validate` on every ctl command, including
  `set-secret`. Table-dependent checks therefore cannot live in
  `Config::validate`, or setting the first credential of an org would trip over
  strict-mode coverage.

## Approaches

A. **Static map plus a routing decorator** (chosen).
   `[object_store.organizations."<org>"]` with a required `bucket` and optional
   connection overrides. A `RoutingObjectStore` picks the per-org store or the
   default one.
B. **Server-managed buckets from a naming policy plus a DB table.** More
   self-service, but needs `CreateBucket` rights, a schema migration, delete
   semantics for non-empty buckets and, for quotas, the provider admin API.
C. **B plus per-org credentials managed from the metadata secrets table.**
   Largest surface, rotation and backup paths included.

A was picked because it is additive, needs no migration and works with any S3
gateway. B and C can follow behind the same interface.

## Design

### Configuration

```toml
[object_store]
backend = "garage"
endpoint = "http://objects.internal:3900"
bucket = "mw-shared"
access_key = "GK-shared"
secret_key = "…"

[object_store.organizations."acme"]
bucket = "mw-acme"
# endpoint, region and path_style are optional per entry
```

`bucket` is required per entry. The other fields are optional and fall back to
the global value field by field. `ObjectStoreConfig.organizations` is a
`BTreeMap<String, ObjectStoreOrgConfig>`.

Per-org credentials are set out of band:

```sh
microwave-ctl set-secret object_store acme.access_key GK-acme
microwave-ctl set-secret object_store acme.secret_key …
```

`set-secret` gains the `<org>.access_key` / `<org>.secret_key` form next to
`object_store.secret`, validated with `valid_org_name` the way the `mcp` group
already validates org names. Outside strict mode an entry without its own
credentials inherits the global ones; inside strict mode they are required.

`MW_OBJECT_STORE__ORGANIZATIONS` accepts the whole map as one JSON object. That
is the container escape hatch and avoids the `_`/`__` ambiguity.

### Strict mode

`object_store.require_org_bucket = true` (default `false`) turns on tenant
mode:

- the global `bucket` must be absent; it is no longer a fallback,
- `health_bucket` is required and is the only status probe target,
- startup fails unless every existing organization has an entry and both
  credentials,
- a reload is rejected unless every organization has an entry and both
  credentials,
- `put`, `get`, `delete` and `metadata` for an unmapped organization return
  `ObjectStoreError::NoOrgBucket`, `exists` returns `false`,
- `presigned_url` returns `Result` so a missing bucket surfaces instead of
  producing a URL into nowhere.

Uniqueness is on `(endpoint, bucket)`, not on the bucket name alone: two
providers may both have an `mw-acme`, but two orgs must not share one target.

The trait change is the broad edit. `presigned_url` currently returns `String`
and must propagate through:

- `add_download_urls` / `annotate_files` (`crates/microwave-api/src/cookbooks.rs`)
  and therefore `render_manifest`,
- `checksum_map` (`crates/microwave-api/src/bookshelf.rs`) and its caller
  `sandboxes.rs`, which builds the JSON inline and needs restructuring,
- every `render_manifest` caller: `cookbooks.rs`, `cookbook_artifacts.rs`,
  `depsolver.rs`, `mcp/tools.rs`.

All callers already use `?` except `sandboxes.rs`. The answers become `500`
instead of a hand-out that fails at the client.

`exists` cannot express the failure: it returns `bool` and swallows backend
errors. `object_metadata` in `bookshelf.rs` must return
`Result<Option<ObjectMetadata>, ObjectStoreError>` and distinguish
`NoOrgBucket` (→ `500`) from `NotFound` (→ `None`). `metadata` already returns
`Result`, and the router answers `NoOrgBucket` before any backend call, so no
trait method and no `AppState` change is needed.

An empty organization table is valid, so a fresh strict install starts. The
first `org-create` needs its entry and credentials immediately after.

Strict mode also means the server key can reach every tenant bucket. Where that
matters, give each tenant its own key; the presigned URLs are signed with it,
so a leaked URL stays inside one bucket.

### Validation

Two layers, because of the `Context::load` constraint.

`Config::validate` (structural, table-independent, runs everywhere):

- the backend is `local` and the map is not empty,
- `require_org_bucket` with the `local` backend,
- `require_org_bucket` with `bucket` set, or without `health_bucket`,
- an entry has an empty or invalid bucket name,
- an invalid `health_bucket`,
- after merging, any of endpoint, access key or secret key is empty,
- two entries share an `(endpoint, bucket)` pair.

`validate_tenant_map(config, org_names, secrets)` (shared by the server and
`microwave-ctl`, runs only on startup, reload and `check-config` /
`diagnostics`):

- `require_org_bucket` and an existing organization is missing its entry,
  `access_key` or `secret_key`,
- an entry name that does not exist in the org table.

The snapshot is rejected as a whole, keeping the running one. An unknown org
name is a warning in the server log and stays inactive, so a deleted tenant
cannot wedge a reload; `microwave-ctl reload` turns it into a non-zero exit and
does not signal. A missing credential is fatal in both.

Bucket reachability is verified for every configured entry on real start and on
reload: one `HEAD <bucket>` per org, the same 1.5 s bound the status probe
uses, at most 8 in flight. A failure rejects the snapshot, so a typo or a wrong
key shows up before traffic moves. The check needs `HeadBucket`/`ListBucket`
rights on the server key, which must be documented. `verify_buckets = false`
turns it off where the request count matters.

`serve --check` and `check-config` stay network-free: they run the structural
checks and the table-dependent ones, but no probes. `diagnostics --probe` adds
them offline.

### Routing

`RoutingObjectStore` holds the default store plus an
`ArcSwap<HashMap<org_id, Arc<dyn ObjectStore>>>` and implements `ObjectStore`
by lookup on `organization_id`. Every method routes, including
`presigned_url`, so a tenant's presigned URL names the tenant's bucket. The
swap guard is held only for the lookup; the call runs on the cloned `Arc`.
`health_check` delegates to `health_bucket`. An id missing from the map — an
organization created since the last reload — takes the default bucket, or
fails in strict mode.

### Reload

Reload rebuilds the whole object-store snapshot: it re-reads the config
*through the same loader* (file plus `MW_*` overlay, so precedence does not
change), re-reads the organization table and the per-org secrets, re-runs both
validation layers and the bucket probes, builds the stores and swaps them in
one atomic step. Because the snapshot is the `[object_store]` block, a changed
global bucket, endpoint, key or backend applies too. A rejected snapshot is
logged and leaves the previous one untouched, the way a failed TLS reload keeps
the old certificate.

Triggers are SIGHUP, `microwave-ctl reload` (via `systemctl reload`) and, when
`object_store.reload_interval_seconds` is non-zero, a periodic check,
mirroring the TLS reloader. The stamp is the config file's mtime and size plus
`MAX(updated_at)` over the `kvpair` rows of the `object_store` group and over
the orgs, so a `set-secret` or an `org-create` triggers it as well as a file
edit.

Correlating a command with its outcome needs more than a counter, because the
counter restarts with the process:

- the server writes a boot id (random per start) into the status file at start,
- the ctl writes a random request token to
  `<data_dir>/objectstore.reload.request`, then signals,
- the server reads the token when handling SIGHUP and echoes it with the
  outcome in `<data_dir>/objectstore.reload`, written to a temporary file,
  renamed, mode `0640`, without secrets,
- the ctl polls for its token for a bounded period and exits non-zero with
  distinct codes for timeout, rejected snapshot, superseded reload (another
  trigger answered first) and a server that is not running,
- a status from an older boot id is stale and ignored.

`diagnostics` prints the last outcome, trigger and boot id.

Org creation and deletion need no restart: create the org, add or remove the
entry and credentials, reload. The created org serves from the shared bucket
until the reload that carries its entry — in strict mode it fails loudly
instead. `org-create` prints the org id, the snippet to add and a warning that
the org is unusable until the map and the secrets cover it.

### Org lifecycle and migration

Deleting an org leaves its bucket and objects in place; the external tool
decides about cleanup. The server cannot enumerate an org's blobs after the
metadata rows are gone, so the tool must collect them before deletion.

The org **id** is part of the key prefix. Re-creating a tenant under the same
name yields a new id and therefore a new prefix: the old blobs stay under the
old prefix and are unreachable by the new org unless the tool copies them.
`org-create` prints the id so the tool can act on it.

Moving an org from shared to its own bucket, in this order:

1. create the bucket and the credentials, `set-secret` them,
2. `rclone copy` `organization-<org>/` from the shared bucket to the new one,
3. add the entry and reload — the entry routes as soon as it is active, even
   with strict mode still off,
4. drain or repeat step 2, because a copy is not quiesced and late writes
   still land in the shared bucket,
5. only then flip `require_org_bucket` and reload.

A rollback needs the reverse copy; without it every existing download breaks.
The layout is identical, so both directions are a plain copy.

### Diagnostics, backup and gather-logs

`backup --config-only` writes `microwave-config.json` from the resolved
`Config`. It gains the `organizations` map, with the per-org credentials
injected from the metadata database, matching how it already stores the global
keys in cleartext. A restore is then faithful.

`gather-logs` writes redacted TOML: `[object_store]` scalars first, then
`[object_store.organizations."<org>"]` sub-tables, credentials omitted.

`diagnostics` reports the strict-mode state, the org count, unmapped orgs and
orgs without credentials as errors, and the last reload result. It stays
offline unless `--probe` is given; the coverage checks run against the store.

### Status

In strict mode `health_bucket` is the only probe target. Outside strict mode
the global `bucket` is probed. A broken per-org bucket shows up as a failed
upload for that org, not in `/_status`.

### Observability

Object-store failures log the org id, the operation and the bucket — never the
keys — so a single broken tenant is attributable. The metrics histogram stays
labelled by backend and operation; an org label is not added, but the log line
is the tie-breaker.

## Tests

- Config parsing and each structural validation failure, including
  bucket-name rules, `(endpoint, bucket)` collisions, strict-mode `bucket`
  rejection and a missing `health_bucket`.
- `validate_tenant_map`: missing entry, missing credential, unknown org.
- A test that `set-secret` still works on a strict config with an uncovered
  org, i.e. `Context::load` does not run the coverage checks.
- Router unit tests: default fallback, override routing, `presigned_url` host
  and path, `health_check` on `health_bucket`, the swap guard not spanning the
  call.
- A mock-S3 test proving a request for an overridden org addresses the
  overridden bucket and the org's own key.
- Reload tests: a valid snapshot swaps, an invalid one keeps the old routing,
  an unknown org warns and stays inactive, an org created after the reload
  uses the shared bucket until the next one, a changed global key applies.
- Strict mode: startup and reload reject an unmapped org and missing
  credentials, `put`/`get`/`metadata` return `NoOrgBucket`, `exists` returns
  false, `presigned_url` propagates, upload and manifest answers are `500`.
- A test that `head` reports an unmapped org as an error and not as `404`.
- Bucket probe: a failing `HEAD` rejects the snapshot, `verify_buckets = false`
  skips it, concurrency stays bounded, `--check` does not probe.
- Reload correlation: the token round-trips, a stale boot id is ignored, a
  timeout and a superseded reload exit with their own codes.
- The stamp fires on a `set-secret` and on an `org-create`, not only on a file
  edit.
- `gather-logs` emits parent scalars before sub-tables; the config backup
  round-trips the mapping with credentials.
- `set-secret object_store <org>.<name>` accepts valid org names and rejects
  invalid ones.

## Open for the builder

- Whether unchanged entries are reused instead of rebuilt, and whether
  `microwave-ctl hup` keeps its current restart behavior or becomes the reload
  command next to a new `reload`.
- Whether `require_org_bucket` stays the flag name or becomes
  `bucket_fallback = false`.
- Whether `org-create` gains a `--bucket` that writes nothing but guides the
  operator.
- How `rotate-all` and `rotate-credentials object_store` treat the dynamic
  per-org keys. Today they iterate the fixed `KNOWN_CREDENTIALS` list and would
  touch only `object_store.secret`; a tenant-aware rotation is a follow-up, and
  until then diagnostics must list the uncovered keys.
- `microwave-migrate` writes blobs to a local `bookshelf` dir only
  (`LocalObjectStore` in `goiardi.rs`, `knife_backup.rs`, `from_api.rs`) and
  points S3 users at bucket replication. Decide whether it gains real
  `ObjectStore` support or stays out of scope for tenant mode.
- A full backup archives the database and the local `bookshelf`; S3 blobs are
  explicitly out of scope today (`backup.rs`). Say in the deployment docs that
  tenant buckets need their own backup path.
