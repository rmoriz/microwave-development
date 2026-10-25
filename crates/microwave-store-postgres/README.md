# microwave-store-postgres

PostgreSQL metadata-store backend for Microwave Server.

This crate implements the backend-neutral repository traits from
`microwave-store` against a schema equivalent to the unified SQLite schema in
`crates/microwave-store/migrations/0001_initial.sql`. The HTTP server selects
it when `database.driver = "postgres"`; SQLite remains the default.

## What is implemented

| Domain | Repository trait | Methods |
| --- | --- | --- |
| Orgs, users, clients, actor keys, principals | `IdentityRepository` | create/update/find/list/delete, profile patches, case-insensitive email uniqueness, key rotation |
| Authz graph and ACLs | `AuthzRepository` | entities, containers, groups, membership, nested and global groups, cycle detection, ACL read/replace, recursive permission checks, ACL target lookup, org read-access ACEs |
| Nodes, roles, environments, data bags/items | `ObjectRepository` | full CRUD, environment/policy indexes, container-inherited ACLs |
| Sandboxes and checksums | `SandboxRepository` | explicit sandbox rows (including empty sandboxes), upload-needed checks, checksum commit, lookup |
| Cookbooks | `CookbookRepository` | create (including `frozen`)/find/list/update/delete versions with checksum links and purge lists |
| Cookbook artifacts | `ArtifactRepository` | list/find artifacts and identifiers, create/find/delete versions with checksums and purge lists |
| Policyfiles | `PolicyRepository` | policies, revisions, policy groups and group/revision associations |
| Organization membership | `MembershipRepository` | org users, associations, `users`/`admins` groups, invitations and accept/reject |
| OAuth | `OAuthRepository` | applications, grants, access/refresh tokens, ID sessions and password-reset tokens (sessions and resets use `kvpair`, mirroring SQLite) |
| Key/value | `KvRepository` | raw `kvpair` access and the maintenance flag |
| Secrets | `SecretsRepository` | namespaced operator secrets under `secret:<group>:<name>` |
| Snapshot export/import | inherent methods | full `StoreSnapshot` export/import, shared format with SQLite, sequence advancement after explicit-id import |

`PostgresStore` implements the full contract, so it satisfies
`microwave_store::MetadataStore`.

## Schema choices

- **IDs stay 32-character lowercase hex** (`TEXT`) except PostgreSQL sequence
  identifiers (`BIGSERIAL`) for cookbooks, cookbook artifacts and OAuth rows.
- **Timestamps are RFC 3339 UTC `TEXT`.** This is deliberate: the Rust domain
  types already carry timestamps as `String`, and `TEXT` round-trips
  byte-for-byte with SQLite. A later migration can move to `TIMESTAMPTZ` if the
  domain types change together.
- **Chef documents are `TEXT`/JSON** (`serialized_object`, metadata, manifests).
  The SQLite schema stores them in `BLOB` columns but always writes UTF-8 JSON;
  PostgreSQL uses `TEXT` to avoid encode/decode at every boundary.
- **`serialized_object` is compressed transparently** on both backends. New
  writes use `mwz1:` + base64(zlib(JSON)); reads also accept legacy plain JSON,
  and `storage.compression = "none"` / `MW_STORAGE__COMPRESSION=none` disables
  new compression without invalidating existing rows. Decompression is bounded
  to 64 MiB and corrupt values return `StoreError::Compression` instead of
  panicking. Snapshot export always emits decoded JSON.
- **Booleans are native `BOOLEAN`**, unlike SQLite's `INTEGER` checks.
- **ACL and key maintenance stays in Rust transactions** — no triggers or
  PL/pgSQL, matching architectural guardrail 2 in the design document.
- The embedded migration is idempotent and guarded by
  `pg_advisory_xact_lock` so several replicas can start at once. It adds the
  `sandboxes` table, resolves global groups from `global_groups`, and widens
  legacy OAuth `expires_in` columns to `BIGINT`.

## Usage

```rust
use microwave_store::{IdentityRepository, MetadataStore};
use microwave_store_postgres::PostgresStore;

let store = PostgresStore::connect("postgres://microwave:secret@db/microwave").await?;
let orgs = store.list_orgs().await?;
```

