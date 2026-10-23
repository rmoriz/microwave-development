# Chef Infra Server Rust Rewrite ("Microwave Server") — Architecture & Design Specification

**Date:** 2026-10-24  
**Status:** Draft for Review  
**Reference Codebases:**
- `chef-server` (`15.10.127` — `oc_erchef`, `oc_bifrost`, `bookshelf`, `oc-id`, `chef-server-ctl`, `nginx`, `oc-chef-pedant`)
- `chef` (`19.4.47` — `Chef::ServerAPI`, `Chef::HTTP::Authenticator`, `Chef::ServerAPIVersions`)

---

## 1. Intent and Why

Chef Infra Server today is a multi-runtime, multi-process system inherited from several generations of architecture:
- **Erlang (`oc_erchef`, `oc_bifrost`, `bookshelf`)** on OTP 26 with Webmachine, Mochiweb, Sqerl, and custom connection pools.
- **Ruby on Rails (`oc-id`)** for OAuth2 identity and application authorization.
- **Ruby (`chef-server-ctl`, `knife-opc`, `oc-chef-pedant`)** for administration, lifecycle, and API verification.
- **OpenResty / Nginx + Lua (`routes.lua`, `dispatch.lua`)** for edge routing, header validation, and internal upstream splitting.
- **PostgreSQL + OpenSearch/Elasticsearch** as mandatory heavy external dependencies even for small installations.

We are reimplementing Chef Infra Server in **Rust** (`microwave-server`) as a **drop-in replacement** for both the core Chef Infra API and the surrounding product surfaces (`oc-id`, `chef-server-ctl`, `data-collector`, `/license`, telemetry, and backup/migration), while collapsing the operational footprint into a **modular monolith** that:
1. Runs out of the box as a **single self-contained binary** with embedded storage (SQLite + Tantivy + local S3-compatible object store) capable of serving **~1,000 nodes**.
2. Scales horizontally in **High Availability (HA) mode** by swapping its provider traits to **PostgreSQL, OpenSearch, and S3 / Garage** behind a load balancer.

---

## 2. Goals, Non-Goals, and Anti-Goals

### Goals
- **Wire-level drop-in compatibility:** Existing `chef-client` (up to 19.x), `knife`, `berkshelf`,Policyfile tooling, Supermarket (`oc-id` OAuth2 consumers), Automate (`data-collector`), and the `oc-chef-pedant` test suite can interact with the server without client modifications.
- **Full API Version Support (`v0`, `v1`, `v2`):** Support `X-Ops-Server-API-Version` negotiation (`min_version: 0`, `max_version: 2`) and all version-specific payload behaviors (e.g., embedded `public_key` in `v0` vs. `/keys` table & `chef_key` responses in `v1+`; single vs. multi-principal responses in `/principals`).
- **Complete Surrounding Product Replacement:**
  - `oc-id` (`/id/*` OAuth2 provider, sessions, profile, password resets, `/id/v1/*` API).
  - `chef-server-ctl` CLI compatibility (user/org management, key rotation, reindexing, backup/restore, maintenance mode, secrets, status, pedant runner).
  - `data-collector` forwarding (`POST /organizations/:org/data-collector`).
  - `/license`, `/_status`, `/_stats`, `/server_api_version` endpoints.
- **Modular Backend Providers:** Clean Rust traits for Metadata Store, Search Engine, and Object Store:
  - *Built-in Default (~1,000 nodes):* SQLite (WAL mode) + Embedded Tantivy + Built-in `/bookshelf` local filesystem store (or local Garage).
  - *HA Production:* PostgreSQL + OpenSearch + S3-compatible object store (Garage / AWS S3 / MinIO).
- **Fresh Install + Migration Tool:** Provide a native migration tool (`microwave-ctl migrate` / `chef-server-ctl backup/restore`) that exports from an existing Erlang Chef Server (via API or `knife-ec-backup` archives / SQL dump) and imports into the Rust server, preserving orgs, users, password hashes, client/user public keys, ACLs, cookbooks, policies, nodes, roles, environments, and data bags.

### Non-Goals
- **In-place database reuse of legacy Chef Server PostgreSQL schemas:** We do not need to run atop an unmigrated `opscode_chef` + `bifrost` database in place. Data moves via the migration/restore tool.
- **Legacy Analytics (`/organizations/:org/controls`) and RabbitMQ indexing:** The `/controls` endpoint already returns `410 Gone` in Chef Server 15 and will continue to return `410 Gone`.
- **Pixel-identical Rails HTML/CSS in `oc-id`:** *(Architect Call)* While all URLs, form actions, OAuth2 flows, and JSON APIs under `/id` must be 100% compatible, the HTML views for login/profile/app management will be clean, lightweight server-rendered templates rather than compiling the legacy Sprockets/Rails asset pipeline.
- **Habitat / Runit supervision internals:** The Rust binary is self-supervised (or managed via `systemd`/containers); `chef-server-ctl` lifecycle commands map to the target runtime rather than `hab sup` or `runit`.

### Anti-Goals (Failure Conditions Even If It Works)
- **Leaking backend differences into API behavior:** A query or ACL check must never behave differently on SQLite/Tantivy vs. PostgreSQL/OpenSearch.
- **Split-brain metadata/ACL writes:** Never commit a domain object without its corresponding ACL graph entries in the same logical transaction.
- **Mandatory external services for default setup:** A user starting the default binary must not need to install Postgres, Java/OpenSearch, Nginx, Ruby, or Erlang.

---

## 3. Constraints

