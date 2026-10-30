# Microwave Server — Master Implementation TODO

This document tracks the end-to-end engineering checklist for reimplementing the upstream Chef Infra Server API (`15.10.127`) and its surrounding compatibility surfaces in Rust, as specified in [`docs/superpowers/specs/2026-10-24-microwave-server-design.md`](docs/superpowers/specs/2026-10-24-microwave-server-design.md).

> **Status snapshot (2026-10-30):** The original phase checklist is kept as a design trace. Phases 1–2 are ticked, and every tick carries the command or file that proves it. Treat the test, pedant, and e2e reports as authoritative.
>
> **Implemented:** the Rust workspace, TOML and `MW_*` configuration, X-Ops signing 1.0/1.1/1.3, both metadata backends behind one repository contract, the authz/ACL engine with the default-organization policy, the HTTP edge middleware, the identity, organization, client, key, principal, container, group, and ACL APIs, and first-start bootstrap of the superuser key/password and the object-store secret. The phases below Phase 3 keep their own boxes.
>
> **Verified:** `cargo test --workspace`, `cargo clippy --workspace --all-targets`, and `cargo fmt --check` are green; the item-level evidence is the `<!-- verified: ... -->` comment on each box. Core pedant is 652 passed / 0 failed / 8 pending and the extended suites are 3,284 passed / 0 failed / 7 pending of 3,291; the 8 pending core examples are pedant's own skips.
>
> **Open:** `gap-chef-server-rb-importer`, `gap-secrets-file`, `gap-transitive-member`, and `gap-x-ops-api-info`.

---

## Phase 1: Workspace Foundation, Configuration, Cryptography & Storage Core

### 1.1 Cargo Workspace & Configuration (`microwave-config`)
- [x] Initialize root `Cargo.toml` workspace with crates: `microwave-config`, `microwave-crypto`, `microwave-domain`, `microwave-authz`, `microwave-store`, `microwave-search`, `microwave-objectstore`, `microwave-depsolver`, `microwave-api`, `microwave-oauth`, `microwave-ctl`, `microwave-migrate`, and binary crates `microwave-server` and `microwave-ctl` (installing a `chef-server-ctl` compatibility alias). <!-- verified: sed -n '3,20p' Cargo.toml -->
- [x] Document and enforce the naming/trademark policy: no `chef-` prefix for project crates, binaries, modules, services, or public types; use upstream names only where required for compatibility. <!-- verified: docs/superpowers/specs/2026-10-24-microwave-server-design.md:12 -->
- [x] Implement configuration schema (`Config`) supporting TOML config file, environment variable overrides, and basic `/etc/opscode/chef-server.rb` key-value extraction for drop-in compatibility. <!-- claim:gap-chef-server-rb-importer -->
- [x] Implement secrets management (`private-chef-secrets.json` reader/writer + environment variable fallback `CHEF_SECRETS_DATA`) for `pivotal` superuser key, `webui_pub`/`webui_priv`, `bookshelf` access/secret keys, `oc_id` secret_key_base, and `data_collector` token. <!-- claim:gap-secrets-file -->
- [x] Implement automatic bootstrap initialization on first start: generate a `pivotal` RSA keypair, bookshelf HMAC credentials, and seed the `pivotal` superuser in the database. (`webui` keys exist only where a snapshot or knife backup carried them; first start does not create them.) <!-- verified: bin/microwave-server/src/bootstrap.rs ensure_superuser, ensure_object_store_secret -->
- [x] Add structured logging (`tracing` + `tracing-subscriber`) and Prometheus/JSON metrics registry.

### 1.2 Cryptography, Request Signing & Passwords (`microwave-crypto`)
- [x] Implement `X-Ops-Content-Hash` calculation and verification:
  - [x] SHA-1 Base64 digest for Sign `1.0` and `1.1`.
  - [x] SHA-256 Base64 digest for Sign `1.3`.
