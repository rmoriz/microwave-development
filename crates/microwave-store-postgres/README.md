# microwave-store-postgres

PostgreSQL metadata-store backend for Microwave Server (first slice).

This crate implements the backend-neutral repository traits from
`microwave-store` against a schema equivalent to the unified SQLite schema in
`crates/microwave-store/migrations/0001_initial.sql`. It is intentionally
additive: the SQLite backend, the API crate, and the `AppState` wiring are
unchanged.

## What is implemented

| Domain | Repository trait | Methods |
| --- | --- | --- |
| Orgs, users, clients, actor keys, principals | `IdentityRepository` | create/update/find/list/delete, profile patches, case-insensitive email uniqueness, key rotation |
| Authz graph and ACLs | `AuthzRepository` | entities, containers, groups, membership, nested groups, cycle detection, ACL read/replace, recursive permission checks, ACL target lookup |
| Nodes, roles, environments, data bags/items | `ObjectRepository` | full CRUD, environment/policy indexes, container-inherited ACLs |
| Sandboxes and checksums | `SandboxRepository` | create sandbox, upload-needed checks, checksum commit, lookup |
| Cookbooks | `CookbookRepository` | create (including `frozen`)/find/list/update/delete versions with checksum links and purge lists |
| Policyfiles | `PolicyRepository` | policies, revisions, policy groups and group/revision associations |

PostgreSQL tables that are migrated but do **not** yet have repository methods:
`oauth_applications`, `oauth_access_grants`, `oauth_access_tokens`, `kvpair`,
`telemetry`, `org_user_invites`, `cookbook_artifacts*`.

`PostgresStore` currently implements only the six legacy repository traits.
The `MetadataStore` contract additionally requires `ArtifactRepository`,
`MembershipRepository`, `OAuthRepository`, `KvRepository` and
`SecretsRepository`, so `PostgresStore` does **not** satisfy `MetadataStore`
until those are implemented.

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
- **Booleans are native `BOOLEAN`**, unlike SQLite's `INTEGER` checks.
- **ACL and key maintenance stays in Rust transactions** — no triggers or
  PL/pgSQL, matching architectural guardrail 2 in the design document.
- The embedded migration is idempotent and guarded by
  `pg_advisory_xact_lock` so several replicas can start at once.

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
# Skip cleanly when no database is configured:
cargo test -p microwave-store-postgres

# Start a disposable PostgreSQL container, run migrations and tests:
scripts/run-postgres-tests.sh
```

The tests skip with an explicit `SKIP:` message when
`MICROWAVE_TEST_POSTGRES_URL` is unset or unreachable. The script also skips
when Docker or its daemon is unavailable, so `cargo test --workspace` remains
green on machines without Docker.

Each test uses a unique org/name suffix so the suite can run in parallel
against one database.

## Remaining work: making `AppState` backend-agnostic

The trait already lives in `microwave-store` (`src/backend.rs`), not in this
backend crate, so the API can depend on the contract rather than on PostgreSQL.
`MetadataStore` is a blanket supertrait over all repository traits.

To finish the integration:

1. **Implement the remaining repository traits for `PostgresStore`.**
   `ArtifactRepository`, `MembershipRepository`, `OAuthRepository`,
   `KvRepository` and `SecretsRepository` are defined in the contract and
   implemented for `SqliteStore`, but not yet for PostgreSQL. The PostgreSQL
   schema for OAuth, KV and invitations already exists; cookbook artifacts have
   tables but no methods yet.
2. **Change `AppState`.** Replace `pub store: Arc<SqliteStore>` with
   `pub store: Arc<dyn MetadataStore>` (all methods are object-safe through
   `async_trait`). `AuthContext` and handlers then operate on the trait.
3. **Abstract search construction.** `microwave-api::state::build_search_backend`
   currently accepts `Arc<SqliteStore>`; switch it to the read-side trait above.
4. **Select the backend in configuration.** Add `metadata.backend =
   "sqlite" | "postgres"` plus a `database_url` to `microwave-config`, and
   construct the appropriate store at startup. Default remains SQLite.
5. **Operational follow-ups.** Connection retry/backoff, per-query timeouts,
   advisory locks for cluster-wide cleanup jobs, and backup/restore support for
   PostgreSQL (the current backup path is SQLite `VACUUM INTO`).