- **Cryptographic & Protocol Fidelity:**
  - Must support `X-Ops-Sign` versions `1.0`, `1.1`, and `1.3` (RSA PKCS#1 v1.5 decryption for `1.0`/`1.1` SHA1; RSA SHA256 signature verification for `1.3`), timestamp skew window (default ±15 minutes), and split `X-Ops-Authorization-1..N` headers (60-char lines).
  - Must support legacy user password hash formats during migration (`bcrypt`, `erlang-bcrypt-0.5.0`, `SHA1-bcrypt`, `sha1+bcrypt`, raw salted SHA1) and transparently upgrade them to `bcrypt` on login (`chef_password.erl` behavior).
- **Header & Client Version Checks:**
  - Reject `X-Chef-Version` below minimum supported version (e.g., `< 10.0.0`, while permitting `0.10.x` or `>= min` per `routes.lua`) with `400 Bad Request`.
  - Tolerate multi-host `X-Forwarded-Host`, `X-Forwarded-For`, `X-Forwarded-Server` headers.
  - Emit `X-Ops-Server-API-Version` JSON header on every response: `{"min_version":"0","max_version":"2","request_version":"<req>","response_version":"<resp>"}`.
- **Concurrency at ~1,000 Nodes (Default Mode):**
  - 1,000 nodes converging every 30 minutes averages ~0.55 converges/sec, with splay spikes of 20–50 concurrent converges (`GET /nodes/:name`, `POST /environments/:env/cookbook_versions`, `GET /search/*`, `PUT /nodes/:name`). SQLite in WAL mode with a serialized writer channel handles hundreds of writes/sec and thousands of concurrent reads/sec easily if kept free of long-held write locks.

---

## 4. Approaches Considered

### Approach A — Modular Monolith with Provider Traits *(Selected)*
A single Cargo workspace producing one primary binary (`microwave-server` / `chef-server-ctl` multicall or paired binaries) with role subcommands (`serve`, `worker`, `ctl`, `migrate`) and trait-abstracted backends (`MetadataStore`, `SearchBackend`, `ObjectStore`).
- **Pros:** Zero network hops between edge routing, API, and ACL checks; atomic SQL transactions across domain objects and ACLs; single binary deployment for ~1,000 nodes; horizontal stateless scaling for HA when pointed at Postgres + OpenSearch + Garage/S3.
- **Cons:** Requires strict crate boundaries so backend-specific code doesn't leak into domain logic.

### Approach B — Service-per-Component Port (`erchef`, `bifrost`, `bookshelf`, `oc-id`)
Separate Rust microservices communicating over HTTP/gRPC, mirroring the Erlang/Ruby topology.
- **Pros:** Matches legacy service boundaries 1:1.
- **Cons:** Makes the default embedded setup complex (multiple processes/databases), reintroduces cross-service partial failure states between `erchef` and `bifrost`, and adds latency.

### Approach C — Strangler Proxy in Front of Erlang Server
A Rust reverse proxy that gradually replaces endpoints while sharing the legacy database.
- **Pros:** Incremental replacement on a live legacy server.
- **Cons:** Requires running the full Erlang/Ruby/Postgres/OpenSearch stack; conflicts with the fresh-install + single-binary default goal.

**Decision:** **Approach A**, refined with five architectural guardrails agreed during review:
1. Use **Tantivy** (not SQLite FTS5) for embedded search behind a unified Chef document expander and Lucene PEG query AST.
2. Keep business logic, cycle detection, and key synchronization in **Rust transactions** rather than SQL triggers/PLpgSQL, using standard SQL + `WITH RECURSIVE` across SQLite and PostgreSQL.
3. Support **dual-mode presigned URLs** in `chef-objectstore` (in-process `/bookshelf` routes for local disk; native SigV4 presigned URLs for Garage/S3).
4. Support both **`inline` and `batch` indexing modes** inside the search subsystem so write-then-read tests and converge storms both work.
5. Implement `chef-server-ctl` directly against the Rust admin/domain crates rather than shelling out to Ruby/`knife-opc`/`psql`.

---

## 5. System Architecture & Crate Workspace Layout

```text
microwave-server/
├── Cargo.toml
├── crates/
│   ├── chef-config/        # TOML/env/chef-server.rb config loader & secrets store
│   ├── chef-crypto/        # X-Ops-Sign (1.0/1.1/1.3), RSA keygen cache, password hashes, SigV4
│   ├── chef-domain/        # Core domain types, validation rules, EJSON normalization, API v0/v1/v2 shapes
│   ├── chef-authz/         # Actors, groups, containers, objects, ACL engine, org creation policy
│   ├── chef-store/         # Unified repository traits + SQLite & PostgreSQL implementations
│   ├── chef-search/        # Document expander, Lucene PEG parser/AST, Tantivy & OpenSearch backends, batcher
│   ├── chef-objectstore/   # Cookbook blob store trait, local disk + S3/Garage backends, /bookshelf HTTP handler
│   ├── chef-depsolver/     # Pure Rust cookbook version constraint solver
│   ├── chef-api/           # Axum/Tower HTTP server, middleware, all ~70 Chef Infra API routes, data-collector
│   ├── chef-oauth/         # oc-id replacement (/id/* OAuth2 server, sessions, UI templates, /id/v1/*)
│   ├── chef-ctl/           # chef-server-ctl compatible CLI & backup/restore/reindex/admin commands
│   └── chef-migrate/       # Importer from legacy Chef Server (API crawler & knife-ec-backup archive reader)
└── bin/
    ├── microwave-server/   # Main daemon entrypoint (`serve`, `worker`)
    └── chef-server-ctl/    # Drop-in CLI binary
```

### Request Pipeline (`chef-api` + `chef-oauth` + `chef-objectstore`)

All HTTP traffic enters a single `axum` router (removing the need for OpenResty/Lua):

1. **Edge Normalization Middleware (`routes.lua` equivalent):**
   - Assigns or propagates `X-Request-Id`.
   - Normalizes multiple slashes (e.g., `/users/foo//association_requests` compatibility).
   - Rewrites default-org paths (`/nodes`, `/cookbooks`, `/data`, `/roles`, `/sandboxes`, `/environments`, `/clients`, `/principals`, `/groups`, `/containers`, `/search`) to `/organizations/{default_org}/...` when `default_orgname` is configured, **while preserving the original URI path in request extensions** for `X-Ops` signature verification (client requests are checksummed against the original path).
   - Validates `X-Chef-Version` header if present.
   - Negotiates `X-Ops-Server-API-Version` (`0`, `1`, `2`; defaults to `0` if omitted; halts with `406 Not Acceptable` and version payload if out of range or non-integer).
   - Checks maintenance mode flag (`503 Service Unavailable` when enabled, except `/_status`).
2. **Route Dispatch:**
   - `/_status`, `/_stats`, `/server_api_version`, `/license` -> System handlers.
   - `/bookshelf/*` -> `chef-objectstore` S3 SigV4 / presigned URL verifier and streaming blob handler (when embedded bookshelf is enabled).
   - `/id/*` -> `chef-oauth` router (OAuth2 + session + `/id/v1/*` handlers).
   - `/organizations/:org/data-collector` -> `chef-api` data-collector token/signed-auth verifier and async/sync forwarder.
   - All Chef Infra routes -> **Signed Auth Middleware** -> **Authz Middleware** -> **Resource Handler**.

---

## 6. Detailed Component Specifications

### 6.1 Cryptographic & Authentication Engine (`chef-crypto`)

#### Request Signature Verification (`X-Ops-Sign`)
- **Headers Required:**
  - `X-Ops-Sign`: `version=1.0`, `version=1.1`, or `version=1.3` (optionally prefixed with `algorithm=sha1;version=1.0;` etc.).
  - `X-Ops-UserId`: actor name (username or clientname).
  - `X-Ops-Timestamp`: ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`). Checked against configurable `authentication_skew` (default 900s).
  - `X-Ops-Content-Hash`: Base64-encoded hash of request body (SHA1 for `1.0`/`1.1`; SHA256 for `1.3`). Verified against actual body bytes.
  - `X-Ops-Authorization-1` ... `X-Ops-Authorization-N`: concatenated and Base64-decoded into signature bytes.
- **Canonical Request Construction:**
  - **Version 1.0:**
    ```text
    Method:METHOD\nHashed Path:BASE64_SHA1(NORMALIZED_PATH)\nX-Ops-Content-Hash:CONTENT_HASH\nX-Ops-Timestamp:TIMESTAMP\nX-Ops-UserId:USER_ID
    ```
  - **Version 1.1:** Same as `1.0`, except `X-Ops-UserId` is replaced by `BASE64_SHA1(USER_ID)`.
  - **Version 1.3:**
    ```text
    Method:METHOD\nPath:NORMALIZED_PATH\nX-Ops-Content-Hash:BASE64_SHA256(BODY)\nX-Ops-Sign:version=1.3\nX-Ops-Timestamp:TIMESTAMP\nX-Ops-UserId:USER_ID\nX-Ops-Server-API-Version:API_VERSION
    ```
    Verified via RSA-SHA256 (`RSASSA-PKCS1-v1_5` with SHA-256). Versions `1.0` and `1.1` use raw RSA public-key decryption (`RSA_public_decrypt` with PKCS#1 v1.5 padding) comparing the plaintext to the canonical string.
- **Key Lookup & Principal Resolution:**
  - Look up active (non-expired) keys in `actor_keys` for `(org_id, client_name)` and global `username`.
  - If an org contains both a client and an associated user with the same name (`API_v1+`), return all matching active keys and verify the signature against each until one succeeds (matching `oc_chef_wm_base` multi-key/multi-requestor verification).
- **RSA Keypair Generation Cache:**
  - Like `chef_keygen_cache`, maintain a background buffer of pre-generated 2048-bit RSA keypairs so client/user/org creation (`create_key: true`) responds immediately without CPU stalls, exposing status in `/_status`.

#### Password Hashing (`chef_password.erl` compatibility)
- Store `(password_hash, password_salt, hash_type)`.
- Default for new passwords: `bcrypt` (configurable cost, default 12).
- Verification supports:
  1. `bcrypt` / `erlang-bcrypt-0.5.0`
  2. `SHA1-bcrypt`: `bcrypt(hex_sha1(salt + "--" + password + "--"), hash)`
  3. `sha1+bcrypt`: `bcrypt(hex_sha1("--" + orig_salt + "--" + password + "--"), bcrypt_salt)`
  4. Legacy plain SHA1 (`ec` and `osc` formats).
- On successful login via `/authenticate_user` or `/id/signin`, non-`bcrypt` hashes are transparently upgraded to `bcrypt`.

---

### 6.2 Unified Data Model & Storage Layer (`chef-store`)

Instead of separate `opscode_chef`, `bifrost`, `bookshelf`, and `oc_id` databases with triggers and stored procedures, `chef-store` uses a **single unified relational schema** (compatible with both SQLite 3.35+ and PostgreSQL 14+) managed via embedded SQL migrations.

#### Core Schema Tables

1. **Identity & Organizations:**
   - `orgs (id CHAR(32) PK, authz_id CHAR(32) UNIQUE, name TEXT UNIQUE, full_name TEXT, assigned_at, last_updated_by, created_at, updated_at)`
   - `users (id CHAR(32) PK, authz_id CHAR(32) UNIQUE, username TEXT UNIQUE, email TEXT, serialized_object BLOB/JSON, password_hash TEXT, password_salt TEXT, hash_type TEXT, external_authentication_uid TEXT, recovery_authentication_enabled BOOL, admin BOOL, last_updated_by, created_at, updated_at)`
     - Case-insensitive unique index on `LOWER(email)`.
   - `org_user_associations (org_id CHAR(32), user_id CHAR(32), last_updated_by, created_at, updated_at, PK(org_id, user_id))`
   - `org_user_invites (id CHAR(32) PK, org_id CHAR(32), user_id CHAR(32), last_updated_by, created_at, updated_at, UNIQUE(org_id, user_id))`
   - `clients (id CHAR(32) PK, org_id CHAR(32), authz_id CHAR(32) UNIQUE, name TEXT, validator BOOL, admin BOOL, last_updated_by, created_at, updated_at, UNIQUE(org_id, name))`
   - `actor_keys (actor_id CHAR(32), actor_type TEXT CHECK(actor_type IN ('user','client')), key_name TEXT, public_key TEXT, key_version INT, expires_at TIMESTAMP, created_at TIMESTAMP, updated_at TIMESTAMP, last_updated_by CHAR(32), PK(actor_id, key_name))`
     - *Note:* In API `v0`, reading/updating `public_key` on a user or client reads/writes the `default` row in `actor_keys` inside the same Rust transaction—no SQL triggers needed.

2. **Authorization Graph (`chef-authz` tables, replacing `bifrost`):**
   - `authz_entities (authz_id CHAR(32) PK, entity_type TEXT CHECK(entity_type IN ('actor','group','object','container')), org_id CHAR(32) NULL)`
   - `authz_acl_actor (target_authz_id CHAR(32), permission TEXT, actor_authz_id CHAR(32), PK(target_authz_id, permission, actor_authz_id))`
   - `authz_acl_group (target_authz_id CHAR(32), permission TEXT, group_authz_id CHAR(32), PK(target_authz_id, permission, group_authz_id))`
   - `authz_group_actors (group_authz_id CHAR(32), actor_authz_id CHAR(32), PK(group_authz_id, actor_authz_id))`
   - `authz_group_groups (parent_group_authz_id CHAR(32), child_group_authz_id CHAR(32), PK(parent_group_authz_id, child_group_authz_id))`
   - `containers (id CHAR(36) PK, org_id CHAR(32), authz_id CHAR(32) UNIQUE, name TEXT, last_updated_by, created_at, updated_at, UNIQUE(org_id, name))`
   - `groups (id CHAR(36) PK, org_id CHAR(32), authz_id CHAR(32) UNIQUE, name TEXT, last_updated_by, created_at, updated_at, UNIQUE(org_id, name))`

3. **Chef Infra Objects:**
   - `nodes (id CHAR(32) PK, authz_id CHAR(32) UNIQUE, org_id CHAR(32), name TEXT, environment TEXT, policy_name TEXT NULL, policy_group TEXT NULL, serialized_object BLOB, last_updated_by, created_at, updated_at, UNIQUE(org_id, name))`
     - Indexes on `(org_id, environment)`, `(org_id, policy_name)`, `(org_id, policy_group)`.
   - `roles (id CHAR(32) PK, authz_id CHAR(32) UNIQUE, org_id CHAR(32), name TEXT, serialized_object BLOB, last_updated_by, created_at, updated_at, UNIQUE(org_id, name))`
   - `environments (id CHAR(32) PK, authz_id CHAR(32) UNIQUE, org_id CHAR(32), name TEXT, serialized_object BLOB, last_updated_by, created_at, updated_at, UNIQUE(org_id, name))`
   - `data_bags (id CHAR(32) PK, authz_id CHAR(32) UNIQUE, org_id CHAR(32), name TEXT, last_updated_by, created_at, updated_at, UNIQUE(org_id, name))`
   - `data_bag_items (id CHAR(32) PK, org_id CHAR(32), data_bag_name TEXT, item_name TEXT, serialized_object BLOB, last_updated_by, created_at, updated_at, UNIQUE(org_id, data_bag_name, item_name), FK(org_id, data_bag_name) ON DELETE CASCADE)`

4. **Cookbooks, Sandboxes & Checksums:**
   - `checksums (org_id CHAR(32), checksum CHAR(32), created_at TIMESTAMP, PK(org_id, checksum))`
   - `sandboxed_checksums (org_id CHAR(32), sandbox_id CHAR(32), checksum CHAR(32), created_at TIMESTAMP, PK(sandbox_id, org_id, checksum))`
   - `cookbooks (id INTEGER/BIGSERIAL PK, org_id CHAR(32), name TEXT, authz_id CHAR(32) UNIQUE, UNIQUE(org_id, name))`
   - `cookbook_versions (id CHAR(32) PK, cookbook_id BIGINT FK, org_id CHAR(32), name TEXT, major BIGINT, minor BIGINT, patch BIGINT, frozen BOOL, meta_attributes BLOB, meta_deps TEXT, meta_long_desc BLOB, metadata BLOB, serialized_object BLOB, last_updated_by, created_at, updated_at, UNIQUE(cookbook_id, major, minor, patch))`
   - `cookbook_version_checksums (cookbook_version_id CHAR(32) FK, org_id CHAR(32), checksum CHAR(32), FK(org_id, checksum) ON DELETE RESTRICT)`
   - `cookbook_artifacts (id INTEGER/BIGSERIAL PK, org_id CHAR(32), name TEXT, authz_id CHAR(32) UNIQUE, UNIQUE(org_id, name))`
   - `cookbook_artifact_versions (id INTEGER/BIGSERIAL PK, cookbook_artifact_id BIGINT FK, identifier BLOB/TEXT, metadata BLOB, serialized_object BLOB, created_at, created_by, UNIQUE(cookbook_artifact_id, identifier))`
   - `cookbook_artifact_version_checksums (cookbook_artifact_version_id BIGINT FK, org_id CHAR(32), checksum CHAR(32), FK(org_id, checksum) ON DELETE RESTRICT)`

5. **Policyfiles:**
   - `policies (id CHAR(32) PK, org_id CHAR(32), name TEXT, authz_id CHAR(32) UNIQUE, last_updated_by, UNIQUE(org_id, name))`
   - `policy_groups (id CHAR(32) PK, org_id CHAR(32), name TEXT, authz_id CHAR(32) UNIQUE, last_updated_by, serialized_object BLOB, UNIQUE(org_id, name))`
   - `policy_revisions (id CHAR(32) PK, org_id CHAR(32), policy_id CHAR(32) FK, revision_id TEXT, name TEXT, last_updated_by, serialized_object BLOB, UNIQUE(org_id, name, revision_id))`
   - `policy_group_revision_associations (id CHAR(32) PK, org_id CHAR(32), policy_revision_id CHAR(32) FK, policy_group_authz_id CHAR(32), policy_revision_name TEXT, policy_revision_revision_id TEXT, policy_group_name TEXT, last_updated_by, UNIQUE(org_id, policy_group_name, policy_revision_name))`

6. **OAuth2 (`oc-id`) & Operational Tables:**
   - `oauth_applications (id BIGSERIAL PK, name TEXT UNIQUE, uid TEXT UNIQUE, secret TEXT, redirect_uri TEXT, scopes TEXT, confidential BOOL, created_at, updated_at)`
   - `oauth_access_grants (id BIGSERIAL PK, resource_owner_id CHAR(32), application_id BIGINT FK, token TEXT UNIQUE, expires_in INT, redirect_uri TEXT, scopes TEXT, created_at, revoked_at)`
   - `oauth_access_tokens (id BIGSERIAL PK, resource_owner_id CHAR(32), application_id BIGINT FK, token TEXT UNIQUE, refresh_token TEXT UNIQUE, expires_in INT, scopes TEXT, created_at, revoked_at)`
   - `kvpair (key TEXT PK, value TEXT, updated_at TIMESTAMP)`
   - `telemetry (property TEXT, event_timestamp TIMESTAMP)`

#### Payload Compression (`chef_db_compression.erl` compatibility)
- `nodes`, `roles`, `environments`, `data_bag_items`, and `cookbook_versions` store `serialized_object` compressed with `zlib`/gzip (or raw JSON bytes—transparently detected on read via header magic bytes so both compressed and uncompressed blobs work).

---

### 6.3 Authorization Engine (`chef-authz`)

Replaces `oc_bifrost` and `oc_chef_authz` in-process:

- **Permissions:** `create`, `read`, `update`, `delete`, `grant`.
- **Superuser (`pivotal`):** Configured platform superuser bypasses ACL checks (except where explicitly forbidden, such as `/authenticate_user` and `/system_recovery` refusing to authenticate as superuser).
- **Transitive Group Resolution:**
  Checked via standard recursive CTE supported identically by SQLite and PostgreSQL:
  ```sql
  WITH RECURSIVE actor_groups(group_authz_id) AS (
      SELECT group_authz_id FROM authz_group_actors WHERE actor_authz_id = $1
      UNION
      SELECT gg.parent_group_authz_id
      FROM authz_group_groups gg
      JOIN actor_groups ag ON gg.child_group_authz_id = ag.group_authz_id
  )
  SELECT 1 FROM authz_acl_actor
   WHERE target_authz_id = $2 AND actor_authz_id = $1 AND ($3 = 'any' OR permission = $3)
  UNION ALL
  SELECT 1 FROM authz_acl_group
   WHERE target_authz_id = $2 AND group_authz_id IN (SELECT group_authz_id FROM actor_groups)
     AND ($3 = 'any' OR permission = $3)
  LIMIT 1;
  ```
- **Group Cycle Detection:**
  Before inserting `(parent, child)` into `authz_group_groups`, run a recursive reachability query in the transaction to verify `parent` is not reachable from `child` (replaces `forbid_group_cycles.sql`).
- **Object Creation Default ACLs:**
  When an actor creates a resource in a container `<C>`:
  - The new object's initial ACL is copied from `<C>`'s ACL, plus the creating actor is granted all 5 permissions (`create, read, update, delete, grant`) on the new object.
- **Organization Creation Policy (`oc_chef_authz_org_creator.erl`):**
  Executed inside a single database transaction when `POST /organizations` is called:
  1. Create `orgs` row.
  2. Create default containers: `clients`, `containers`, `cookbooks`, `data`, `environments`, `groups`, `nodes`, `roles`, `sandboxes`, `policies`, `policy_groups`, `cookbook_artifacts`.
  3. Create default groups: `admins`, `billing-admins`, `clients`, `users`, `public_key_read_access`, plus global `::{org}_read_access_group` and `::{org}_global_admins`.
  4. Apply the exact `DEFAULT_EC_EXPANDED_ORG` group memberships and container/group/org ACL matrix.
  5. Create the `_default` environment.
  6. Create the `{org}-validator` client (`validator: true`) + default RSA keypair and return `{"uri": ..., "clientname": "{org}-validator", "private_key": "..."}`.
- **ACL Constraints (`oc_chef_authz_acl_constraints.erl`):**
  - Prevent removing the `admins` group from the `grant` ACE of an organization or key objects when required by Chef Server semantics.
  - Enforce `clients` + `users` vs. `actors` query param/payload rules on `/_acl` endpoints.

---

### 6.4 Complete API Route Surface (`chef-api`)

Every route in `src/oc_erchef/apps/oc_chef_wm/priv/dispatch.conf` and `src/nginx/habitat/config/routes.lua` is implemented:

| Category | Routes | Key Compatibility Details |
|---|---|---|
| **Global / System** | `GET /_status`, `GET /_stats`, `GET /server_api_version`, `GET /license`, `POST /authenticate_user`, `POST /system_recovery` | `/_status` checks DB, search, object store, keygen cache; `/_stats` exposes Prometheus/JSON metrics. |
| **Users** | `GET,POST /users`, `GET,PUT,DELETE /users/:username` | Handles `v0` inline `public_key`/`private_key` vs `v1+` `create_key` & `chef_key` object. |
| **User Orgs & Invites** | `GET /users/:user/organizations`, `GET /users/:user/association_requests`, `GET /users/:user/association_requests/count`, `PUT /users/:user/association_requests/:id` | Supports double-slash `/users/:user//association_requests` compatibility route. |
| **Organizations** | `GET,POST /organizations`, `GET,PUT,DELETE /organizations/:org` | Creates default containers, groups, `_default` env, and validator client atomically. |
| **Org Users & Invites** | `GET,POST /organizations/:org/users`, `GET,DELETE /organizations/:org/users/:user`, `GET,POST /organizations/:org/association_requests`, `DELETE /organizations/:org/association_requests/:id` | Refuses removing user from org if still in `admins` group (`400`/`403` with exact message); manages `users` and `read_access` groups on association/dissociation. |
| **Clients** | `GET,POST /organizations/:org/clients`, `GET,PUT,DELETE /organizations/:org/clients/:client` | Validator client restrictions (`validator` can only create non-validator clients and read minimal data); `v0` vs `v1+` key behavior. |
| **Keys** | `GET,POST /users/:user/keys`, `GET,PUT,DELETE /users/:user/keys/:key`, `GET,POST /organizations/:org/clients/:client/keys`, `GET,PUT,DELETE /organizations/:org/clients/:client/keys/:key`, `GET /organizations/:org/users/:user/keys`, `GET /organizations/:org/users/:user/keys/:key` | Supports `"expiration_date": "infinity"` or ISO-8601 timestamp, `create_key: true`, key renaming on `PUT`. |
| **Principals** | `GET /organizations/:org/principals/:name` | May be called unsigned in internal flows; returns single object in `v0` or `{"principals": [...]}` array in `v1+`. |
| **Containers & Groups** | `GET,POST /organizations/:org/containers`, `GET,DELETE /organizations/:org/containers/:name`, `GET,POST /organizations/:org/groups`, `GET,PUT,DELETE /organizations/:org/groups/:name` | Group payload normalizes `actors`, `users`, `clients`, `groups`. |
| **ACLs** | `GET /users/:user/_acl`, `PUT /users/:user/_acl/:perm`, `GET /organizations/:org/:type/:name/_acl`, `PUT /organizations/:org/:type/:name/_acl/:perm`, `GET /organizations/:org/organizations/_acl`, `PUT /organizations/:org/organizations/_acl/:perm`, `GET /organizations/:org/*/_acl` | Supports `?detail=granular` (splitting `actors` into `users` and `clients`). Supported `:type`s: `clients`, `groups`, `containers`, `data`, `nodes`, `roles`, `environments`, `cookbooks`, `policies`, `policy_groups`, `cookbook_artifacts`, `organizations`. |
| **Nodes** | `GET,POST /organizations/:org/nodes`, `GET,PUT,DELETE /organizations/:org/nodes/:name`, `GET /organizations/:org/environments/:env/nodes` | Extracts `chef_environment`, `policy_name`, `policy_group`; indexes node on create/update/delete. |
| **Roles** | `GET,POST /organizations/:org/roles`, `GET,PUT,DELETE /organizations/:org/roles/:name`, `GET /organizations/:org/roles/:name/environments`, `GET /organizations/:org/roles/:name/environments/:env`, `GET /organizations/:org/environments/:env/roles/:name` | Supports `env_run_lists` extraction. |
| **Environments** | `GET,POST /organizations/:org/environments`, `GET,PUT,DELETE /organizations/:org/environments/:env`, `GET /organizations/:org/environments/:env/recipes`, `GET /organizations/:org/environments/:env/cookbooks`, `GET /organizations/:org/environments/:env/cookbooks/:cookbook` | `_default` environment cannot be deleted or renamed (`405 Method Not Allowed` / `400`). |
| **Depsolver** | `POST /organizations/:org/environments/:env/cookbook_versions` | Resolves `run_list` constraints + environment cookbook constraints via `chef-depsolver`, returns expanded cookbook version manifests with presigned download URLs. |
| **Data Bags** | `GET,POST /organizations/:org/data`, `GET,POST,DELETE /organizations/:org/data/:bag`, `GET,PUT,DELETE /organizations/:org/data/:bag/:item` | Each data bag appears as a dynamic search index in `/search` and `/search/:bag`. |
| **Sandboxes** | `POST /organizations/:org/sandboxes`, `PUT /organizations/:org/sandboxes/:id` | `POST` checks existing `checksums` for org; returns `needs_upload: true` + presigned `PUT` URL for missing MD5s, `needs_upload: false` for existing. `PUT` (`{"is_completed": true}`) verifies all needed checksums exist in `chef-objectstore` (returning `503` if missing) and moves them from `sandboxed_checksums` to `checksums`. |
| **Cookbooks** | `GET /organizations/:org/cookbooks`, `GET /organizations/:org/cookbooks/_latest`, `GET /organizations/:org/cookbooks/_recipes`, `GET /organizations/:org/cookbooks/:name`, `GET,PUT,DELETE /organizations/:org/cookbooks/:name/:version` | Supports `?num_versions=N` or `all`; enforces `frozen?` flag (`409 Conflict` unless `force=true`); validates all referenced file checksums exist in `checksums`; garbage-collects unreferenced checksums from `chef-objectstore` on delete. |
| **Cookbook Artifacts** | `GET /organizations/:org/cookbook_artifacts`, `GET /organizations/:org/cookbook_artifacts/:name`, `GET,PUT,DELETE /organizations/:org/cookbook_artifacts/:name/:identifier` | Used by Policyfiles; keyed by 40-char SHA1 identifier rather than semver. |
| **Policies & Policy Groups** | `GET /organizations/:org/policies`, `GET,POST,DELETE /organizations/:org/policies/:name`, `GET /organizations/:org/policies/:name/revisions`, `GET,DELETE /organizations/:org/policies/:name/revisions/:rev`, `GET /organizations/:org/policy_groups`, `GET,DELETE /organizations/:org/policy_groups/:group`, `GET,PUT,DELETE /organizations/:org/policy_groups/:group/policies/:name` | Full Policyfile revision and group association management. |
| **Search** | `GET /organizations/:org/search`, `GET,POST /organizations/:org/search/:index` | `GET` returns full objects; `POST` performs partial search extracting requested attribute paths (`chef_deep_merge` / path extraction). |
| **Universe & Validate** | `GET /organizations/:org/universe`, `ANY /organizations/:org/validate/*`, `GET /organizations/:org/:type/:name/_identifiers` | `/universe` returns Berkshelf-compatible dependency graph; `/validate/*` verifies signed headers and returns `{"requestor_name": ..., "requestor_type": ...}`; `/_identifiers` returns internal IDs for nodes. |
| **Controls (Deprecated)** | `POST /organizations/:org/controls` | Returns `410 Gone` `{"error": "Resource gone, analytics is no longer supported by Chef Server"}`. |
| **Data Collector** | `POST /organizations/:org/data-collector` | Validates `x-data-collector-token` or Chef signed request and forwards run/action payloads to configured external Automate/webhook endpoint. |

---

### 6.5 Search & Indexing Subsystem (`chef-search`)

#### 1. Document Expansion (`chef_index_expand` port)
When any indexable object (`node`, `role`, `environment`, `client`, `data_bag_item`) is created or updated:
- Prepare the indexable JSON representation (e.g., for nodes: merge `default`, `normal`, `override`, `automatic` attributes according to Chef deep-merge precedence, attach `recipe` and `role` expansions from `run_list`, plus `name`, `chef_environment`, `policy_name`, `policy_group`, `chef_type`, `X_CHEF_id_CHEF_X`, `X_CHEF_database_CHEF_X`, `X_CHEF_type_CHEF_X`).
- Flatten nested keys with `_` and emit both full-path and leaf-key tokens separated by `__=__`:
  - `{"a": {"b": "foo"}}` -> `a_b__=__foo` and `b__=__foo`.
- Special-case array/run_list values (`recipe[foo]`, `recipe[foo::default]`, `role[bar]`) exactly as `chef_index_expand.erl` does so `oc-chef-pedant/spec/api/search/search_spec.rb` and `word_break_spec.rb` pass.

#### 2. Query Parser (`chef_lucene.peg` port)
- Parse incoming `?q=...` query strings using a Rust PEG parser (`peg` or `pest` crate) matching `chef_lucene.peg`:
  - `*:*`, `field:term`, `field:"phrase"`, `field:[start TO end]`, `field:{start TO end}`, wildcards (`*`, `?`), boolean operators (`AND`, `&&`, `OR`, `||`, `NOT`, `!`, `+`, `-`), grouping `(...)`, boosting `^`, fuzzy `~`.
  - Invalid queries throw `400 Bad Request` with Chef's exact malformed search error message.
  - Missing `q` parameter defaults to `*:*`; empty `q=""` returns `400 Bad Request`.

#### 3. Search Backends (`SearchBackend` trait)
- **Embedded Default (Tantivy):**
  - Stores index in `{data_dir}/search/`.
  - Schema fields: `id` (STRING, stored), `org_id` (STRING, fast/indexed), `obj_type` (STRING, indexed), `data_bag` (STRING, optional), `content` (TEXT with custom tokenizer preserving `__=__` and Chef word-break rules).
  - Evaluates the parsed query AST directly into Tantivy queries (`TermQuery`, `PhraseQuery`, `RegexQuery`/`Wildcard`, `RangeQuery`, `BooleanQuery`), sorted by `X_CHEF_id_CHEF_X asc`, paginated by `start` and `rows` (default `start=0, rows=1000`).
- **HA Backend (OpenSearch / Elasticsearch):**
  - Translates the rewritten query + `filter_query` (`+X_CHEF_database_CHEF_X:chef_{org_id} +X_CHEF_type_CHEF_X:{type}`) to OpenSearch `/_search` and `/_bulk` APIs, matching `chef_opensearch.erl`.

#### 4. Indexing Queue Modes & Post-Filtering
- Supports `inline` (immediate commit before HTTP response returns) and `batch` (coalesces concurrent writes over configurable `search_batch_max_wait` e.g. 20ms or `search_batch_max_size`, commits once, and unblocks all waiting HTTP handlers—matching `chef_index_batch.erl`).
- **Database Hydration & Strict ACL Filtering:**
  - Search returns matching object IDs -> `chef-store` bulk-fetches the objects -> if `strict_search_result_acls` is enabled, `chef-authz` filters out objects the requestor lacks `READ` permission on -> for `POST /search/:index` (partial search), extracts only the requested key-path map before serializing JSON.

---

### 6.6 Object Storage & Bookshelf (`chef-objectstore`)

Cookbook files are content-addressed by `(org_id, md5_checksum)` under key `organization-{org_id}/checksum-{md5}`.

- **`ObjectStore` Trait:**
  - `head_checksums(org_id, &[checksum]) -> ChecksumStatus { present, missing, errors }`
  - `delete_checksums(org_id, &[checksum])`
  - `generate_presigned_url(org_id, method: Put|Get, checksum, ttl) -> Url`
  - `put_stream(bucket, key, content_md5, stream)` / `get_stream(bucket, key)`
- **Built-in Default (Local Filesystem + Embedded `/bookshelf` HTTP Endpoint):**
  - Stores blobs in `{data_dir}/bookshelf/organization-{org_id}/checksum-{md5}` (writing to a temporary file first, verifying MD5 stream digest on the fly, and atomically renaming on completion).
  - Exposes `HEAD`, `GET`, `PUT`, `DELETE` on `/bookshelf/:bucket/organization-:org/checksum-:md5`.
  - Verifies AWS SigV4 (both query-string presigned URLs and `Authorization: AWS4-HMAC-SHA256` headers) using `chef-crypto`'s SigV4 implementation (`bksw_wm_base.erl` / `bksw_sec.erl` parity).
- **External S3 / Garage Backend:**
  - Uses standard S3 SigV4 presigned URLs pointing to Garage / MinIO / AWS S3 (`external_url` for client-facing presigned URLs, `internal_url` for server-side `HEAD` verification and `DELETE` garbage collection).
  - Can also optionally run an embedded or sidecar **Garage** instance if configured.

---

### 6.7 Cookbook Dependency Solver (`chef-depsolver`)

Port of `src/oc_erchef/apps/depsolver`:
- Supports Chef version strings (`MAJOR.MINOR` normalized to `MAJOR.MINOR.0`, and `MAJOR.MINOR.PATCH`) and constraint operators (`=`, `>`, `<`, `>=`, `<=`, `~>`).
- Given:
  1. All cookbook versions and their dependency metadata in the organization (`fetch_all_cookbook_version_dependencies`),
  2. The target environment's `cookbook_versions` constraints,
  3. The expanded `run_list` cookbook goals from `POST /organizations/:org/environments/:env/cookbook_versions`,
- Runs a backtracking constraint solver with a configurable timeout (default 2,000ms).
- On success, bulk-fetches the selected `cookbook_version` records, attaches presigned `GET` URLs for all file checksums, and returns the map.
- On failure, returns `412 Precondition Failed` with the exact structured error JSON (`message`, `non_existent_cookbooks`, `most_constrained_cookbooks`, `unable_to_solve`) expected by `chef-client` and `depsolver_spec.rb`.

---

### 6.8 Surrounding Products

#### 1. `oc-id` Replacement (`chef-oauth`)
Mounted under `/id`:
- **OAuth2 Provider (Doorkeeper-compatible):**
  - `GET /id/oauth/authorize`, `POST /id/oauth/authorize`, `DELETE /id/oauth/authorize`
  - `POST /id/oauth/token` (authorization_code and refresh_token grants)
  - `POST /id/oauth/revoke`
  - `GET,POST /id/oauth/applications`, `GET,PUT,DELETE /id/oauth/applications/:id` (restricted to configured `oc_id.administrators` or server admins)
- **User Sessions, Profile & Password Reset:**
  - `GET /id/signin`, `POST /id/sessions`, `DELETE /id/signout`
  - `GET,PUT /id/profile`, `PUT /id/profile/password`, `GET /id/profile/email`, `POST /id/profile/regen_key`
  - `GET,POST,PUT /id/password-reset`
- **REST API for OAuth Consumers (e.g., Chef Supermarket `omniauth-chef-oauth2`):**
  - `GET /id/v1/me` (returns authenticated user profile + public key)
  - `GET /id/v1/me/organizations` (returns user's organizations)
  - `GET /id/v1/users/:id`
  - `GET /id/v1/status`
- **Implementation:** Uses internal service calls to `chef-domain`/`chef-store` (no need to sign HTTP loopback requests with `webui_priv.pem`, though `webui_priv.pem` is still generated/maintained in secrets for external tooling compatibility).

#### 2. `chef-server-ctl` Replacement (`chef-ctl`)
A binary named `chef-server-ctl` (and `microwave-ctl` alias) implementing the full operational command surface:
- **Organization & User Management (formerly `wrap-knife.rb` / `knife-opc`):**
  - `user-list`, `user-show`, `user-create`, `user-delete`, `user-edit`
  - `org-list`, `org-show`, `org-create`, `org-delete`, `org-user-add`, `org-user-remove`
  - `grant-server-admin-permissions`, `remove-server-admin-permissions`, `list-server-admins`
  - `password <username>`
- **Key Rotation (`key_control.rb`):**
  - `add-client-key`, `add-user-key`, `list-client-keys`, `list-user-keys`, `delete-client-key`, `delete-user-key`
- **Search & Maintenance:**
  - `reindex [org]` (with `-w/--with-timing`, `-d/--Select-type`)
  - `maintenance on|off|status`
  - `cleanup`, `cleanup-bifrost` (no-op or orphan check in unified DB)
  - `filtered-dump`
- **Secrets & Config:**
  - `show-secret`, `set-secret`, `remove-secret`, `rotate-credentials`, `rotate-all-credentials`, `rotate-shared-secrets`, `oc-id-show-app`, `check-config`, `reconfigure`
- **Backup, Restore & Migration:**
  - `backup` / `restore` (creates/restores a portable tarball of metadata, ACLs, keys, cookbook blobs, and `oc-id` apps).
  - `migrate-from-legacy` (imports from a live legacy Chef Server API + Postgres/bookshelf dump or `knife-ec-backup` directory).
- **Diagnostics & Testing:**
  - `status`, `ha-status`, `version`, `gather-logs`, `psql` (opens `sqlite3` or `psql` shell against configured store), `test` (invokes `oc-chef-pedant` against localhost).

#### 3. Data Collector, License & Telemetry
- **Data Collector (`/organizations/:org/data-collector`):**
  - Enabled when `data_collector.root_url` is configured.
  - Accepts compliance/converge payloads, validates `x-data-collector-token` (or client signature), and forwards asynchronously/synchronously via `reqwest` connection pool.
  - Also supports optional action notifications (`oc_chef_action`) when objects are mutated on the server.
- **License (`/license`) & Telemetry:**
  - `GET /license` returns `{"limit_exceeded": false, "node_license": <configured_max>, "node_count": <count>, "upgrade_url": "..."}`.
  - Telemetry defaults to **disabled** (no external phone-home), while keeping the `telemetry` table and config toggles so pedant's `license_spec.rb` and `server_license_spec.rb` pass.

---

### 6.9 High Availability (HA) vs. Default Single-Node Architecture

| Aspect | Default Mode (~1,000 Nodes) | HA Mode (10,000+ Nodes) |
|---|---|---|
| **Process Topology** | Single `microwave-server serve` process on one host. | Multiple stateless `microwave-server serve` replicas behind any L4/L7 load balancer. |
| **Metadata & ACL Store** | Embedded **SQLite** in WAL mode (`synchronous=NORMAL`, `busy_timeout=5000`, single writer lock + pooled readers). | Shared **PostgreSQL** cluster (RDS, Patroni, etc.) via `sqlx::PgPool`. |
| **Search Index** | Embedded **Tantivy** index on local disk (`{data_dir}/search`). | Shared **OpenSearch** cluster via HTTP bulk/search APIs. |
| **Cookbook Blobs** | Built-in `/bookshelf` endpoint writing to `{data_dir}/bookshelf` (or local single-node Garage). | Shared **Garage** cluster or **AWS S3 / MinIO** via SigV4 presigned URLs. |
| **Background Tasks** | In-process Tokio tasks (keygen cache, index batch flusher, sandbox expiry cleanup). | Stateless per-replica keygen & batch flushers; cluster-wide periodic cleanup coordinated via PostgreSQL advisory locks (`pg_try_advisory_lock`). |

---

### 6.10 Verification Strategy (`oc-chef-pedant` + Rust Test Suite)

1. **Unit & Property Tests (Rust):**
   - `chef-crypto`: Test vectors from `mixlib-authentication` (Sign 1.0, 1.1, 1.3) and `chef_password_tests.erl`.
   - `chef-search`: Port all `chef_index_expand` and `chef_lucene` EUnit test cases.
   - `chef-depsolver`: Port all `depsolver_tests.erl` cases.
   - `chef-store`: Run identical repository test suite against both in-memory SQLite and test PostgreSQL.
2. **Integration Harness (`oc-chef-pedant`):**
   - Generate a `pedant_config.rb` pointing at the running `microwave-server` instance (seeded with `pivotal` superuser and test keys).
   - Run `bundle exec ./bin/oc-chef-pedant` incrementally by tag (`--focus-headers`, `--focus-keys`, `--focus-users`, `--focus-organizations`, `--focus-clients`, `--focus-nodes`, `--focus-roles`, `--focus-environments`, `--focus-data`, `--focus-sandboxes`, `--focus-cookbooks`, `--focus-cookbook_artifacts`, `--focus-policies`, `--focus-search`, `--focus-acls`, `--focus-depsolver`, `--focus-api_v1`, `--focus-oc_id`, `--focus-license`).
3. **End-to-End Client Converge Test:**
   - Run the `chef` 19.4.47 client (`chef-client` and `knife`) from `./chef` against `microwave-server` for node bootstrap, cookbook upload, policyfile push, search, and converge.

---

## 7. Decisions Made vs. Left to the Builder

### Decisions Locked In
- **Architecture:** Modular monolith in Rust (Approach A) with trait-abstracted Metadata, Search, and Object stores.
- **Default Backends:** SQLite (WAL) + Tantivy + Local `/bookshelf` (with first-class Garage/S3 support) targeting ~1,000 nodes out of the box.
- **HA Backends:** PostgreSQL + OpenSearch + Garage/S3.
- **Scope:** Full drop-in replacement including core Chef Infra API (`v0`–`v2`), `oc-id`, `chef-server-ctl`, `data-collector`, `/license`, and a legacy migration tool.

### Left to the Builder
- Choice of exact Rust crates for HTTP/SQL/Templates (recommended defaults: `axum` + `tower`, `sqlx` or `rusqlite`+`tokio-postgres`, `tantivy`, `rsa`+`sha1`+`sha2`+`bcrypt`, `askama` or `maud` for `oc-id` HTML templates, `clap` for CLI).
- Exact internal binary packaging (single multicall binary detecting `argv[0] == "chef-server-ctl"` vs. two binaries built from the same workspace).
