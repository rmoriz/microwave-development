# Microwave Server — Master Implementation TODO

This document tracks the end-to-end engineering checklist for reimplementing the upstream Chef Infra Server API (`15.10.127`) and its surrounding compatibility surfaces in Rust, as specified in [`docs/superpowers/specs/2026-10-24-microwave-server-design.md`](docs/superpowers/specs/2026-10-24-microwave-server-design.md).

> **Status snapshot (2026-10-25):** The implementation is well beyond the checkbox state below. The original phase checklist is kept as a design trace; many completed items were never ticked. Treat the test, pedant, and e2e reports as authoritative.
>
> **Implemented:** the Rust workspace; TOML/`MW_*` configuration; X-Ops signing/verification 1.0/1.1/1.3; SQLite and PostgreSQL metadata backends behind one contract, with a shared dual-backend repository matrix, and the HTTP API, OAuth, embedded search, authorization policy, bootstrap, operational CLI, and snapshot migration running on either; authz/ACL engine and default org policy; HTTP edge and signed-auth middleware; users, organizations, clients, principals, keys, memberships/invitations; nodes, roles, environments, data bags; sandboxes, `/bookshelf`, local and S3/Garage object stores; cookbooks, cookbook artifacts, policyfiles/policy groups; full dependency solver; search with embedded and OpenSearch backends; `/id` OAuth; `microwave-ctl`/`chef-server-ctl` including reconfigure, batched/purge-capable reindex, pedant runner, systemd service management, Docker foreground mode, and tar backup; snapshot migration and the live `from-api` crawler; Docker/Compose/systemd packaging; HA backend wiring; TLS listener with SIGHUP/periodic certificate rotation, pre-generated key cache, transparent `serialized_object` compression, and a dual-backend HTTP route parity matrix.
>
> **Verified:** `cargo test --workspace`, clippy with `-D warnings`, and `cargo fmt --check` are green. PostgreSQL integration covers the store, HTTP runtime, operational CLI, and snapshot export/import against PostgreSQL 18, and the server can be started with `MW_DATABASE__DRIVER=postgres`. The Debian trixie container image builds and both binaries start. Core pedant: 651 passed / 1 failed / 8 pending; the single failure is the known plain-HTTP `header_spec.rb` artifact. Extended pedant (final, full `all` run): 3,284 passed / 0 failed / 7 pending across all twelve tags. The complete Core+Extended run reports only the known plain-HTTP `headers` artifact; every other tag is green. A strict stock `chef-client` converge now passes with both compatibility shims disabled (`MW_E2E_COMPAT=0 MW_E2E_SYNTH_V2=0`), including validator registration.
>
> **Still open:** keeping the extended pedant suite green in CI; extending HTTP parity beyond the current matrix to OAuth HTML session/profile pages and remote OpenSearch/Elasticsearch; OpenSearch 3.x evaluation; HA/performance validation; outbound telemetry export (intentionally disabled, no phone-home); and non-systemd service supervision. `reconfigure`, remote `reindex` (batched `_bulk`, refresh, and optional `--purge`), the pedant runner, systemd `service` management, Docker foreground mode, and the tar backup format are implemented. Data Collector forwarding, `oc_chef_action` mutation notifications, the compliance profile proxy, `required_recipe`, maintenance 503, metrics/`_stats`, transparent compression, TLS (including SIGHUP/periodic certificate rotation), and the keygen cache are implemented.

---

## Phase 1: Workspace Foundation, Configuration, Cryptography & Storage Core

### 1.1 Cargo Workspace & Configuration (`microwave-config`)
- [ ] Initialize root `Cargo.toml` workspace with crates: `microwave-config`, `microwave-crypto`, `microwave-domain`, `microwave-authz`, `microwave-store`, `microwave-search`, `microwave-objectstore`, `microwave-depsolver`, `microwave-api`, `microwave-oauth`, `microwave-ctl`, `microwave-migrate`, and binary crates `microwave-server` and `microwave-ctl` (installing a `chef-server-ctl` compatibility alias).
- [ ] Document and enforce the naming/trademark policy: no `chef-` prefix for project crates, binaries, modules, services, or public types; use upstream names only where required for compatibility.
- [ ] Implement configuration schema (`Config`) supporting TOML config file, environment variable overrides, and basic `/etc/opscode/chef-server.rb` key-value extraction for drop-in compatibility.
- [ ] Implement secrets management (`private-chef-secrets.json` reader/writer + environment variable fallback `CHEF_SECRETS_DATA`) for `pivotal` superuser key, `webui_pub`/`webui_priv`, `bookshelf` access/secret keys, `oc_id` secret_key_base, and `data_collector` token.
- [ ] Implement automatic bootstrap initialization on first start: generate `pivotal` RSA keypair, `webui` RSA keypair, bookshelf HMAC credentials, and seed the `pivotal` superuser in the database.
- [x] Add structured logging (`tracing` + `tracing-subscriber`) and Prometheus/JSON metrics registry.

