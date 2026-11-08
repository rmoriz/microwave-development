# Per-organization object quotas

## Intent

A multi-tenant install has no ceiling on what one tenant can store. A tenant can
upload cookbooks until the bucket fills, register nodes until the database and
the license distance grow, and create environments, roles and policy revisions
without bound. Bucket quotas and lifecycle rules only reach the blobs; they do
nothing for the metadata rows and give the tenant no early, comprehensible stop.

Operators running MTM want plan limits per organization: this tenant may have
500 nodes and 5000 cookbook versions, that one 20 nodes and 100 versions. This
document adds exactly those five ceilings — nodes, cookbook versions,
environments, roles, policy revisions — as opt-in, hard, per-organization
limits.

## Goals

- Optional, per-organization maximum for each of the five kinds.
- A creation that would exceed a maximum fails with `403` and a stable,
  readable message before anything is written.
- Counts are exact at the boundary, also with concurrent creators and more than
  one server instance on the same PostgreSQL database.
- Changing and deleting stay allowed while an organization is at or above a
  maximum, so a converge refreshes nodes and a cleanup frees slots.
- Configuration validates before a restart, changes apply through the existing
  reload path, and `check-config`/`diagnostics` name every mistake.
- An organization without an entry, and every existing install without a
  `[quotas]` section, remains unlimited.

## Non-goals

- Byte or blob quotas. Bucket quotas and lifecycle rules stay the operator's
  job (see [Multi-tenant mode](../multi-tenant-mode.md)).
- Limits for users, clients, groups, data bags or containers. A cookbook
  artifact version shares the cookbook version ceiling so the policyfile route
  cannot dodge it (there is no MCP artifact tool today, so the artifact route is
  HTTP-only).
- Soft warnings, grace overrides, per-principal limits, billing integration.
- Automatic cleanup or shrinking when an operator lowers a limit. Existing
  objects stay; the organization simply cannot add more.
- Quotas during `microwave-migrate` imports. The importer builds its own store
  handle without the policy, so a restore is never truncated.

## Anti-goals

- A quota that silently forgets a creation path, so HTTP and MCP disagree.
- A check-then-insert race that lets a tenant overshoot by the number of its
  concurrent creators.
- A `403` for an operation that creates no row (re-uploading an existing node,
  cookbook version or policy revision).
- Quotas that switch off with `limits.enabled`, or that need a restart to
  change.

## Constraints

- `limits` today bounds request pressure and lives process-locally with
  `limits.enabled` as its master switch; quota state is derived from the
  database, so it must not share that switch.
- The store is the only layer every write passes through: the signed HTTP API,
  the MCP tools and the action dispatcher all call the same `create_*` methods.
  Node creation (`create_node_object`) and cookbook upload
  (`put_cookbook_version_object`) already prove that sharing.
- The reload path already re-reads the configuration, resolves organization
  names to ids and swaps a validated snapshot; unknown organizations are a
  warning there, never a rejection.
- SQLite writes serialize on one connection; PostgreSQL needs an explicit
  per-organization lock to make counting exact across connections and replicas.
- `Config` is loaded and validated before any store exists, and
  `microwave-ctl check-config` must run without a live server.

## Approaches

**A — Own `[quotas]` section, guard in the store.** Chosen. The quota policy is
resolved to organization ids at startup and on reload, and installed into the
store through a shared handle. Each `create_*` reads its limit, takes an
advisory transaction lock for the organization and kind, skips the check when
the target row already exists, counts otherwise, and inserts in the same
transaction. Cost: a second organization-keyed map with its own validation,
diagnostics cross-check and reload wiring, plus a store-wide policy handle.

**B — Quotas on `object_store.organizations.<org>`.** Free validation and live
reload, and every MTM organization has an entry anyway. Rejected because the
object-store block would carry node and role ceilings that have nothing to do
with storage, and outside MTM the coupling reads as accidental.