`PostgresStore::connect` owns a `sqlx::PgPool` (max 10 connections) and runs
the embedded migration. Use `PostgresStore::from_pool` when the application
owns the pool, and call `migrate()` explicitly.

## Tests

```bash
# Crate integration tests; skip cleanly when no database is configured:
cargo test -p microwave-store-postgres

# Cross-backend matrix: SQLite always, PostgreSQL when the URL is set:
cargo test -p microwave-store --test dual_backend

# Start a disposable PostgreSQL container and run the whole PostgreSQL gate
# (crate integration tests, dual-backend matrix, HTTP runtime/coverage, CLI,
# and snapshot import/export):
scripts/run-postgres-tests.sh
```

`MICROWAVE_TEST_POSTGRES_URL` selects the database. Both the crate tests and
the matrix skip with an explicit `SKIP:` message when it is unset; when it is
set, a connection or migration failure is a hard test failure. The script also
skips only when Docker or its daemon is unavailable, so
`cargo test --workspace` remains green on machines without Docker.

The dual-backend matrix lives in
[`../microwave-store/tests/dual_backend.rs`](../microwave-store/tests/dual_backend.rs)
and covers identity, authz/ACLs, objects, sandboxes/cookbooks, policies,
artifacts, membership/invitations, OAuth, and KV/secrets. Names, checksums and
organizations are unique per run, assertions are scoped to the fixture
organization, and org-scoped fixtures plus test-created global groups are
deleted afterwards. The shared `server-admins` group and maintenance KV flag
remain. This lets the matrix and the crate tests run in parallel against one
database.

Because every `#[tokio::test]` owns its runtime, the matrix cannot share one
pool across tests. It applies the embedded schema once per test binary (in a
dedicated thread) and then opens a fresh pool per test with
`connect_existing`. This avoids running migration DDL concurrently with suite
DML, which PostgreSQL can deadlock on even though the migration itself is
advisory-locked. The cached migration result is retained so every test reports
the original connection or migration error rather than a poisoned `Once`.

The concurrency cases (global-group bootstrap, password-reset consumption, and
secret bootstrap) use eight barrier-released Tokio tasks on a multi-thread
runtime, so they exercise real database races. The maintenance flag is global
to the database: the test serializes access within the binary and restores the
flag before propagating a panic. Separate processes running maintenance
assertions against the same database must be serialized externally.

### Parity fixes found by the matrix

- `create_user_with_profile` now adds the default self grant and the
  `server-admins` group ACL entries (all permissions except `grant`), and adds
  admin users to that group, matching SQLite. Previously PostgreSQL users had
  no self ACL and no server-admin default, so authorization results differed by
  backend.
- `create_client_with_acl` now grants a non-validator client all permissions on
  itself, matching SQLite.
- `ensure_global_group` is race-safe in both backends: concurrent
  first-user/first-org startup uses an atomic insert, rolls back the losing
  authz entity, and returns the winning group instead of a spurious conflict.
  The matrix releases eight tasks together to pin this behavior.

## Runtime integration

`AppState` holds `Arc<dyn MetadataStore>`, so the API handlers, OAuth/identity
surface, embedded search, authorization policy, and bootstrap use the same code
for both backends. The server selects the backend in
`bin/microwave-server/src/main.rs`:

```sh
MW_DATABASE__DRIVER=postgres \
MW_DATABASE__URL=postgres://microwave:secret@db/microwave \
microwave-server serve
```

`AppState::from_sqlite` keeps the concrete SQLite handle available for tests
and SQLite-only helpers; `AppState::new` is backend-neutral.
`SecretsRepository::set_secret_if_absent` provides the atomic first-start
bootstrap (`INSERT ... ON CONFLICT DO NOTHING`), so several replicas starting
against one database converge on one object-store secret.

## Deferred work

- The `microwave-migrate` snapshot intentionally omits `kvpair` rows,
  including operator secrets and the maintenance flag. Use
  `microwave-ctl backup` when those must be preserved.
- Importing into a non-empty PostgreSQL database can conflict on unique names
  (for example an existing global `server-admins` group with a different id).
  Import into a fresh database, or reconcile the ids first.
- Multi-replica HA has not been load-validated. Connection retry/backoff,
  per-query timeouts, and advisory locks for cluster-wide cleanup jobs remain
  follow-ups.