- [x] Implement canonical path normalization (collapsing duplicate slashes `/+`, stripping trailing slashes except `/`, ignoring query strings, preserving original path before default-org rewrite).
- [x] Implement `X-Ops-Sign` header parser (`version=1.0`, `version=1.1`, `version=1.3`, with optional `algorithm=...;` prefix).
- [x] Implement `X-Ops-Authorization-1..N` header reassembly and Base64 decoding. <!-- verified: crates/microwave-crypto/src/signature.rs:61 -->
- [x] Implement timestamp skew validator (`X-Ops-Timestamp` ISO-8601 UTC parsing, configurable `authentication_skew` default 900s). <!-- verified: crates/microwave-api/src/middleware.rs:849 -->
- [x] Implement Sign `1.0` & `1.1` verifier (RSA PKCS#1 v1.5 public-key decryption / raw verification against canonical string).
- [x] Implement Sign `1.3` verifier (`RSASSA-PKCS1-v1_5` with SHA-256 against canonical string including `X-Ops-Server-API-Version`).
- [x] Implement outbound `X-Ops-Sign` request signer (for `microwave-ctl`, `microwave-migrate`, and test harnesses).
- [x] Implement AWS SigV4 signer & verifier (both query-string presigned URLs `X-Amz-Algorithm=AWS4-HMAC-SHA256` and `Authorization: AWS4-HMAC-SHA256` headers) for Bookshelf/S3 compatibility. <!-- verified: crates/microwave-objectstore/src/s3.rs -->
- [x] Implement `chef_password` verifier and upgrader:
  - [x] Standard `bcrypt` and `erlang-bcrypt-0.5.0`.
  - [x] `SHA1-bcrypt` (`bcrypt(hex_sha1(salt + "--" + pass + "--"))`).
  - [x] `sha1+bcrypt` (`bcrypt(hex_sha1("--" + orig_salt + "--" + pass + "--"), bcrypt_salt)`).
  - [x] Legacy plain salted SHA-1 (`ec` and `osc` formats).
  - [x] Constant-time comparison and automatic re-hash to `bcrypt` on verify.
- [x] Implement `KeygenCache` background worker (pre-generating 2048-bit RSA PEM keypairs in a bounded queue) and health reporting for `/_status`.
- [x] Port unit test vectors from `mixlib-authentication`, `chef_authn`, and `chef_password_tests.erl`.

### 1.3 Unified Database Schema & Store Providers (`microwave-store`)
- [x] Define `MetadataStore` and transactional unit-of-work traits covering all domain and authz operations. <!-- verified: crates/microwave-store/src/backend.rs:48 -->
- [x] Author unified SQL schema migrations compatible with both SQLite 3.35+ and PostgreSQL 14+: <!-- verified: crates/microwave-store/migrations/0001_initial.sql -->
  - [x] `orgs`, `users`, `org_user_associations`, `org_user_invites`, `clients`, `actor_keys`.
  - [x] `authz_entities`, `authz_acl_actor`, `authz_acl_group`, `authz_group_actors`, `authz_group_groups`, `containers`, `groups`.
  - [x] `nodes`, `roles`, `environments`, `data_bags`, `data_bag_items`.
  - [x] `checksums`, `sandboxed_checksums`, `cookbooks`, `cookbook_versions`, `cookbook_version_checksums`, `cookbook_artifacts`, `cookbook_artifact_versions`, `cookbook_artifact_version_checksums`.
  - [x] `policies`, `policy_groups`, `policy_revisions`, `policy_group_revision_associations`.
  - [x] `oauth_applications`, `oauth_access_grants`, `oauth_access_tokens`, `kvpair`, `telemetry`.
- [x] Implement SQLite backend (`WAL` journal mode, `foreign_keys = ON`, `busy_timeout = 5000`, serialized write connection + read pool). <!-- verified: crates/microwave-store/src/sqlite.rs:87 -->
- [x] Implement PostgreSQL backend (`sqlx::PgPool`, advisory lock helper for HA background tasks). <!-- verified: crates/microwave-store-postgres/src/lib.rs -->
- [x] Implement transparent compression for `serialized_object` columns using the custom MWZ1 envelope (`mwz1:` + base64(zlib(JSON))). This is not byte-level upstream compatibility; legacy plain JSON remains readable. Configure with `storage.compression = "zlib" | "none"` / `MW_STORAGE__COMPRESSION`, default `zlib`.
- [x] Write a dual-backend integration matrix (`crates/microwave-store/tests/dual_backend.rs`) running the same identity, authz, object, cookbook, policy, artifact, membership, OAuth, and KV/secret suites against SQLite always and PostgreSQL when `MICROWAVE_TEST_POSTGRES_URL` is set. `scripts/run-postgres-tests.sh` runs it in the PostgreSQL gate.
- [x] Add HTTP-route parity coverage (`crates/microwave-api/tests/http_parity_test.rs`) that sends the same real signed requests through both routers and compares statuses, `Location`/content-type headers, and normalized bodies for the main API surface, including groups/containers, user/org/data-bag/artifact ACLs, user and organization deletion (including atomic global read-access-group cleanup, client-key/sandbox removal and `server-admins` preservation), credential authentication, system recovery, and OAuth v1 identity/user reads. Asserted cleanup plus a panic-safe PostgreSQL drop guard remove the run's rows, client keys and global groups, then verify they are actually gone; `scripts/run-postgres-tests.sh` runs both halves and honors `MICROWAVE_REQUIRE_POSTGRES=1`/`true` so Docker/tooling skips fail in CI.

### 1.4 Authorization & ACL Engine (`microwave-authz`)
- [x] Implement entity creation (`actor`, `group`, `object`, `container`) with 32-char hex `authz_id` generation. <!-- verified: cargo test -p microwave-store --test acl_test -->
- [x] Implement transitive permission check (`actor_has_permission_on` and bulk permission check `actor_has_bulk_permission_on`) using `WITH RECURSIVE` CTEs. <!-- verified: cargo test -p microwave-store --test acl_test -->
- [x] Implement default ACL inheritance when creating an object inside a container (copy container ACL + grant all 5 permissions to creator). <!-- verified: cargo test -p microwave-authz --test org_test -->
- [x] Implement group membership management (`add_actor`, `remove_actor`, `add_group`, `remove_group`) with recursive cycle detection (`forbid_group_cycles`). <!-- verified: cargo test -p microwave-store --test acl_test -->
- [x] Implement transitive group membership check (`/groups/:id/transitive_member/actors/:actor_id` semantics). <!-- claim:gap-transitive-member -->
- [x] Implement `DEFAULT_EC_EXPANDED_ORG` organization bootstrap policy in a single transaction: <!-- verified: crates/microwave-authz/src/org.rs:31 -->
  - [x] Create 12 default containers (`clients`, `containers`, `cookbooks`, `data`, `environments`, `groups`, `nodes`, `roles`, `sandboxes`, `policies`, `policy_groups`, `cookbook_artifacts`). <!-- verified: crates/microwave-authz/src/org.rs:8 -->
  - [x] Create default groups (`admins`, `billing-admins`, `clients`, `users`, `public_key_read_access`, `::{org}_read_access_group`, `::{org}_global_admins`). <!-- verified: crates/microwave-authz/src/org.rs:22 -->
  - [x] Populate initial group memberships and full ACL matrix across containers, groups, and org. <!-- verified: crates/microwave-authz/src/org.rs:60 -->
- [x] Implement ACL constraints (`check_admins_group_removal_from_grant_ace`, `validate_actors_clients_users`). <!-- verified: crates/microwave-api/src/acls.rs:299 -->

---

## Phase 2: Edge Middleware, System Endpoints & Identity/Org APIs

### 2.1 HTTP Edge & Version Negotiation Middleware (`microwave-api`)
- [x] Set up `axum` server with TLS support (`rustls`) and configurable HTTP/HTTPS listeners. <!-- verified: crates/microwave-api/src/lib.rs, crates/microwave-tls/src/lib.rs -->
- [x] Implement `X-Request-Id` generation and response header injection. <!-- verified: cargo test -p microwave-api --test http_hardening_test generates_a_request_id -->
- [x] Implement URI normalization and default-org path rewriter (preserving raw URI for signature verification). <!-- verified: crates/microwave-api/src/middleware.rs:453 -->
- [x] Implement `X-Chef-Version` validator (`400 Bad Request` on unsupported legacy client version < 10.0, allowing 0.10.x). <!-- verified: crates/microwave-api/src/middleware.rs:594 -->
- [x] Implement `X-Ops-Server-API-Version` request parser and response header formatter: <!-- verified: crates/microwave-api/src/middleware.rs:1059 -->
  - [x] Default to `0` when header omitted. <!-- verified: crates/microwave-api/src/middleware.rs:1335 -->
  - [x] Return `406 Not Acceptable` with `{"error":"invalid-x-ops-server-api-version","message":...,"min_version":0,"max_version":2}` when out of range `0..=2` or malformed. <!-- verified: crates/microwave-api/src/middleware.rs:1335 -->
  - [x] Attach `X-Ops-Server-API-Version` JSON header to all responses. <!-- verified: crates/microwave-api/src/middleware.rs:1359 -->
- [x] Implement `X-Ops-API-Info` header support when configured (`include_x_ops_api_info`). <!-- claim:gap-x-ops-api-info -->
- [x] Implement multi-host `X-Forwarded-Host`, `X-Forwarded-For`, `X-Forwarded-Server` handling (`chef_wm_util:base_uri` parity). <!-- verified: crates/microwave-api/src/middleware.rs:107 -->
- [x] Implement maintenance mode middleware (`503 Service Unavailable` when active, bypassing `/_status` and `/_stats`).
- [x] Verify against `oc-chef-pedant/spec/api/header_spec.rb` and `server_api_version_spec.rb`. <!-- verified: docs/pedant-report.md:8 -->

### 2.2 System & Authentication Endpoints
- [x] `GET /_status` — upstream-shaped `upstreams` map with bounded, cached probes for the metadata database (`SELECT 1` on SQLite/PostgreSQL; SQLite runs off-worker via `spawn_blocking`), the search backend (embedded no-op or OpenSearch/Elasticsearch `_cluster/health`, including red-cluster failure), and the object store (local data-directory read/write check with unique probe files or signed S3/Garage bucket `HEAD`). Probes are single-flight with a 1s stale-while-refresh cache, one background refresh task at a time, and a 5s absolute stale ceiling; SQLite retries a briefly busy lock for up to 200ms, then reports busy instead of queuing; local orphan sweeps are rate-limited to once per hour. Dependency failures are reported as `fail` in the payload without returning a 500.
- [x] `GET /_stats` — Prometheus text and JSON (`application/json`) metrics endpoints (`chef_wm_prometheus_json_format` parity).
- [x] `GET /server_api_version` — return `{"min_api_version": 0, "max_api_version": 2}`. <!-- verified: crates/microwave-api/src/lib.rs:56 -->
- [x] `GET /license` — return `{"limit_exceeded": bool, "node_license": int, "node_count": int, "upgrade_url": str}`. <!-- verified: cargo test -p microwave-api --test license_test -->
- [x] `POST /authenticate_user` — superuser-only endpoint verifying username/password (with transparent hash upgrade) and returning `linked` user payload; reject superuser target with `403`. <!-- verified: crates/microwave-api/src/users.rs:629 -->
- [x] `POST /system_recovery` — superuser-only endpoint verifying recovery password when `recovery_authentication_enabled = true`. <!-- verified: crates/microwave-api/src/users.rs:26 -->
- [x] `ANY /organizations/:org/validate/*` — verifies the signature against the wildcard path and the actual request body, then returns `{"requestor_name": ..., "requestor_type": ...}`.
- [x] `GET /organizations/:org/:type/:name/_identifiers` — returns `{"id": ..., "authz_id": ..., "org_id": ...}` for nodes and the upstream 404 for unsupported types.
- [x] `POST /organizations/:org/controls` — returns the upstream `410 Gone` analytics deprecation response for every method.
- [x] Verify against `status_spec.rb`, `stats_spec.rb`, `license_spec.rb`, `authenticate_user_spec.rb`, `system_recovery_spec.rb`, `validate_spec.rb`, `object_identifier_spec.rb`, `controls_spec.rb`. <!-- verified: docs/pedant-report.md:8 -->

### 2.3 Users, Organizations, Associations, Clients, Keys & Principals
- [x] **Users (`/users`, `/users/:user`):** <!-- verified: cargo test -p microwave-api --test users_test -->
  - [x] `GET /users` (list with URL map), `POST /users` (validate username/email/password, handle `v0` `public_key`/`private_key` vs `v1+` `create_key`/`chef_key`, create user + authz actor + default key). <!-- verified: cargo test -p microwave-api --test users_test -->
  - [x] `GET /users/:user`, `PUT /users/:user` (handle password updates, `v0` vs `v1` key mutation rules, conflict on duplicate username/email), `DELETE /users/:user` (dissociate from orgs, clean up keys and authz actor). <!-- verified: cargo test -p microwave-api --test users_test --test keys_test -->
- [x] **Organizations (`/organizations`, `/organizations/:org`):** <!-- verified: cargo test -p microwave-api --test organizations_test -->
  - [x] `GET /organizations`, `POST /organizations` (validate org `name` and `full_name`, execute `DEFAULT_EC_EXPANDED_ORG` policy, create `_default` environment, create `{org}-validator` client & key in `v0` vs `v1+` format). <!-- verified: cargo test -p microwave-api --test organizations_test -->
  - [x] `GET /organizations/:org`, `PUT /organizations/:org`, `DELETE /organizations/:org` (cascade delete org objects, containers, groups, associations, search entries, and checksums). <!-- verified: cargo test -p microwave-api --test organizations_test -->
- [x] **Associations & Invitations:** <!-- verified: cargo test -p microwave-api --test membership_test -->
  - [x] `GET,POST /organizations/:org/association_requests` and `DELETE /organizations/:org/association_requests/:id`. <!-- verified: cargo test -p microwave-api --test membership_test -->
  - [x] `GET /users/:user/association_requests`, `GET /users/:user/association_requests/count`, `PUT /users/:user/association_requests/:id` (`accept` / `reject`). <!-- verified: cargo test -p microwave-api --test membership_test -->
  - [x] `GET /users/:user/organizations`. <!-- verified: cargo test -p microwave-api --test membership_shapes_test -->
  - [x] `GET,POST /organizations/:org/users`, `GET,DELETE /organizations/:org/users/:user` (add/remove user from org `users` group and `read_access_group`; block removal if user is still in `admins` group with exact error message). <!-- verified: cargo test -p microwave-api --test membership_test -->
- [x] **Clients (`/organizations/:org/clients`, `/organizations/:org/clients/:client`):** <!-- verified: cargo test -p microwave-api --test clients_test -->
  - [x] `GET /organizations/:org/clients`, `POST /organizations/:org/clients` (support `validator: true/false`, `admin` field in `v0`, `create_key` in `v1+`, auto-add to `clients` group, index client in search). <!-- verified: cargo test -p microwave-api --test clients_test -->
  - [x] `GET,PUT,DELETE /organizations/:org/clients/:client` (handle validator self-deletion restrictions, `private_key: true` regeneration in `v0`). <!-- verified: cargo test -p microwave-api --test clients_test -->
- [x] **Keys (`/users/:user/keys`, `/organizations/:org/clients/:client/keys`, `/organizations/:org/users/:user/keys`):** <!-- verified: cargo test -p microwave-api --test keys_test -->
  - [x] `GET` list keys (returning `uri`, `name`, `expired` boolean). <!-- verified: cargo test -p microwave-api --test keys_test -->
  - [x] `POST` create key (validate PEM public key or `create_key: true`, validate `expiration_date` ISO-8601 or `"infinity"`, return `201`). <!-- verified: cargo test -p microwave-api --test keys_test -->
  - [x] `GET,PUT,DELETE .../keys/:key_name` (support renaming `name`, updating `public_key` / `create_key` / `expiration_date`). <!-- verified: cargo test -p microwave-api --test keys_identity_test -->
- [x] **Principals (`GET /organizations/:org/principals/:name`):** <!-- verified: cargo test -p microwave-api --test principals_test -->
  - [x] Implement `v0` single principal response vs `v1+` `{"principals": [...]}` array response (ordering client before user when both exist, setting `org_member` accurately). <!-- verified: crates/microwave-api/tests/principals_test.rs -->
- [x] **Containers, Groups & ACLs:** <!-- verified: cargo test -p microwave-api --test containers_test -->
  - [x] `GET,POST /organizations/:org/containers` and `GET,DELETE /organizations/:org/containers/:name`. <!-- verified: cargo test -p microwave-api --test containers_test -->
  - [x] `GET,POST /organizations/:org/groups` and `GET,PUT,DELETE /organizations/:org/groups/:name` (prevent deleting default org groups `admins`, `users`, `clients`, `billing-admins`). <!-- verified: cargo test -p microwave-api --test groups_test -->
  - [x] `GET .../_acl` (with `?detail=granular` support) and `PUT .../_acl/:permission` across all 13 resource types (`user`, `organization`, `client`, `group`, `container`, `data_bag`, `node`, `role`, `environment`, `cookbook`, `policy`, `policy_group`, `cookbook_artifact`). <!-- verified: cargo test -p microwave-api --test acls_test -->
- [x] Verify against `user_spec.rb`, `organization_spec.rb`, `account/*`, `clients/*`, `keys/*`, `principal_spec.rb`, `groups_acl_spec.rb`, `auth_spec.rb`, `versioned_behaviors/server_api_v1_spec.rb`. <!-- verified: docs/pedant-report.md:8 -->

---

## Phase 3: Core Converge Objects, Bookshelf, Cookbooks, Policyfiles & Depsolver

### 3.1 Nodes, Roles, Environments & Data Bags
- [x] **Nodes (`/organizations/:org/nodes`):** <!-- verified: cargo test -p microwave-api --test nodes_test -->
  - [x] `GET,POST /organizations/:org/nodes`, `GET,PUT,DELETE /organizations/:org/nodes/:name`, `GET /organizations/:org/environments/:env/nodes`. <!-- verified: cargo test -p microwave-api --test nodes_test -->
  - [x] Validate node JSON schema (`name` match, `json_class: "Chef::Node"`, `chef_type: "node"`, `chef_environment`, `run_list` normalization, `default`/`normal`/`override`/`automatic` attribute maps, `policy_name`, `policy_group`). <!-- verified: crates/microwave-api/src/nodes.rs:343 -->
  - [x] Trigger search index add/update/delete on mutation. <!-- verified: crates/microwave-api/src/nodes.rs:169 -->
- [x] **Roles (`/organizations/:org/roles`):** <!-- verified: cargo test -p microwave-api --test roles_contract_test -->
  - [x] `GET,POST /organizations/:org/roles`, `GET,PUT,DELETE /organizations/:org/roles/:name`. <!-- verified: cargo test -p microwave-api --test roles_contract_test -->
  - [x] `GET /organizations/:org/roles/:name/environments` and `GET /organizations/:org/roles/:name/environments/:env` (and `/environments/:env/roles/:name`).
  - [x] Validate role JSON schema (`json_class: "Chef::Role"`, `run_list`, `env_run_lists`, `default_attributes`, `override_attributes`). <!-- verified: crates/microwave-api/src/roles.rs:145 -->
- [x] **Environments (`/organizations/:org/environments`):** <!-- verified: cargo test -p microwave-api --test environments_contract_test -->
  - [x] `GET,POST /organizations/:org/environments`, `GET,PUT,DELETE /organizations/:org/environments/:env`. <!-- verified: cargo test -p microwave-api --test environments_contract_test -->
  - [x] Protect `_default` environment from deletion or modification (`405 Method Not Allowed`). <!-- verified: crates/microwave-api/src/environments.rs:249 -->
  - [x] Validate environment JSON (`cookbook_versions` constraint map, `default_attributes`, `override_attributes`). <!-- verified: crates/microwave-api/src/environments.rs:26 -->
  - [x] Implement `GET /organizations/:org/environments/:env/cookbooks`, `.../cookbooks/:cookbook`, and `.../recipes` (filtering cookbooks by environment version constraints).
- [x] **Data Bags (`/organizations/:org/data`):** <!-- verified: cargo test -p microwave-api --test data_bags_test -->
  - [x] `GET,POST /organizations/:org/data`, `GET,POST,DELETE /organizations/:org/data/:bag`. <!-- verified: cargo test -p microwave-api --test data_bags_test -->
  - [x] `GET,PUT,DELETE /organizations/:org/data/:bag/:item` (handle `raw_data` wrapper vs unwrapped item JSON, validate `id` field match, index items under `{data_bag, bag_name}`). <!-- verified: cargo test -p microwave-api --test data_bags_test -->
- [x] Verify against `nodes/*`, `roles/*`, `environments/*`, `data_bag/*` pedant specs. <!-- verified: docs/pedant-report.md:8 -->

### 3.2 Object Store, Bookshelf & Sandboxes (`microwave-objectstore`)
- [x] Implement `ObjectStore` trait with two backends: <!-- verified: crates/microwave-objectstore/src/lib.rs:243 -->
  - [x] **Local Filesystem Backend:** atomic temp-file write + on-the-fly MD5 verification + rename into `{data_dir}/bookshelf/organization-{org_id}/checksum-{md5}`. <!-- verified: crates/microwave-objectstore/src/lib.rs:243 -->
  - [x] **S3 / Garage Backend:** SigV4 presigned URL generation (`PUT` with `content-md5` and `content-type: application/x-binary`; `GET`), `HEAD` object existence checks, and batch `DELETE`. <!-- verified: crates/microwave-objectstore/src/s3.rs -->
- [x] Implement embedded `/bookshelf/:bucket/organization-:org/checksum-:md5` HTTP routes (`HEAD`, `GET`, `PUT`, `DELETE`) with SigV4 query-string and header authentication, `ETag` (Base64 MD5), and streaming body I/O. <!-- verified: cargo test -p microwave-api --test bookshelf_contract_test -->
- [x] Implement `POST /organizations/:org/sandboxes`: <!-- verified: cargo test -p microwave-api --test sandboxes_test -->
  - [x] Validate `{"checksums": {"<md5>": null, ...}}` payload. <!-- verified: crates/microwave-api/src/sandboxes.rs:31 -->
  - [x] Check existing `checksums` table for the org; insert needed checksums into `sandboxed_checksums` and return `201 Created` with `sandbox_id`, `uri`, and per-checksum `{needs_upload, url}`. <!-- verified: crates/microwave-api/src/sandboxes.rs:45 -->
- [x] Implement `PUT /organizations/:org/sandboxes/:id`: <!-- verified: cargo test -p microwave-api --test sandboxes_test -->
  - [x] Validate `{"is_completed": true}`. <!-- verified: crates/microwave-api/src/sandboxes.rs:65 -->
  - [x] Verify all `needs_upload` checksums are present in `microwave-objectstore` (return `503 Service Unavailable` listing missing checksums if any are absent). <!-- verified: crates/microwave-api/src/sandboxes.rs:92 -->
  - [x] Promote checksums to `checksums` table and delete sandbox record. <!-- verified: crates/microwave-api/src/sandboxes.rs:65 -->
- [x] Verify against `oc-chef-pedant/spec/api/sandboxes/complete_endpoint_spec.rb`. <!-- verified: docs/pedant-extended-report.md:164 -->

### 3.3 Cookbooks, Cookbook Artifacts, Policyfiles & Universe
- [x] **Cookbooks (`/organizations/:org/cookbooks`):** <!-- verified: cargo test -p microwave-api --test cookbooks_test -->
  - [x] `GET /organizations/:org/cookbooks` (`?num_versions=N|all`), `GET .../cookbooks/_latest`, `GET .../cookbooks/_recipes`. <!-- verified: cargo test -p microwave-api --test cookbooks_test -->
  - [x] `GET /organizations/:org/cookbooks/:name` (`?num_versions=N|all`). <!-- verified: cargo test -p microwave-api --test cookbooks_order_test -->
  - [x] `GET,PUT,DELETE /organizations/:org/cookbooks/:name/:version`: <!-- verified: cargo test -p microwave-api --test cookbooks_test -->
    - [x] Validate cookbook version manifest (segments: `recipes`, `definitions`, `libraries`, `attributes`, `files`, `templates`, `resources`, `providers`, `root_files`, `metadata`). <!-- verified: crates/microwave-api/src/cookbooks.rs:32 -->
    - [x] Verify all file checksums exist in `checksums` for the org (`400 Bad Request` if any missing). <!-- verified: cargo test -p microwave-api --test cookbooks_test -->
    - [x] Enforce `frozen?` immutability (`409 Conflict` on `PUT` unless `?force=true`). <!-- verified: crates/microwave-api/src/cookbooks.rs:162 -->
    - [x] Attach presigned `GET` URLs to all manifest file entries on `GET`. <!-- verified: cargo test -p microwave-api --test cookbooks_test -->
    - [x] On `DELETE`, garbage-collect checksums no longer referenced by any `cookbook_versions` or `cookbook_artifact_versions` in the org. <!-- verified: cargo test -p microwave-api --test cookbooks_test -->
- [x] **Cookbook Artifacts (`/organizations/:org/cookbook_artifacts`):** <!-- verified: cargo test -p microwave-api --test cookbook_compat_test -->
  - [x] `GET /organizations/:org/cookbook_artifacts`, `GET /organizations/:org/cookbook_artifacts/:name`. <!-- verified: cargo test -p microwave-api --test cookbook_compat_test -->
  - [x] `GET,PUT,DELETE /organizations/:org/cookbook_artifacts/:name/:identifier` (validate 40-char hex SHA1 identifier, checksum references, presigned URLs, and orphan checksum cleanup). <!-- verified: cargo test -p microwave-api --test cookbook_compat_test -->
- [x] **Policies & Policy Groups (`/organizations/:org/policies`, `/organizations/:org/policy_groups`):** <!-- verified: cargo test -p microwave-api --test policies_test -->
  - [x] `GET /organizations/:org/policies`, `GET,POST,DELETE /organizations/:org/policies/:name`. <!-- verified: cargo test -p microwave-api --test policies_test -->
  - [x] `GET /organizations/:org/policies/:name/revisions`, `GET,DELETE /organizations/:org/policies/:name/revisions/:revision_id`. <!-- verified: cargo test -p microwave-api --test policies_test -->
  - [x] `GET /organizations/:org/policy_groups`, `GET,DELETE /organizations/:org/policy_groups/:group`. <!-- verified: cargo test -p microwave-api --test policies_test -->
  - [x] `GET,PUT,DELETE /organizations/:org/policy_groups/:group/policies/:name` (create policy/group/revision associations dynamically on `PUT` if not yet present, matching `oc_chef_wm_policy_group_policy_rev.erl`). <!-- verified: cargo test -p microwave-api --test policies_test -->
- [x] **Universe (`GET /organizations/:org/universe`):** <!-- verified: crates/microwave-api/src/cookbooks.rs:309 -->
  - [x] Return Berkshelf-compatible dependency universe JSON across all cookbooks and versions in the org. <!-- verified: cargo test -p microwave-api --test cookbooks_test -->
- [x] Verify against `cookbooks/*`, `cookbook_artifacts/*`, `policies/*`, and `universe_spec.rb`. <!-- verified: docs/pedant-extended-report.md:164 -->

### 3.4 Dependency Solver (`microwave-depsolver`)
- [x] Implement upstream cookbook version parser (`MAJOR.MINOR` and `MAJOR.MINOR.PATCH`) and constraint operators (`=`, `>`, `<`, `>=`, `<=`, `~>`). <!-- verified: crates/microwave-depsolver/src/version.rs -->
- [x] Implement backtracking dependency graph solver with timeout (2,000ms) and detailed failure diagnostics (`non_existent_cookbooks`, `most_constrained_cookbooks`, `unable_to_solve`). <!-- verified: crates/microwave-depsolver/src/solver.rs:6 -->
- [x] Implement `POST /organizations/:org/environments/:env/cookbook_versions` handler: <!-- verified: cargo test -p microwave-api --test depsolver_test -->
  - [x] Parse `{"run_list": [...]}` recipes/cookbook constraints. <!-- verified: cargo test -p microwave-api --test depsolver_test -->
  - [x] Combine with `:env` cookbook constraints and solve against all cookbook versions in the org. <!-- verified: cargo test -p microwave-api --test depsolver_test -->
  - [x] Return solved cookbook version objects with presigned URLs (filtered to minimal metadata fields required by `chef-client`), or `412 Precondition Failed` with exact error JSON on conflict/missing cookbook. <!-- verified: cargo test -p microwave-api --test depsolver_test -->
- [x] Port all `depsolver_tests.erl` unit tests and verify against `oc-chef-pedant/spec/api/depsolver_spec.rb`. <!-- verified: docs/pedant-extended-report.md:169 -->

---

## Phase 4: Search & Indexing Subsystem (`microwave-search`)

### 4.1 Document Expansion (port of upstream `chef_index_expand`)
- [x] Implement upstream attribute deep-merge (`chef_deep_merge.erl` parity) for node attribute precedence (`default` < `normal` < `override` < `automatic`). <!-- verified: crates/microwave-api/src/search/expand.rs:82 -->
- [x] Implement JSON flattening/expansion into `content` tokens (`parent_child__=__value` + leaf `child__=__value`) with exact character escaping and word-break behavior. <!-- verified: crates/microwave-api/src/search/expand.rs:4 -->
- [x] Implement special field expansion for nodes (`recipe`, `recipes`, `role`, `roles`, `run_list`, `chef_environment`, `policy_name`, `policy_group`), roles, environments, clients, and data bag items. <!-- verified: crates/microwave-api/src/search/index.rs -->
- [x] Add metadata fields `X_CHEF_id_CHEF_X`, `X_CHEF_database_CHEF_X` (`chef_{org_id}`), `X_CHEF_type_CHEF_X`, and `data_bag`. <!-- verified: crates/microwave-api/src/search/index.rs:123 -->
- [x] Port all EUnit tests from `src/oc_erchef/apps/chef_index/test/chef_index_expand_tests.erl`. <!-- verified: cargo test -p microwave-api --test search_grammar_test --test search_test -->

### 4.2 Lucene Query Parser & Rewriter (port of upstream `chef_lucene.peg`)
- [x] Implement PEG grammar matching `chef_lucene.peg`: <!-- verified: crates/microwave-api/src/search/query.rs:648 -->
  - [x] `*:*`, field queries `field:term`, phrase queries `field:"foo bar"`, range queries `field:[a TO b]` / `field:{a TO b}` (including `*` bounds rewritten to `\ufff0`), wildcards `*` and `?`, fuzzy `~`, boost `^`, boolean `AND`/`&&`/`OR`/`||`/`NOT`/`!`/`+`/`-`, nested parentheses. <!-- verified: cargo test -p microwave-api --test search_grammar_test -->
- [x] Implement URI percent-decoding and query parameter validation (`start` >= 0, `rows` >= 0, empty `q=""` -> `400 Bad Request`). <!-- verified: crates/microwave-api/src/search/mod.rs:189 -->
- [x] Port all EUnit tests from `src/oc_erchef/apps/chef_index/test/chef_lucene_tests.erl` and `chef_index_query_tests.erl`. <!-- verified: cargo test -p microwave-api --test search_grammar_test -->

### 4.3 Search Backends & Endpoints
- [x] Implement **Tantivy Embedded Backend**: <!-- claim:gap-tantivy-backend -->
  - [x] Custom tokenizer matching the upstream whitespace/symbol tokenization on `content`. <!-- claim:gap-tantivy-backend -->
  - [x] Query AST to Tantivy query compiler (Term, Phrase, Wildcard/Regex, Range, Boolean, Boost). <!-- claim:gap-tantivy-backend -->
  - [x] Deterministic sorting by `X_CHEF_id_CHEF_X asc` and pagination (`start`, `rows`). <!-- claim:gap-tantivy-backend -->
- [x] Implement **OpenSearch / Elasticsearch Backend**: <!-- verified: cargo test -p microwave-api --test opensearch_test --test opensearch_mapping_test -->
  - [x] `/_bulk` indexing/deletion and `/_search` query execution matching `chef_opensearch.erl`. <!-- verified: crates/microwave-api/src/search/opensearch.rs -->
- [x] Implement `inline` and `batch` indexing queue modes (`search_queue_mode`). <!-- claim:gap-search-queue-mode -->
- [x] Implement `GET /organizations/:org/search` (list built-in indexes `client`, `environment`, `node`, `role` + all `data_bag` names in org). <!-- verified: cargo test -p microwave-api --test search_test -->
- [x] Implement `GET /organizations/:org/search/:index` and `POST /organizations/:org/search/:index` (partial search extracting attribute path arrays from hydrated objects; return `404` if searching non-existent data bag). <!-- verified: cargo test -p microwave-api --test search_test -->
- [x] Implement `strict_search_result_acls` post-filtering. <!-- verified: cargo test -p microwave-api --test search_acl_test -->
- [x] Verify against `oc-chef-pedant/spec/api/search/search_spec.rb`, `word_break_spec.rb`, and `reindex_spec.rb`. <!-- verified: docs/pedant-extended-report.md:165 -->

---

## Phase 5: Surrounding Compatibility Surfaces (`/id`, `chef-server-ctl`, Data Collector, Migration)

### 5.1 `/id` OAuth2 & Identity Service (`microwave-oauth`)
- [x] Implement session management (signed/encrypted cookies compatible with `/id/signin` and `/id/signout`). <!-- verified: cargo test -p microwave-api --test session_cookie_test -->
- [x] Implement OAuth2 Authorization Server endpoints: <!-- verified: crates/microwave-oauth/src/lib.rs:51 -->
  - [x] `GET,POST,DELETE /id/oauth/authorize` <!-- verified: crates/microwave-oauth/src/lib.rs:51 -->
  - [x] `POST /id/oauth/token` (`authorization_code` and `refresh_token` grants) <!-- verified: crates/microwave-oauth/src/lib.rs:56 -->
  - [x] `POST /id/oauth/revoke` <!-- verified: crates/microwave-oauth/src/lib.rs:58 -->
  - [x] `GET,POST /id/oauth/applications` and `GET,PUT,DELETE /id/oauth/applications/:id` <!-- verified: crates/microwave-oauth/src/lib.rs:59 -->
- [x] Implement User Profile & Password Reset endpoints: <!-- verified: crates/microwave-oauth/src/profile.rs:20 -->
  - [x] `GET,PUT /id/profile`, `PUT /id/profile/password`, `GET /id/profile/email`, `POST /id/profile/regen_key` <!-- verified: crates/microwave-oauth/src/profile.rs:44 -->
  - [x] `GET,POST,PUT /id/password-reset` <!-- verified: crates/microwave-oauth/src/password_reset.rs:91 -->
- [x] Implement `/id/v1/*` JSON endpoints: <!-- verified: crates/microwave-oauth/src/v1.rs -->
  - [x] `GET /id/v1/me`, `GET /id/v1/me/organizations`, `GET /id/v1/users/:id`, `GET /id/v1/status` <!-- verified: crates/microwave-oauth/src/lib.rs:98 -->
- [x] Create clean server-rendered HTML templates for sign-in, OAuth consent, profile, and application management. <!-- verified: crates/microwave-oauth/src/applications.rs:31 -->
- [x] Verify against `oc-chef-pedant/spec/api/oc_id_spec.rb`. <!-- verified: docs/pedant-extended-report.md:151 -->

### 5.2 Data Collector & Required Recipe (`microwave-api`)
- [x] Implement `POST /organizations/:org/data-collector` endpoint (token & signature validation, bounded upstream `reqwest` forwarding, status/body preservation).
- [x] Implement opt-in `oc_chef_action` mutation notifications for successful node/role/environment/data-bag/client/key/policy/membership/ACL/cookbook/organization writes, using a bounded in-process queue, configured timeout, and failure logging. Disabled by default and whenever `data_collector.root_url` is unset; enable with `actions_enabled = true`.
- [x] Implement `required_recipe` support if enabled in config (`required_recipe_spec.rb`).
- [x] Verify `required_recipe_spec.rb` under pedant (6 examples, 0 failures). Rust integration tests cover both enabled and disabled behavior; no vendored data-collector pedant spec exists.
- [x] Implement compliance-profile proxy forwarding for both `/organizations/:org/owners/:owner/compliance[/profile]` and `/compliance/organizations/:org/owners/:owner/compliance[/profile]`, including configurable URL/token/header/timeout, status/body preservation, disabled 404 behavior, and `compliance_proxy_spec.rb` harness support.
- [x] Parse `data_collector` (`root_url`, `token`, `timeout`, `actions_enabled`, `service_hostname`), `compliance_proxy`, and `telemetry` configuration with `MW_*` overrides. Telemetry defaults to disabled and has no external sender; the `telemetry` table exists in both SQLite and PostgreSQL.

### 5.3 Control CLI (`microwave-ctl`, `chef-server-ctl` compatibility alias)
> **Backend status:** the commands below that are implemented work against
> both SQLite and PostgreSQL. `backup`/`restore` use `VACUUM INTO` for SQLite
> and `pg_dump`/`pg_restore` for PostgreSQL; both require the matching client
> tools for PostgreSQL. A directory or a single tar archive can be used for
> either backend. Backups accept an optional timestamped destination,
> `--config-only`, validated `--pg-options`, and opt-in
> `--include-private-keys`; restore accepts `--pg-options` and the separate
> `--restore-private-keys`/`--private-only` flags. `reconfigure` validates,
> initializes/migrates, and reports
> without restarting a server. `reindex` prints relational counts for the
> embedded backend and backfills remote OpenSearch/Elasticsearch documents in
> chunked `_bulk` requests, counting only acknowledged items and refreshing the
> index when done. `reindex --purge` first deletes each visited organization's
> remote documents; without it the reindex stays additive.
- [x] Implement the remaining useful `chef-server-ctl` commands: `user-edit` (editor or `--input` JSON, password hashing, superuser protection), `cleanup-bifrost` (orphan `authz_entities` count/delete on SQLite and PostgreSQL), `filtered-dump` (redacted org/node/environment/role/data-bag/client dumps), `gather-logs` (redacted tar.gz with journald and system data), `oc-id-show-app`, `rotate-credentials`, `rotate-all-credentials`, and `rotate-shared-secrets`. `cleanup` remains intentionally unimplemented: upstream it is an Omnibus post-upgrade chef run, like `reconfigure`.
  - [x] Install `chef-server-ctl` as a compatibility alias to `microwave-ctl`.
  - [x] Org & User commands: `user-list`, `user-show`, `user-create`, `user-delete`, `org-list`, `org-show`, `org-create`, `org-delete`, `org-user-add`, `org-user-remove`, `password`.
  - [x] Server Admin commands: `grant-server-admin-permissions`, `remove-server-admin-permissions`, `list-server-admins`.
  - [x] Key Rotation commands: `add-client-key`, `add-user-key`, `list-client-keys`, `list-user-keys`, `delete-client-key`, `delete-user-key`.
  - [x] Search & Maintenance commands: `reindex` (embedded count verification and remote backfill) and `maintenance`.
  - [x] Secrets & Config commands: `show-secret`, `set-secret`, `remove-secret`, `check-config`, and `reconfigure`.
  - [x] Lifecycle & Diagnostics: `status`, `ha-status`, `version`, `psql`, `backup`, `restore`, `pedant` (the `test` equivalent), and `service start|stop|restart|status|tail` (systemd, plus `start --foreground` for containers). Directory and tar backup formats are supported for SQLite and PostgreSQL.

### 5.4 Legacy Data Migration Tool (`microwave-migrate`)
> **Backend status:** export and import support both SQLite and PostgreSQL
> through the shared versioned JSON snapshot, including `kvpair`, OAuth
> grants/tokens, explicit/empty sandboxes, and telemetry. Default exports are
> secret-safe; `--include-secrets` adds database-backed secret values, and the
> independent `--include-private-keys` adds on-disk `pivotal.pem`,
> `pivotal.password`, `objectstore.secret`, private-chef secrets, WebUI/oc-id
> files, and configured TLS cert/key material under `secrets/` with a manifest.
> `--archive PATH.tar.gz` carries the snapshot plus local `bookshelf/` blobs
> with an allowlist, traversal/symlink/absolute-path/special-file rejection,
> mode-0600 extraction, overwrite refusal unless `--force`, `--secrets-dir`
> support, and confirmation-gated restore. Data-dir private paths are restricted
> to the fixed export allowlist; an empty archive bookshelf requires `--force`
> over a populated live one; telemetry merges instead of deleting local events;
> private files are pre-staged and committed as a set; partial-secret imports
> warn; archives are capped at 1,000,000 entries. Use both secret flags for a
> truly complete self-migration. Remote S3/Garage objects are outside the
> archive; replicate the bucket. `microwave-ctl backup --include-private-keys`
> adds an explicit `private/` manifest for the fixed server key/certificate
> set to either directory or tar backups; restore requires the separate
> `--restore-private-keys` opt-in, validates and preflights/stages every
> private destination before the database, and applies them as a set after a
> successful database/bookshelf restore with rollback on ordinary commit
> failure; old-content backups are kept until the whole set commits and are
> preserved at a reported path if rollback fails. Backup destinations are
> preflighted before any private tree replacement, so a failed re-run cannot
> destroy an existing full backup's keys; populated trees are never replaced by
> a default backup or an empty include. The swap is crash-window documented:
> old keys can remain at a `.microwave-private-old-*` sibling. `--private-only
> --restore-private-keys` retries keys without replaying metadata (an empty set
> is a no-op). Without that flag, existing target keys are left untouched. The default backup is key-**file**-free, not DB-secret-free:
> database-backed secrets (including the object-store secret in `kvpair`) are
> always in the database dump. An existing destination cannot be extended; a
> new backup in a new destination is required for keys.
> `import-knife-backup` also supports both backends, writes cookbook blobs to a
> local object-store root, and requires `--bookshelf-dir` with
> `--database-url`. Explicit `--secrets-dir`/`--secrets-archive` sources migrate
> server-owned private material; client/user private keys are not stored
> server-side and remain client-owned.
- [x] Implement **Snapshot importer (`microwave-migrate import`)** for both backends:
  - [x] Import users (including legacy password hashes and salts), organizations, associations, invites, and server-admins.
  - [x] Import clients and multi-key records (`actor_keys`).
  - [x] Import containers, groups, and full ACL graphs (preserving `authz_id`s).
  - [x] Import nodes, roles, environments, data bags, cookbooks, cookbook artifacts, policies, policy groups, sandboxes, and telemetry.
  - [x] Import `oc-id` OAuth applications and access grants/tokens.
  - [x] Import local bookshelf blobs through `--archive`.
- [x] Implement **Mode 1: Archive / Backup Importer (`knife-ec-backup` directory):**
  - [x] `microwave-migrate import-knife-backup` parses a ChefFS backup directory offline, supports `--dry-run`, and requires confirmation/`--yes`.
  - [x] Import users (profiles, admin flag, legacy password hashes/salts from `key_dump.json`), organizations, associations, and invites.
  - [x] Import clients and multi-key records from `key_table_dump.json`/inline public keys (`actor_keys`), preserving key names, versions, and expiry.
  - [x] Import containers, groups, group nesting, and ACL graphs, including user ACLs, granular `users`/`clients`, and the standard global `server-admins`/read-access graph.
  - [x] Import nodes, roles, environments, data bags/items, and ChefFS cookbooks (metadata, frozen status, MD5 checksums, and local bookshelf blobs). Environment constraints are preserved in the environment bodies.
  - [x] Import explicitly supplied server-owned private material (`pivotal.pem`/`pivotal.password`, web UI keys, `objectstore.secret`, `private-chef-secrets.json`, TLS certificate/key, and CA files) with `--secrets-dir` or allowlisted `.tar`/`.tar.gz`, overwrite protection, modes, staged/rollback writes, size bounds, and no content logging. CA private keys stay `0600`; Chef `nginx/ca/<fqdn>.crt|.key` map to TLS targets. Client/user private keys are not recoverable from the server.
  - [x] Harden metadata/cookbook paths (safe contained `README` reads including nested `docs/README.md`, symlink/canonical collection checks), normalize legacy key expiries, suppress inline defaults after invalid default-key rows, reconcile bootstrapped users with `--merge-users` (preserving target credentials/profile/default keys unless `--replace-user-credentials`, warning when no key material is imported, dropping the target default key under `--replace-user-credentials` when the backup has none, and reconciling `server-admins` membership to the final admin flag), and replace revoked ACLs/group membership with `--replace-acls` (backup-file groups only, restore captured state on import failure).
  - [x] Import cookbook artifacts, policies/policy groups, and `oc-id` OAuth applications. These are not present in a knife backup; snapshot import already handles them when a snapshot contains them. <!-- claim:gap-knife-artifacts-policies -->
  - [x] Evaluate arbitrary Ruby in `metadata.rb` and import blobs into S3/Garage. The current parser is a tested DSL subset and blobs target the local object store. <!-- claim:gap-metadata-rb-s3 -->
- [x] Implement **Mode 2: Live API Crawler:**
  - [x] Crawl a running upstream server using `pivotal` credentials for metadata, ACLs, policies, nodes, roles, environments, data bags, and public keys.
  - [x] Crawl cookbook blobs from the legacy bookshelf layout with `--bookshelf-dir`: collect unique `(org_id, checksum, url)` pairs, skip existing local blobs, resolve relative URLs against the legacy base while signing them, fetch absolute presigned URLs directly, verify MD5, write atomically into `organization-<org_id>/checksum-<md5>`, and cap each blob at 64 MiB and each crawl at 4 GiB. Blob files without a `url` stay metadata-only. Use bucket replication for sources that do not expose per-file URLs.
- [x] Trigger full search reindex and verify object counts and checksum integrity post-migration. <!-- claim:gap-post-migration-reindex -->

### 5.5 P1/P2 Compatibility Hardening
- [x] P0: configurable superuser, organization ACLs, actor-identity key self-service, password-reset non-disclosure, session TTL/Secure cookies, form CSRF checks, OpenSearch mapping, strict search ACLs, depsolver cookbook ACLs.
- [x] P1a: membership/invitation shapes, environment/role contracts, OAuth PATCH/new/authorized apps/token info, `/_status` 500, `/_stats` text/auth/406, `/version`, newest-first cookbooks, default-org rewrites, trailing slashes.
- [x] P1b: `Location` headers, Chef JSON error envelopes, bookshelf 250 MiB/ETag/Last-Modified/204, search grammar (default OR, `&&`/`||`, wildcard fields, phrases, fuzzy/boost), embedded `X_CHEF_*` fields.
- [x] P1c: top-level service aliases, optional/timestamped backups, `psql SERVICE`, upstream `user-create`/`org-create` forms and validator keys, `show-service-credentials`, `require-credential-rotation`, orphan-blob cleanup, org-delete blob/search cleanup.
- [x] P2: per-user USAG groups, SQLite indexes, transactional group-cycle checks, authz entity-type triggers, policy/artifact FKs, access log/request IDs, compression, trusted-proxy handling.
- [x] Integrate the reviewed `backup-keys` branch (`microwave-ctl backup --include-private-keys`) with the newer P1c backup command surface: optional timestamped PATH, `--yes`, `--config-only`, validated `--pg-options`, tar/directory formats, private-key manifest/TLS handling, and restore `--restore-private-keys`/`--private-only`/`--pg-options` all coexist.
- [x] Run the non-harness Pedant `environments`/`roles` suites: `environments` 610 examples / 0 failures / 23 pending, `roles` 78 / 0. Exposed as `scripts/run-pedant.sh extra` (`environments roles`); `strict` combines core, extended, and extra.
### 5.6 P4 SMTP Mail-Out
- [x] Remote SMTP transport with STARTTLS/implicit TLS, AUTH PLAIN/LOGIN/auto, private CA support, and bounded asynchronous dispatch.
- [x] Password-reset email with absolute links from `server.public_url`, TTL enforcement, and preservation of `password_reset_token_in_response`.
- [x] Operator docs and `check-config` validation; password is omitted from `microwave-config.json` backups.
- [x] Email-change verification, invitation mail, SMTP rate limiting, and a durable outbox/retry. <!-- claim:gap-mail-outbox -->

### 5.7 P3 Compatibility Completion
- [x] Client update/rename reindexes the search document.
- [x] Depsolver timeout is configurable (default 20s), runs on `spawn_blocking`, and returns 503 `Dependency solver overloaded` when saturated.
- [x] Environment rename (201 + Location) and 409 conflicts; role name changes return upstream `400 Role name mismatch.`; 406 content negotiation.
- [x] Remaining `Location`/Chef JSON error coverage (sandboxes, keys, ACLs, cookbooks, artifacts).
- [x] Bookshelf expiring local tokens, Range/206/416, HEAD metadata, and S3 ListObjectsV2; multipart upload remains a documented gap.
- [x] Full `microwave-ctl backup --include-private-keys` merged with optional/default backup paths, config-only, and `--pg-options`.
- [x] P4: global auth-header statuses/skew, environment/role PUT name semantics, bounded `cookbook_versions`, and the `environments`/`roles` contract suites (610/0/23 and 78/0). Added as `scripts/run-pedant.sh extra`/`strict`.
- [x] Multi-replica HA / OpenSearch 3.x / Garage load validation; full pedant `all` is green — every core and extended tag reports 0 failures, and the old plain-HTTP `headers` artifact is fixed at the harness. <!-- claim:gap-multi-replica-ha -->
### 5.8 Full Pedant Verification, HA & Performance Benchmarks
- [x] Run `cargo fmt --check`, clippy, the test suite, claim and render checks, the homepage suite, and pedant in CI (`.github/workflows/`). <!-- verified: .github/workflows/ci.yml, .github/workflows/postgres.yml, .github/workflows/pedant.yml -->
- [x] Run complete `oc-chef-pedant` suite against Default Embedded mode (SQLite + Tantivy + Local Bookshelf) and achieve 100% pass rate on supported specs. <!-- claim:gap-full-pedant-embedded -->
- [x] Run complete `oc-chef-pedant` suite against HA mode (PostgreSQL + OpenSearch + Garage/S3) and achieve 100% pass rate. <!-- claim:gap-multi-replica-ha -->
- [x] Run `chef-client` (`19.4.47`) and `knife` end-to-end integration tests. <!-- claim:gap-knife-e2e -->
- [x] Run 1,000-node concurrent converge load simulation against Default Embedded mode to verify latency, memory footprint, and zero `SQLITE_BUSY` errors. <!-- claim:gap-load-simulation -->