**C — Pre-check in the API, quotas in `limits.organizations`.** Least code.
Rejected because a plain count before the store call cannot hold its own
transaction, so it is not exact, and `limits.enabled = false` would turn the
quotas off with the rate limits.

## Configuration

```toml
[quotas]
# Master switch; false ignores every entry below.
enabled = true

# Every field is optional, absent means unlimited for that kind. Zero is a
# valid ceiling: the organization may hold no object of that kind at all.
[quotas.organizations."acme"]
max_nodes = 500
max_cookbook_versions = 5000
max_environments = 20
max_roles = 100
max_policy_revisions = 200
```

- Fields: `max_nodes`, `max_cookbook_versions`, `max_environments`,
  `max_roles`, `max_policy_revisions`; all `u64`, all default unset.
- `max_environments` counts custom environments only. The auto-created
  `_default` is exempt: it cannot be deleted, so counting it would make zero
  meaningless and one unusable.
- `enabled` defaults to `true`, so adding an entry is the whole opt-in.
- The map is keyed by organization name and validates with `valid_org_name`.
- Environment forms are `MW_QUOTAS__ENABLED` and `MW_QUOTAS__ORGANIZATIONS` as
  one JSON object, like `MW_OBJECT_STORE__ORGANIZATIONS`.
- Quotas are independent of `object_store.require_org_bucket`. The MTM page
  recommends them together, `check-config` does not require it.

## Behaviour

### Counting

A `create_*` that carries a quota looks up its limit for the organization. With
no limit it behaves exactly as today: no lock, no count.

With a limit, inside the already-open creation transaction:

1. Take `pg_advisory_xact_lock(hashtextextended('<org_id>:<kind>', 0))`
   (PostgreSQL) or rely on the single SQLite connection's write serialization.
2. Return early if the target row exists: `(org, name)` for nodes,
   environments and roles, `(org, name, version)` for cookbook versions,
   `(org, name, revision)` for policy revisions, `(org, name, identifier)` for
   cookbook artifacts. Re-uploading an existing row never trips a limit and
   keeps its familiar `409`, not a misleading `403`.
   The policyfile association route creates a missing revision through
   `ensure_policy_revision`, so that helper carries the same existence
   short-circuit and the same quota check; a whole association that would add a
   revision fails with the quota error.
3. Count the kind for the organization. Cookbook versions count
   `cookbook_versions` plus `cookbook_artifact_versions`; environments count
   `name <> '_default'`; every other kind counts its own table.
4. If `count >= limit`, roll back and answer the quota error.

The lock is held until the commit, so two creators of the same organization and
kind run one after the other and the second sees the first. Different kinds and
different organizations stay parallel. The lock is per (organization, kind), so
a wide cookbook upload cannot block node registration.

### Response

`403` with the ordinary error body:

```json
{"error":["node limit of 500 reached"]}
```

Kind spellings: `node`, `cookbook version`, `environment`, `role`, `policy
revision`. MCP tools surface the same text as an `isError` result. Each
rejection logs `WARN` with the organization id, the kind and the limit, and
counts on the existing request-limit series as
`microwave_server_limits_total{scope="quota:<kind>",org="<org>",outcome="rejected"}`,
so `/_stats`, OTLP and the cardinality caps work unchanged.

### What stays allowed

- `PUT` of an existing node, environment, role, cookbook version or policy
  revision.
- Every delete. The next create of the same kind succeeds once the count is
  below the limit again.
- Reads, search, reports, MCP reads.
- `org-create` itself: quotas disabled → `None`. With an entry for the name,
  the new organization id is registered in the policy before the default
  containers and the `_default` environment exist, so the entry acts from the
  first request on. `_default` stays exempt, so `max_environments = 0` does not
  block creation.

### Validation, diagnostics and reload

- `Config::validate` rejects invalid organization names.
- `check-config` and `diagnostics` treat an entry that names an unknown
  organization as a problem and fail the check, exactly like the object-store
  tenant map does today; they also print a `quotas: <n> organization(s)` line.
  A reload treats the same entry as an inactive warning.