### 1.2 Cryptography, Request Signing & Passwords (`microwave-crypto`)
- [x] Implement `X-Ops-Content-Hash` calculation and verification:
  - [x] SHA-1 Base64 digest for Sign `1.0` and `1.1`.
  - [x] SHA-256 Base64 digest for Sign `1.3`.
- [x] Implement canonical path normalization (collapsing duplicate slashes `/+`, stripping trailing slashes except `/`, ignoring query strings, preserving original path before default-org rewrite).
- [x] Implement `X-Ops-Sign` header parser (`version=1.0`, `version=1.1`, `version=1.3`, with optional `algorithm=...;` prefix).
- [ ] Implement `X-Ops-Authorization-1..N` header reassembly and Base64 decoding.
- [ ] Implement timestamp skew validator (`X-Ops-Timestamp` ISO-8601 UTC parsing, configurable `authentication_skew` default 900s).
- [x] Implement Sign `1.0` & `1.1` verifier (RSA PKCS#1 v1.5 public-key decryption / raw verification against canonical string).
- [x] Implement Sign `1.3` verifier (`RSASSA-PKCS1-v1_5` with SHA-256 against canonical string including `X-Ops-Server-API-Version`).
- [x] Implement outbound `X-Ops-Sign` request signer (for `microwave-ctl`, `microwave-migrate`, and test harnesses).
- [ ] Implement AWS SigV4 signer & verifier (both query-string presigned URLs `X-Amz-Algorithm=AWS4-HMAC-SHA256` and `Authorization: AWS4-HMAC-SHA256` headers) for Bookshelf/S3 compatibility.
- [x] Implement `chef_password` verifier and upgrader:
  - [x] Standard `bcrypt` and `erlang-bcrypt-0.5.0`.
  - [x] `SHA1-bcrypt` (`bcrypt(hex_sha1(salt + "--" + pass + "--"))`).
  - [x] `sha1+bcrypt` (`bcrypt(hex_sha1("--" + orig_salt + "--" + pass + "--"), bcrypt_salt)`).
  - [x] Legacy plain salted SHA-1 (`ec` and `osc` formats).
  - [x] Constant-time comparison and automatic re-hash to `bcrypt` on verify.
- [x] Implement `KeygenCache` background worker (pre-generating 2048-bit RSA PEM keypairs in a bounded queue) and health reporting for `/_status`.
- [x] Port unit test vectors from `mixlib-authentication`, `chef_authn`, and `chef_password_tests.erl`.

### 1.3 Unified Database Schema & Store Providers (`microwave-store`)
- [ ] Define `MetadataStore` and transactional unit-of-work traits covering all domain and authz operations.
- [ ] Author unified SQL schema migrations compatible with both SQLite 3.35+ and PostgreSQL 14+:
  - [ ] `orgs`, `users`, `org_user_associations`, `org_user_invites`, `clients`, `actor_keys`.
  - [ ] `authz_entities`, `authz_acl_actor`, `authz_acl_group`, `authz_group_actors`, `authz_group_groups`, `containers`, `groups`.
  - [ ] `nodes`, `roles`, `environments`, `data_bags`, `data_bag_items`.
  - [ ] `checksums`, `sandboxed_checksums`, `cookbooks`, `cookbook_versions`, `cookbook_version_checksums`, `cookbook_artifacts`, `cookbook_artifact_versions`, `cookbook_artifact_version_checksums`.
  - [ ] `policies`, `policy_groups`, `policy_revisions`, `policy_group_revision_associations`.
  - [ ] `oauth_applications`, `oauth_access_grants`, `oauth_access_tokens`, `kvpair`, `telemetry`.
- [ ] Implement SQLite backend (`WAL` journal mode, `foreign_keys = ON`, `busy_timeout = 5000`, serialized write connection + read pool).
- [ ] Implement PostgreSQL backend (`sqlx::PgPool`, advisory lock helper for HA background tasks).
- [x] Implement transparent compression for `serialized_object` columns using the custom MWZ1 envelope (`mwz1:` + base64(zlib(JSON))). This is not byte-level upstream compatibility; legacy plain JSON remains readable. Configure with `storage.compression = "zlib" | "none"` / `MW_STORAGE__COMPRESSION`, default `zlib`.
- [x] Write a dual-backend integration matrix (`crates/microwave-store/tests/dual_backend.rs`) running the same identity, authz, object, cookbook, policy, artifact, membership, OAuth, and KV/secret suites against SQLite always and PostgreSQL when `MICROWAVE_TEST_POSTGRES_URL` is set. `scripts/run-postgres-tests.sh` runs it in the PostgreSQL gate.
- [x] Add HTTP-route parity coverage (`crates/microwave-api/tests/http_parity_test.rs`) that sends the same real signed requests through both routers and compares statuses, `Location`/content-type headers, and normalized bodies for the main API surface, including groups/containers, user/org/data-bag/artifact ACLs, user and organization deletion (including atomic global read-access-group cleanup, client-key/sandbox removal and `server-admins` preservation), credential authentication, system recovery, and OAuth v1 identity/user reads. Asserted cleanup plus a panic-safe PostgreSQL drop guard remove the run's rows, client keys and global groups, then verify they are actually gone; `scripts/run-postgres-tests.sh` runs both halves and honors `MICROWAVE_REQUIRE_POSTGRES=1`/`true` so Docker/tooling skips fail in CI.

### 1.4 Authorization & ACL Engine (`microwave-authz`)
- [ ] Implement entity creation (`actor`, `group`, `object`, `container`) with 32-char hex `authz_id` generation.
- [ ] Implement transitive permission check (`actor_has_permission_on` and bulk permission check `actor_has_bulk_permission_on`) using `WITH RECURSIVE` CTEs.
- [ ] Implement default ACL inheritance when creating an object inside a container (copy container ACL + grant all 5 permissions to creator).
- [ ] Implement group membership management (`add_actor`, `remove_actor`, `add_group`, `remove_group`) with recursive cycle detection (`forbid_group_cycles`).
- [ ] Implement transitive group membership check (`/groups/:id/transitive_member/actors/:actor_id` semantics).
- [ ] Implement `DEFAULT_EC_EXPANDED_ORG` organization bootstrap policy in a single transaction:
  - [ ] Create 12 default containers (`clients`, `containers`, `cookbooks`, `data`, `environments`, `groups`, `nodes`, `roles`, `sandboxes`, `policies`, `policy_groups`, `cookbook_artifacts`).
  - [ ] Create default groups (`admins`, `billing-admins`, `clients`, `users`, `public_key_read_access`, `::{org}_read_access_group`, `::{org}_global_admins`).
  - [ ] Populate initial group memberships and full ACL matrix across containers, groups, and org.
- [ ] Implement ACL constraints (`check_admins_group_removal_from_grant_ace`, `validate_actors_clients_users`).

---

## Phase 2: Edge Middleware, System Endpoints & Identity/Org APIs

### 2.1 HTTP Edge & Version Negotiation Middleware (`microwave-api`)
- [ ] Set up `axum` server with TLS support (`rustls`) and configurable HTTP/HTTPS listeners.
- [ ] Implement `X-Request-Id` generation and response header injection.
- [ ] Implement URI normalization and default-org path rewriter (preserving raw URI for signature verification).
- [ ] Implement `X-Chef-Version` validator (`400 Bad Request` on unsupported legacy client version < 10.0, allowing 0.10.x).
- [ ] Implement `X-Ops-Server-API-Version` request parser and response header formatter:
  - [ ] Default to `0` when header omitted.
  - [ ] Return `406 Not Acceptable` with `{"error":"invalid-x-ops-server-api-version","message":...,"min_version":0,"max_version":2}` when out of range `0..=2` or malformed.
  - [ ] Attach `X-Ops-Server-API-Version` JSON header to all responses.
- [ ] Implement `X-Ops-API-Info` header support when configured (`include_x_ops_api_info`).
- [ ] Implement multi-host `X-Forwarded-Host`, `X-Forwarded-For`, `X-Forwarded-Server` handling (`chef_wm_util:base_uri` parity).
- [x] Implement maintenance mode middleware (`503 Service Unavailable` when active, bypassing `/_status` and `/_stats`).
- [ ] Verify against `oc-chef-pedant/spec/api/header_spec.rb` and `server_api_version_spec.rb`.

### 2.2 System & Authentication Endpoints
- [x] `GET /_status` — upstream-shaped `upstreams` map with bounded, cached probes for the metadata database (`SELECT 1` on SQLite/PostgreSQL; SQLite runs off-worker via `spawn_blocking`), the search backend (embedded no-op or OpenSearch/Elasticsearch `_cluster/health`, including red-cluster failure), and the object store (local data-directory read/write check with unique probe files or signed S3/Garage bucket `HEAD`). Probes are single-flight with a 1s stale-while-refresh cache, one background refresh task at a time, and a 5s absolute stale ceiling; SQLite retries a briefly busy lock for up to 200ms, then reports busy instead of queuing; local orphan sweeps are rate-limited to once per hour. Dependency failures are reported as `fail` in the payload without returning a 500.
- [x] `GET /_stats` — Prometheus text and JSON (`application/json`) metrics endpoints (`chef_wm_prometheus_json_format` parity).
- [ ] `GET /server_api_version` — return `{"min_api_version": 0, "max_api_version": 2}`.
- [ ] `GET /license` — return `{"limit_exceeded": bool, "node_license": int, "node_count": int, "upgrade_url": str}`.
- [ ] `POST /authenticate_user` — superuser-only endpoint verifying username/password (with transparent hash upgrade) and returning `linked` user payload; reject superuser target with `403`.
- [ ] `POST /system_recovery` — superuser-only endpoint verifying recovery password when `recovery_authentication_enabled = true`.
- [x] `ANY /organizations/:org/validate/*` — verifies the signature against the wildcard path and the actual request body, then returns `{"requestor_name": ..., "requestor_type": ...}`.
- [x] `GET /organizations/:org/:type/:name/_identifiers` — returns `{"id": ..., "authz_id": ..., "org_id": ...}` for nodes and the upstream 404 for unsupported types.
- [x] `POST /organizations/:org/controls` — returns the upstream `410 Gone` analytics deprecation response for every method.
- [ ] Verify against `status_spec.rb`, `stats_spec.rb`, `license_spec.rb`, `authenticate_user_spec.rb`, `system_recovery_spec.rb`, `validate_spec.rb`, `object_identifier_spec.rb`, `controls_spec.rb`.

### 2.3 Users, Organizations, Associations, Clients, Keys & Principals
- [ ] **Users (`/users`, `/users/:user`):**
  - [ ] `GET /users` (list with URL map), `POST /users` (validate username/email/password, handle `v0` `public_key`/`private_key` vs `v1+` `create_key`/`chef_key`, create user + authz actor + default key).
  - [ ] `GET /users/:user`, `PUT /users/:user` (handle password updates, `v0` vs `v1` key mutation rules, conflict on duplicate username/email), `DELETE /users/:user` (dissociate from orgs, clean up keys and authz actor).
- [ ] **Organizations (`/organizations`, `/organizations/:org`):**
  - [ ] `GET /organizations`, `POST /organizations` (validate org `name` and `full_name`, execute `DEFAULT_EC_EXPANDED_ORG` policy, create `_default` environment, create `{org}-validator` client & key in `v0` vs `v1+` format).
  - [ ] `GET /organizations/:org`, `PUT /organizations/:org`, `DELETE /organizations/:org` (cascade delete org objects, containers, groups, associations, search entries, and checksums).
- [ ] **Associations & Invitations:**
  - [ ] `GET,POST /organizations/:org/association_requests` and `DELETE /organizations/:org/association_requests/:id`.
  - [ ] `GET /users/:user/association_requests`, `GET /users/:user/association_requests/count`, `PUT /users/:user/association_requests/:id` (`accept` / `reject`).
  - [ ] `GET /users/:user/organizations`.
  - [ ] `GET,POST /organizations/:org/users`, `GET,DELETE /organizations/:org/users/:user` (add/remove user from org `users` group and `read_access_group`; block removal if user is still in `admins` group with exact error message).
- [ ] **Clients (`/organizations/:org/clients`, `/organizations/:org/clients/:client`):**
  - [ ] `GET /organizations/:org/clients`, `POST /organizations/:org/clients` (support `validator: true/false`, `admin` field in `v0`, `create_key` in `v1+`, auto-add to `clients` group, index client in search).
  - [ ] `GET,PUT,DELETE /organizations/:org/clients/:client` (handle validator self-deletion restrictions, `private_key: true` regeneration in `v0`).
- [ ] **Keys (`/users/:user/keys`, `/organizations/:org/clients/:client/keys`, `/organizations/:org/users/:user/keys`):**
  - [ ] `GET` list keys (returning `uri`, `name`, `expired` boolean).
  - [ ] `POST` create key (validate PEM public key or `create_key: true`, validate `expiration_date` ISO-8601 or `"infinity"`, return `201`).
  - [ ] `GET,PUT,DELETE .../keys/:key_name` (support renaming `name`, updating `public_key` / `create_key` / `expiration_date`).
- [ ] **Principals (`GET /organizations/:org/principals/:name`):**
  - [ ] Implement `v0` single principal response vs `v1+` `{"principals": [...]}` array response (ordering client before user when both exist, setting `org_member` accurately).
- [ ] **Containers, Groups & ACLs:**
  - [ ] `GET,POST /organizations/:org/containers` and `GET,DELETE /organizations/:org/containers/:name`.
  - [ ] `GET,POST /organizations/:org/groups` and `GET,PUT,DELETE /organizations/:org/groups/:name` (prevent deleting default org groups `admins`, `users`, `clients`, `billing-admins`).
  - [ ] `GET .../_acl` (with `?detail=granular` support) and `PUT .../_acl/:permission` across all 13 resource types (`user`, `organization`, `client`, `group`, `container`, `data_bag`, `node`, `role`, `environment`, `cookbook`, `policy`, `policy_group`, `cookbook_artifact`).
- [ ] Verify against `user_spec.rb`, `organization_spec.rb`, `account/*`, `clients/*`, `keys/*`, `principal_spec.rb`, `groups_acl_spec.rb`, `auth_spec.rb`, `versioned_behaviors/server_api_v1_spec.rb`.

---

## Phase 3: Core Converge Objects, Bookshelf, Cookbooks, Policyfiles & Depsolver

### 3.1 Nodes, Roles, Environments & Data Bags
- [ ] **Nodes (`/organizations/:org/nodes`):**
  - [ ] `GET,POST /organizations/:org/nodes`, `GET,PUT,DELETE /organizations/:org/nodes/:name`, `GET /organizations/:org/environments/:env/nodes`.
  - [ ] Validate node JSON schema (`name` match, `json_class: "Chef::Node"`, `chef_type: "node"`, `chef_environment`, `run_list` normalization, `default`/`normal`/`override`/`automatic` attribute maps, `policy_name`, `policy_group`).
  - [ ] Trigger search index add/update/delete on mutation.
- [ ] **Roles (`/organizations/:org/roles`):**
  - [ ] `GET,POST /organizations/:org/roles`, `GET,PUT,DELETE /organizations/:org/roles/:name`.
  - [x] `GET /organizations/:org/roles/:name/environments` and `GET /organizations/:org/roles/:name/environments/:env` (and `/environments/:env/roles/:name`).
  - [ ] Validate role JSON schema (`json_class: "Chef::Role"`, `run_list`, `env_run_lists`, `default_attributes`, `override_attributes`).
- [ ] **Environments (`/organizations/:org/environments`):**
  - [ ] `GET,POST /organizations/:org/environments`, `GET,PUT,DELETE /organizations/:org/environments/:env`.
  - [ ] Protect `_default` environment from deletion or modification (`405 Method Not Allowed`).
  - [ ] Validate environment JSON (`cookbook_versions` constraint map, `default_attributes`, `override_attributes`).
  - [x] Implement `GET /organizations/:org/environments/:env/cookbooks`, `.../cookbooks/:cookbook`, and `.../recipes` (filtering cookbooks by environment version constraints).
- [ ] **Data Bags (`/organizations/:org/data`):**
  - [ ] `GET,POST /organizations/:org/data`, `GET,POST,DELETE /organizations/:org/data/:bag`.
  - [ ] `GET,PUT,DELETE /organizations/:org/data/:bag/:item` (handle `raw_data` wrapper vs unwrapped item JSON, validate `id` field match, index items under `{data_bag, bag_name}`).
- [ ] Verify against `nodes/*`, `roles/*`, `environments/*`, `data_bag/*` pedant specs.

### 3.2 Object Store, Bookshelf & Sandboxes (`microwave-objectstore`)
- [ ] Implement `ObjectStore` trait with two backends:
  - [ ] **Local Filesystem Backend:** atomic temp-file write + on-the-fly MD5 verification + rename into `{data_dir}/bookshelf/organization-{org_id}/checksum-{md5}`.
  - [ ] **S3 / Garage Backend:** SigV4 presigned URL generation (`PUT` with `content-md5` and `content-type: application/x-binary`; `GET`), `HEAD` object existence checks, and batch `DELETE`.
- [ ] Implement embedded `/bookshelf/:bucket/organization-:org/checksum-:md5` HTTP routes (`HEAD`, `GET`, `PUT`, `DELETE`) with SigV4 query-string and header authentication, `ETag` (Base64 MD5), and streaming body I/O.
- [ ] Implement `POST /organizations/:org/sandboxes`:
  - [ ] Validate `{"checksums": {"<md5>": null, ...}}` payload.
  - [ ] Check existing `checksums` table for the org; insert needed checksums into `sandboxed_checksums` and return `201 Created` with `sandbox_id`, `uri`, and per-checksum `{needs_upload, url}`.
- [ ] Implement `PUT /organizations/:org/sandboxes/:id`:
  - [ ] Validate `{"is_completed": true}`.
  - [ ] Verify all `needs_upload` checksums are present in `microwave-objectstore` (return `503 Service Unavailable` listing missing checksums if any are absent).
  - [ ] Promote checksums to `checksums` table and delete sandbox record.
- [ ] Verify against `oc-chef-pedant/spec/api/sandboxes/complete_endpoint_spec.rb`.

### 3.3 Cookbooks, Cookbook Artifacts, Policyfiles & Universe
- [ ] **Cookbooks (`/organizations/:org/cookbooks`):**
  - [ ] `GET /organizations/:org/cookbooks` (`?num_versions=N|all`), `GET .../cookbooks/_latest`, `GET .../cookbooks/_recipes`.
  - [ ] `GET /organizations/:org/cookbooks/:name` (`?num_versions=N|all`).
  - [ ] `GET,PUT,DELETE /organizations/:org/cookbooks/:name/:version`:
    - [ ] Validate cookbook version manifest (segments: `recipes`, `definitions`, `libraries`, `attributes`, `files`, `templates`, `resources`, `providers`, `root_files`, `metadata`).
    - [ ] Verify all file checksums exist in `checksums` for the org (`400 Bad Request` if any missing).
    - [ ] Enforce `frozen?` immutability (`409 Conflict` on `PUT` unless `?force=true`).
    - [ ] Attach presigned `GET` URLs to all manifest file entries on `GET`.
    - [ ] On `DELETE`, garbage-collect checksums no longer referenced by any `cookbook_versions` or `cookbook_artifact_versions` in the org.
- [ ] **Cookbook Artifacts (`/organizations/:org/cookbook_artifacts`):**
  - [ ] `GET /organizations/:org/cookbook_artifacts`, `GET /organizations/:org/cookbook_artifacts/:name`.
  - [ ] `GET,PUT,DELETE /organizations/:org/cookbook_artifacts/:name/:identifier` (validate 40-char hex SHA1 identifier, checksum references, presigned URLs, and orphan checksum cleanup).
- [ ] **Policies & Policy Groups (`/organizations/:org/policies`, `/organizations/:org/policy_groups`):**
  - [ ] `GET /organizations/:org/policies`, `GET,POST,DELETE /organizations/:org/policies/:name`.
  - [ ] `GET /organizations/:org/policies/:name/revisions`, `GET,DELETE /organizations/:org/policies/:name/revisions/:revision_id`.
  - [ ] `GET /organizations/:org/policy_groups`, `GET,DELETE /organizations/:org/policy_groups/:group`.
  - [ ] `GET,PUT,DELETE /organizations/:org/policy_groups/:group/policies/:name` (create policy/group/revision associations dynamically on `PUT` if not yet present, matching `oc_chef_wm_policy_group_policy_rev.erl`).
- [ ] **Universe (`GET /organizations/:org/universe`):**
  - [ ] Return Berkshelf-compatible dependency universe JSON across all cookbooks and versions in the org.
- [ ] Verify against `cookbooks/*`, `cookbook_artifacts/*`, `policies/*`, and `universe_spec.rb`.

### 3.4 Dependency Solver (`microwave-depsolver`)
- [ ] Implement upstream cookbook version parser (`MAJOR.MINOR` and `MAJOR.MINOR.PATCH`) and constraint operators (`=`, `>`, `<`, `>=`, `<=`, `~>`).
- [ ] Implement backtracking dependency graph solver with timeout (2,000ms) and detailed failure diagnostics (`non_existent_cookbooks`, `most_constrained_cookbooks`, `unable_to_solve`).
- [ ] Implement `POST /organizations/:org/environments/:env/cookbook_versions` handler:
  - [ ] Parse `{"run_list": [...]}` recipes/cookbook constraints.
  - [ ] Combine with `:env` cookbook constraints and solve against all cookbook versions in the org.
  - [ ] Return solved cookbook version objects with presigned URLs (filtered to minimal metadata fields required by `chef-client`), or `412 Precondition Failed` with exact error JSON on conflict/missing cookbook.
- [ ] Port all `depsolver_tests.erl` unit tests and verify against `oc-chef-pedant/spec/api/depsolver_spec.rb`.

---

## Phase 4: Search & Indexing Subsystem (`microwave-search`)

### 4.1 Document Expansion (port of upstream `chef_index_expand`)
- [ ] Implement upstream attribute deep-merge (`chef_deep_merge.erl` parity) for node attribute precedence (`default` < `normal` < `override` < `automatic`).
- [ ] Implement JSON flattening/expansion into `content` tokens (`parent_child__=__value` + leaf `child__=__value`) with exact character escaping and word-break behavior.
- [ ] Implement special field expansion for nodes (`recipe`, `recipes`, `role`, `roles`, `run_list`, `chef_environment`, `policy_name`, `policy_group`), roles, environments, clients, and data bag items.
- [ ] Add metadata fields `X_CHEF_id_CHEF_X`, `X_CHEF_database_CHEF_X` (`chef_{org_id}`), `X_CHEF_type_CHEF_X`, and `data_bag`.
- [ ] Port all EUnit tests from `src/oc_erchef/apps/chef_index/test/chef_index_expand_tests.erl`.

### 4.2 Lucene Query Parser & Rewriter (port of upstream `chef_lucene.peg`)
- [ ] Implement PEG grammar matching `chef_lucene.peg`:
  - [ ] `*:*`, field queries `field:term`, phrase queries `field:"foo bar"`, range queries `field:[a TO b]` / `field:{a TO b}` (including `*` bounds rewritten to `\ufff0`), wildcards `*` and `?`, fuzzy `~`, boost `^`, boolean `AND`/`&&`/`OR`/`||`/`NOT`/`!`/`+`/`-`, nested parentheses.
- [ ] Implement URI percent-decoding and query parameter validation (`start` >= 0, `rows` >= 0, empty `q=""` -> `400 Bad Request`).
- [ ] Port all EUnit tests from `src/oc_erchef/apps/chef_index/test/chef_lucene_tests.erl` and `chef_index_query_tests.erl`.

### 4.3 Search Backends & Endpoints
- [ ] Implement **Tantivy Embedded Backend**:
  - [ ] Custom tokenizer matching the upstream whitespace/symbol tokenization on `content`.
  - [ ] Query AST to Tantivy query compiler (Term, Phrase, Wildcard/Regex, Range, Boolean, Boost).
  - [ ] Deterministic sorting by `X_CHEF_id_CHEF_X asc` and pagination (`start`, `rows`).
- [ ] Implement **OpenSearch / Elasticsearch Backend**:
  - [ ] `/_bulk` indexing/deletion and `/_search` query execution matching `chef_opensearch.erl`.
- [ ] Implement `inline` and `batch` indexing queue modes (`search_queue_mode`).
- [ ] Implement `GET /organizations/:org/search` (list built-in indexes `client`, `environment`, `node`, `role` + all `data_bag` names in org).
- [ ] Implement `GET /organizations/:org/search/:index` and `POST /organizations/:org/search/:index` (partial search extracting attribute path arrays from hydrated objects; return `404` if searching non-existent data bag).
- [ ] Implement `strict_search_result_acls` post-filtering.
- [ ] Verify against `oc-chef-pedant/spec/api/search/search_spec.rb`, `word_break_spec.rb`, and `reindex_spec.rb`.

---

## Phase 5: Surrounding Compatibility Surfaces (`/id`, `chef-server-ctl`, Data Collector, Migration)

### 5.1 `/id` OAuth2 & Identity Service (`microwave-oauth`)
- [ ] Implement session management (signed/encrypted cookies compatible with `/id/signin` and `/id/signout`).
- [ ] Implement OAuth2 Authorization Server endpoints:
  - [ ] `GET,POST,DELETE /id/oauth/authorize`
  - [ ] `POST /id/oauth/token` (`authorization_code` and `refresh_token` grants)
  - [ ] `POST /id/oauth/revoke`
  - [ ] `GET,POST /id/oauth/applications` and `GET,PUT,DELETE /id/oauth/applications/:id`
- [ ] Implement User Profile & Password Reset endpoints:
  - [ ] `GET,PUT /id/profile`, `PUT /id/profile/password`, `GET /id/profile/email`, `POST /id/profile/regen_key`
  - [ ] `GET,POST,PUT /id/password-reset`
- [ ] Implement `/id/v1/*` JSON endpoints:
  - [ ] `GET /id/v1/me`, `GET /id/v1/me/organizations`, `GET /id/v1/users/:id`, `GET /id/v1/status`
- [ ] Create clean server-rendered HTML templates for sign-in, OAuth consent, profile, and application management.
- [ ] Verify against `oc-chef-pedant/spec/api/oc_id_spec.rb`.

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
> either backend. `reconfigure` validates, initializes/migrates, and reports
> without restarting a server. `reindex` prints relational counts for the
> embedded backend and backfills remote OpenSearch/Elasticsearch documents in
> chunked `_bulk` requests, counting only acknowledged items and refreshing the
> index when done. `reindex --purge` first deletes each visited organization's
> remote documents; without it the reindex stays additive.
- [ ] Full upstream command/flag parity. The clap surface is implemented; the remaining gaps are `user-edit`, `cleanup`, `cleanup-bifrost`, `filtered-dump`, `rotate-credentials`, `rotate-all-credentials`, `rotate-shared-secrets`, `oc-id-show-app`, and `gather-logs`.
  - [x] Install `chef-server-ctl` as a compatibility alias to `microwave-ctl`.
  - [x] Org & User commands: `user-list`, `user-show`, `user-create`, `user-delete`, `org-list`, `org-show`, `org-create`, `org-delete`, `org-user-add`, `org-user-remove`, `password`.
  - [x] Server Admin commands: `grant-server-admin-permissions`, `remove-server-admin-permissions`, `list-server-admins`.
  - [x] Key Rotation commands: `add-client-key`, `add-user-key`, `list-client-keys`, `list-user-keys`, `delete-client-key`, `delete-user-key`.
  - [x] Search & Maintenance commands: `reindex` (embedded count verification and remote backfill) and `maintenance`.
  - [x] Secrets & Config commands: `show-secret`, `set-secret`, `remove-secret`, `check-config`, and `reconfigure`.
  - [x] Lifecycle & Diagnostics: `status`, `ha-status`, `version`, `psql`, `backup`, `restore`, `pedant` (the `test` equivalent), and `service start|stop|restart|status|tail` (systemd, plus `start --foreground` for containers). Directory and tar backup formats are supported for SQLite and PostgreSQL.

### 5.4 Legacy Data Migration Tool (`microwave-migrate`)
> **Backend status:** export and import support both SQLite and PostgreSQL
> through the shared versioned JSON snapshot. `import-knife-backup` also
> supports both backends, writes cookbook blobs to a local object-store root,
> and requires `--bookshelf-dir` with `--database-url`. The snapshot
> intentionally omits `kvpair` secrets; use `microwave-ctl backup` for a full
> database copy.
- [x] Implement **Mode 1: Archive / Backup Importer (`knife-ec-backup` directory):**
  - [x] `microwave-migrate import-knife-backup` parses a ChefFS backup directory offline, supports `--dry-run`, and requires confirmation/`--yes`.
  - [x] Import users (profiles, admin flag, legacy password hashes/salts from `key_dump.json`), organizations, associations, and invites.
  - [x] Import clients and multi-key records from `key_table_dump.json`/inline public keys (`actor_keys`), preserving key names, versions, and expiry.
  - [x] Import containers, groups, group nesting, and ACL graphs, including user ACLs, granular `users`/`clients`, and the standard global `server-admins`/read-access graph.
  - [x] Import nodes, roles, environments, data bags/items, and ChefFS cookbooks (metadata, frozen status, MD5 checksums, and local bookshelf blobs). Environment constraints are preserved in the environment bodies.
  - [ ] Import cookbook artifacts, policies/policy groups, and `oc-id` OAuth applications. These are not present in a knife backup; snapshot import already handles them when a snapshot contains them.
  - [ ] Evaluate arbitrary Ruby in `metadata.rb` and import blobs into S3/Garage. The current parser is a tested DSL subset and blobs target the local object store.
- [x] Implement **Mode 2: Live API Crawler:**
  - [x] Crawl a running upstream server using `pivotal` credentials to pull orgs, users, ACLs, cookbooks/metadata, policies, nodes, roles, environments, and data bags into a snapshot.
  - [ ] Copy cookbook blobs during the live crawl; the crawler currently transfers metadata and checksum references only.
- [ ] Trigger full search reindex and verify object counts and checksum integrity post-migration.

### 5.5 Full Pedant Verification, HA & Performance Benchmarks
- [ ] Run complete `oc-chef-pedant` suite against Default Embedded mode (SQLite + Tantivy + Local Bookshelf) and achieve 100% pass rate on supported specs.
- [ ] Run complete `oc-chef-pedant` suite against HA mode (PostgreSQL + OpenSearch + Garage/S3) and achieve 100% pass rate.
- [ ] Run `chef-client` (`19.4.47`) and `knife` end-to-end integration tests.
- [ ] Run 1,000-node concurrent converge load simulation against Default Embedded mode to verify latency, memory footprint, and zero `SQLITE_BUSY` errors.