- The reloader resolves the map against the freshly read organization list and
  installs it only after `verify_buckets` passed, so a rejected reload leaves
  both routing and quotas unchanged instead of half-applying.
- `quotas.enabled = false` installs an empty policy, so the entries stay in the
  file but stop acting.

## Implementation shape

- `microwave-config`: `QuotasConfig`/`OrgQuotas`, defaults, validation,
  environment overlay, `packaging/microwave.toml.example` block.
- `microwave-store`: `QuotaKind`, `QuotaTarget`, `QuotaPolicy` (map keyed by
  organization id; `OrgQuotas` comes from `microwave-config`) and a
  `StoreError::Quota { kind, limit }` whose `Display` is the message above.
  `MetadataStore::set_quotas` replaces the whole policy, `set_org_quotas`
  merges one id; both backends hold an `RwLock<QuotaPolicy>` and implement the
  check in the five `create_*` methods and in `associate_policy_revision`. The
  count helper takes the open transaction: on SQLite a `self.`-based count
  would re-lock the `Mutex` the `create_*` already holds and deadlock.
  `microwave-store` gains the `microwave-config` dependency for `OrgQuotas`.
  Rejections reuse `Metrics::record_limit` with scope `quota:<kind>` and
  outcome `rejected`, so no metrics series is added.
- `microwave-authz`: `create_organization` gains a `quotas: Option<OrgQuotas>`
  argument and registers it on the fresh organization id before the default
  policy, so the entry is live immediately. Both callers pass the entry for the
  name from the loaded configuration.
- `microwave-api`: `StoreError::Quota` maps to `forbidden_with`, so HTTP and MCP
  share the mapping; the `org-create` handler passes the configured entry.
- `bin/microwave-server`: resolve and install the policy after the superuser
  bootstrap; the reloader installs the re-resolved policy after the routing
  snapshot verified.
- `microwave-ctl`: the diagnostics and check-config lines, and the configured
  entry passed from `org-create`.
- Docs: `docs/quotas.md` as the operator page, rows in the `deployment.md`
  configuration index, a cross-reference from `multi-tenant-mode.md` and
  `request-limits.md`, an entry in `docs/index.md` and the sidebar, claims in
  `docs/claims.toml`.

## Testing

- Config: parse and defaults, all five fields, zero accepted, invalid
  organization name rejected, `MW_QUOTAS__*` overlay, example-file block
  parses.
- Store, in `crates/microwave-store/tests/dual_backend.rs` so both backends
  run: under the limit allowed, at the limit rejected with the message, delete
  frees the slot, update of the existing row allowed, re-upload of an existing
  row allowed, other organization unaffected, unlimited organization
  unaffected, artifact and cookbook version sharing one ceiling, `_default`
  exempt from the environment count, an association that creates a policy
  revision, and a concurrent-crease test asserting the final count never
  exceeds the limit.
- API `crates/microwave-api/tests/quotas_test.rs`: `403` and body on all five
  creation endpoints and on a policyfile association that adds a revision,
  `200` on update and after delete, no cross-organization bleed, the MCP node,
  environment and cookbook-version paths, and `org-create` honoring an entry
  immediately.
- Ctl: unknown-organization problem, summary line, disabled switch.
- Reload: a changed file applies without restart; a rejected reload keeps the
  previous quotas.
- Docs: `website/docs` build, claims check, `cargo test --workspace`.

## Decisions taken in the conversation

- The policyfile association route is guarded through `ensure_policy_revision`.
- An entry for a fresh organization acts immediately, registered by
  `org-create` on the new id.

## Rollout

Opt-in, no migration. An install without `[quotas]` keeps today's behaviour;
the handle starts empty. Adding or changing an entry takes effect at the next
reload; an entry for a new organization takes effect with the creation.

## Calls left to the builder

- Exact counter and log naming beyond the ones above.
- Whether the diagnostics summary also prints current usage per organization;
  it is optional and needs one count per kind.
- The advisory-lock key derivation details on PostgreSQL, as long as it is
  stable per (organization, kind).
