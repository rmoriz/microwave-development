# Microwave Server

A Rust reimplementation of the Chef Infra Server API, designed as a drop-in replacement for existing clients and operations tooling. This is an independent project, not affiliated with or endorsed by Progress Software Corporation.

## Status

The repository contains a working vertical slice:

- **Configuration** — TOML plus `MW_*` environment overrides, including an optional TLS listener.
- **Authentication** — `X-Ops-Sign` 1.0/1.1/1.3 canonicalization, RSA signing/verification, key reassembly, and legacy password hash compatibility.
- **Key generation** — API-created keys use a bounded, thread-safe cache of pre-generated 2048-bit RSA keypairs. Background workers refill it, `keygen.size` / `keygen.workers` / `keygen.warm_up` (and `MW_KEYGEN__*`) control it, and `/_status` reports cached count, capacity, and worker health.
- **Storage** — one metadata contract with two backends: SQLite (default) and PostgreSQL, selected by `database.driver` / `MW_DATABASE__DRIVER`. The HTTP API, OAuth/identity surface, embedded search, authorization policy, bootstrap, operational CLI, and snapshot migration run on either backend. SQLite remains the default and the only backend with an in-process database file. `serialized_object` documents are stored transparently compressed as `mwz1:` + base64(zlib(JSON)); legacy plain JSON remains readable, and `storage.compression = "none"` restores plain writes without breaking existing rows.
- **Authorization** — recursive ACL/group checks, cycle rejection, the default organization policy, and ACL read/write endpoints with granular `users`/`clients` responses.
- **HTTP API** — `/_status`, `/_stats`, `/server_api_version`, `/license`, users, organizations, clients, principals, key management, organization memberships/invitations, nodes, environments, roles, data bags, sandboxes, bookshelf uploads, cookbooks, cookbook resolution, policyfiles/policy groups, ACL endpoints, search (node/role/environment/client/data-bag indexes with a query subset and partial search), data-collector forwarding and opt-in mutation action notifications, the compliance profile proxy, and the optional server-enforced `required_recipe` endpoint.
- **Operations CLI** — `microwave-ctl` with a `chef-server-ctl` compatibility binary: user/org administration, server-admin grant/remove/list, org membership, password reset, key rotation (add/list/delete user and client keys), secrets management, maintenance state, `check-config`, `reconfigure`, `ha-status`, `psql`, status, directory or tar backup/restore, `reindex` (embedded count verification or a remote OpenSearch/Elasticsearch backfill), the `oc-chef-pedant` runner, systemd service management with a container-friendly `service start --foreground` mode, and `migrate-from-legacy` snapshot import. The same data commands work against either configured metadata backend.
- **Migration** — `microwave-migrate` exports a versioned JSON snapshot from SQLite or PostgreSQL (users with password hashes, organizations, clients, keys, authz ACLs, nodes, roles, environments, data bags, checksums, cookbooks, policies, and OAuth applications) and imports it into either backend with original ids, authz ids, timestamps, and keys intact. Snapshots always contain decoded plain JSON even when the database stores compressed values. PostgreSQL imports also advance the sequence-backed identifiers. The live legacy API crawler (`from-api`) is implemented.
- **OAuth/identity** — `/id` sign-in/sessions, OAuth application management, authorization-code and refresh-token flows, `/id/v1/me`, `/id/v1/me/organizations`, `/id/v1/status`, profile/password/key regeneration, and password-reset tokens.
- **Dependency solving** — Chef version constraints (`=`, `>`, `<`, `>=`, `<=`, `~>`), deterministic backtracking, environment constraints, role expansion, timeouts, and upstream-shaped 412 failures.
- **External backends** — S3/Garage object storage with SigV4 signing and presigned URLs, and OpenSearch/Elasticsearch search with bulk indexing and query translation. The local object store and embedded search remain the defaults.
- **Server** — `microwave-server serve` selects the configured metadata backend, bootstraps a `pivotal` superuser and listens on the configured address. It serves plain HTTP by default and HTTPS directly when `server.tls.cert_path` and `server.tls.key_path` are configured. `serve --check` initializes/migrates the backend and prints its type without exposing credentials.

Testing and formatting are green, and a strict stock `chef-client` converge passes without compatibility shims (`MW_E2E_COMPAT=0 MW_E2E_SYNTH_V2=0`):

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
scripts/run-chef-client-e2e.sh
scripts/run-postgres-tests.sh
```

## Run Locally

```sh
cargo run -p microwave-server -- serve --check
cargo run -p microwave-server -- serve
```

The first run creates `./data/pivotal.pem` (superuser private key), `./data/pivotal.password`, and `./data/objectstore.secret`. Use `MW_PATHS__DATA_DIR` to choose another data directory.

### TLS

Plain HTTP remains the default. To terminate TLS in the server itself, point it at a
PEM certificate chain and matching PEM private key:

```sh
MW_SERVER__TLS__CERT_PATH=/etc/microwave/tls.crt \
MW_SERVER__TLS__KEY_PATH=/etc/microwave/tls.key \
cargo run -p microwave-server -- serve
```

`microwave-server serve --check` and `microwave-ctl check-config` verify that both
files exist, are readable, contain a certificate and private key, and form a
matching pair. They print only the paths, never key material. After rotation,
send `SIGHUP` on Unix (`kill -HUP`, `systemctl reload microwave-server`, or
`docker kill --signal=HUP`) to swap in the new pair without dropping the
listener; a failed reload keeps the previous certificate. Set
`server.tls.reload_interval_seconds` to poll the files instead. A load balancer
or reverse proxy terminating TLS in front of plain HTTP remains fully supported;
see [`docs/deployment.md`](docs/deployment.md#tls-listener).

The full workspace suite is green.

### Dual-backend store matrix

[`crates/microwave-store/tests/dual_backend.rs`](crates/microwave-store/tests/dual_backend.rs)
contains one async suite for every repository domain and runs it against both
backends through `MetadataStore`:

- identity: orgs, users (profiles/patches), clients, actor keys, principals
- authorization: groups/containers, nested and global groups, ACL replacement,
  recursive permission checks, org read-access ACEs
- objects: nodes, roles, environments, data bags and bag items
- cookbooks: sandboxes, checksum commit/purge, version create/find/update/delete
- policies: revisions, policy groups, associations
- artifacts: artifact/version create/find/delete and checksum purge
- membership: add/remove, invitations accept/reject/delete, `users`/`admins`
- OAuth: applications, grants, access/refresh tokens, ID sessions, password resets
- KV/secrets: raw KV, maintenance flag, secret bootstrap

SQLite always runs in-memory. The PostgreSQL half runs when
`MICROWAVE_TEST_POSTGRES_URL` is set and is skipped only when the variable is
unset; a configured database that cannot connect or migrate fails the test.
`scripts/run-postgres-tests.sh` exports the URL and runs the matrix as part of
the PostgreSQL gate. Names and checksums are unique per run, assertions that
could see other rows (such as node counts) are scoped to the fixture
organization, and org-scoped fixtures plus test-created global groups are
deleted afterwards. The shared `server-admins` group and the global maintenance
KV flag are intentionally left in place. The maintenance test is serialized
within the test binary and restores the previous flag even when an assertion
panics; separate processes sharing one database must not run their maintenance
assertions concurrently.

### HTTP route parity

[`crates/microwave-api/tests/http_parity_test.rs`](crates/microwave-api/tests/http_parity_test.rs)
runs the same signed HTTP scenarios through SQLite and PostgreSQL and compares
status, `Location`/content-type headers, and normalized bodies for users
(including deletion), organizations, clients, keys, nodes, roles, environments,
data bags, groups, containers, sandboxes, cookbooks, cookbook artifacts,
policies/policy groups, search, user/org/object/data-bag/artifact ACLs,
membership/invitations, OAuth application/token and v1 identity/user routes,
credential authentication, system recovery, organization deletion (including
dependent checksum, client-key, sandbox, ACL and global read-access-group rows,
with atomic rollback and `server-admins` preservation), license, status/stats,
and depsolver. The PostgreSQL half skips only when
`MICROWAVE_TEST_POSTGRES_URL` is unset; set `MICROWAVE_REQUIRE_POSTGRES=1` or
`true` to make an unset URL, missing Docker, or missing client tools a hard
failure. Cleanup asserts every deletion, checks captured client keys and
sandbox rows, verifies no run-named organizations, users, OAuth applications,
or global groups survive, and keeps a panic-safe PostgreSQL drop guard.
Remaining parity gaps are OAuth HTML session/profile pages and remote
OpenSearch/Elasticsearch search, which rely on their dedicated suites.

## Deployment

Docker, Docker Compose, systemd, backup/restore, migration, and the supported
HA topology are documented in [`docs/deployment.md`](docs/deployment.md).
Packaging artifacts: `Dockerfile`, `.dockerignore`, `docker-compose.yml`,
`docker-compose.ha.yml`, and `packaging/`.

The image default command is the supported container entrypoint:

```sh
docker run ... microwave-server:local \
  microwave-ctl service start --foreground --config /etc/microwave/microwave.toml
```

`microwave-ctl` validates the config path and `exec`s `microwave-server serve`,
so the server is PID 1 and Docker signals reach it directly. `SIGTERM` and
`SIGINT` trigger graceful shutdown; `SIGHUP` reloads TLS. The environment
variable `MW_SERVICE_FOREGROUND=1` is equivalent to `--foreground` for
`service start`. The direct command
`microwave-server serve --config /etc/microwave/microwave.toml` remains
supported as a `command:`/`docker run` override.

## Operational CLI Notes

`microwave-ctl` and the `chef-server-ctl` alias share one dispatch, so every command below is available through both names.

- **Server admins.** `grant-server-admin-permissions`, `remove-server-admin-permissions`, and `list-server-admins` use the `users.admin` column as the source of truth. The unified schema has no separate global `server-admins` group, and the HTTP/OAuth surfaces already treat `users.admin` as the administrator flag. The configured superuser cannot be demoted.
- **Key rotation.** `add-client-key`, `add-user-key`, `list-client-keys`, `list-user-keys`, `delete-client-key`, and `delete-user-key` manage `actor_keys`. New keys are 2048-bit RSA keypairs; the private key is printed exactly once at creation and is not stored. When `--key-name` is omitted, the key name is the Chef-compatible SHA-1 public-key fingerprint. `--public-key-path` imports an existing PEM public key instead of generating one, and `--expiration-date` accepts `infinity` or an ISO-8601 timestamp.
- **Secrets.** `set-secret`, `show-secret`, and `remove-secret` store values in the `kvpair` table under `secret:<group>:<name>`. Only `show-secret` prints a value. The supported credential names mirror upstream, including `object_store secret`, `bookshelf access_key_id`, `bookshelf secret_access_key`, `data_collector token`, and the SQL password groups. At startup the server resolves the local `/bookshelf` HMAC secret from configuration, then `object_store secret`, then `<data_dir>/objectstore.secret`, and copies file-backed values into the secret store. S3/Garage access and secret keys fall back to the `bookshelf` secrets when configuration omits them.
- **Maintenance.** `maintenance on`, `maintenance off`, and `maintenance status` persist a flag in `kvpair` (`maintenance:enabled`). While the flag is on, the HTTP middleware returns `503 Service Unavailable` with a JSON error for normal API requests; `/_status` and `/_stats` remain reachable so health checks and monitoring keep working.
- **Diagnostics.** `check-config` loads the resolved configuration and validates the data directory, database URL/driver, bind address, search backend, object-store credentials, TLS certificate/key pair, and superuser name. `reconfigure` runs the same validation, opens the configured metadata backend so the schema is initialized or migrated, and reports the resolved database, search, and object-store backends plus user/org counts. It never restarts a running server and never deletes data; restart the server yourself to apply configuration changes. `ha-status` reports a single-node deployment; multi-node, DRBD, and keepalived HA are not implemented. `psql` execs `sqlite3` against the configured database, or `psql` with a configured PostgreSQL URL, and passes trailing arguments through.
- **Search reindex.** `reindex [--org ORG] [--purge]` rebuilds remote search documents from the relational store. With the embedded backend it reports that relational search is authoritative and prints the acknowledged document counts. With `opensearch`/`elasticsearch` it iterates organizations and indexes nodes, roles, environments, clients, and every data bag item through the same document builder the API uses, then reports per-type counts of documents the cluster acknowledged. Documents are written through `index_many` in `_bulk` requests of up to 500 documents, and the command calls `POST /chef/_refresh` when it finishes so results are immediately visible. A bulk response is parsed item by item: a 200 response containing `errors:true` or any failed item/status aborts the command, and rejected documents are never counted. By default it only adds or replaces documents by id and leaves documents for objects deleted while the remote backend was inactive in place. Pass `--purge` to delete each visited organization's existing remote documents with delete-by-query before rebuilding. A missing `chef` index is treated as nothing to purge, and a delete response that times out, reports failures or version conflicts, or deletes fewer documents than matched aborts the command instead of claiming a complete purge. Purging is destructive, so an interrupted run can leave an organization unsearchable until it is re-run. Both paths converge on the same document ids; running a reindex while the server is serving is supported, though concurrent writes may also be indexed by the server.
- **Pedant runner.** `pedant [TAGS...]` (also available as the upstream `test` alias) locates `scripts/run-pedant.sh` in the current tree or next to the executable, searching only up to the repository root (`.git` or the workspace `Cargo.toml`; override with `MW_PEDANT_SCRIPT`). It runs the harness with the tags as separate arguments, streams output, and exits with the harness status. No shell string is constructed, so tag values cannot inject commands.
- **Service management.** `service start|stop|restart|status|tail` delegates to `systemctl` (and `journalctl` for `tail`) for the `microwave-server` unit, overridable with `--unit`, `MW_SERVICE_UNIT`, `MW_SYSTEMCTL`, and `MW_JOURNALCTL`. Unit names are validated (no leading `-`, whitespace/control characters, or `/`) before being passed as an argument, and the systemd exit status is preserved. `tail` needs only `journalctl`. For containers and other environments without systemd, `service start --foreground` (or `MW_SERVICE_FOREGROUND=1`) resolves `microwave-server` from `MW_SERVER_BIN`, the current executable's directory, or `PATH`, validates the `--config` path, and runs `microwave-server serve --config PATH`. On Unix the CLI `exec`s the server, so the server keeps the PID and receives `SIGTERM`/`SIGINT`/`SIGHUP` directly; `SIGTERM`/`SIGINT` drain and exit cleanly, while `SIGHUP` reloads TLS. `--foreground` is rejected for the other actions. Foreground mode does not reproduce the systemd unit's user, filesystem, capability, or syscall hardening; in a container that isolation must come from the image and runtime. When `systemctl` is unavailable and foreground mode was not requested, the command fails with an actionable error that suggests `service start --foreground`.
- **Backup formats.** `backup PATH` writes a directory containing the SQLite `VACUUM INTO` snapshot or PostgreSQL custom-format `pg_dump` plus the local `bookshelf/` copy. `backup PATH.tar --format tar` (or `--tar`) writes the same payload as one uncompressed tar archive. Restore validates and copies the backup bookshelf into staging before replacing the live database or bookshelf, so a symlink/FIFO in a directory backup fails without changing live data; tar restore validates its entry allowlist the same way. Restore remains confirmation-gated: it refuses to replace live data without an interactive `yes` or `--yes`. Neither format includes `pivotal.pem`, `pivotal.password`, `objectstore.secret`, S3/Garage objects, or the remote search index.
- **PostgreSQL operations.** `status`, user/org/key/secret/maintenance commands, `check-config`, `reconfigure`, `ha-status`, `psql`, `backup`, `restore`, `reindex`, and `migrate-from-legacy` all use the configured backend. PostgreSQL `backup` writes `microwave.dump` with `pg_dump --format=custom`; `restore` validates the archive with `pg_restore --list`, prints the target without credentials, compares the archive's source database, and then runs `pg_restore --clean --if-exists --single-transaction`. `restore` is destructive and requires confirmation or `--yes`. PostgreSQL passwords are removed from client-tool command lines; `password` is passed through `PGPASSWORD`, while `sslpassword` is placed in a temporary `0600` libpq service file referenced by `PGSERVICEFILE`/`PGSERVICE`. Environment variables and temporary files may still be observable to the same UID on some platforms. Install `pg_dump`/`pg_restore` on the host. `migrate-from-legacy` and `microwave-migrate import` also ask for confirmation unless `--yes` is given. Commands that need the store apply the embedded schema on connect; `status` and `check-config` use `connect_existing` so read-only probes do not create tables. SQLite backup/restore keeps the existing `VACUUM INTO` and staged-file path.

## Remaining Work

- Keep the full extended `oc-chef-pedant` suite green in CI. All twelve extended tags pass, including `compliance_proxy` (4 examples, 0 failures); the remaining 7 pending examples are Pedant's own skips. The core suite is also near-green, with only the known plain-HTTP header artifact.
- PostgreSQL operational tooling. The HTTP runtime, OAuth, authorization, search, bootstrap, `microwave-ctl`, `microwave-migrate`, and backup/restore run against PostgreSQL. PostgreSQL backups use `pg_dump`/`pg_restore` and require the matching client tools on the host. Remaining gaps are the snapshot's intentional omission of `kvpair` secrets, no multi-replica HA load validation, and no automated failover.
- Service-management and reindex gaps. Remote search reindexing purges stale documents only when `--purge` is passed; the default remains additive. Service management is systemd-based (`systemctl`/`journalctl`) or an explicit foreground/container mode (`service start --foreground`); there is no built-in supervisor for Docker daemons, launchd, or detached processes. The pedant runner invokes the repository harness and therefore expects the checkout or a separately installed script.
- OpenSearch 3.x evaluation and multi-node HA/performance validation.
- Outbound telemetry export. The `telemetry` table (SQLite and PostgreSQL) and `telemetry.*` / `MW_TELEMETRY__*` configuration exist, but the sender is intentionally not implemented and defaults to disabled: Microwave never phones home. Operators can enable the flag for forward compatibility, but no background worker or external endpoint is contacted.
- Optional data-collector mutation action notifications and the compliance profile proxy are implemented behind configuration (`data_collector.actions_enabled`, `compliance_proxy.url`). The pedant compliance-proxy tag is wired through the harness; there is no vendored data-collector pedant spec.

### Pedant Compatibility

The first `oc-chef-pedant` run is recorded in [`docs/pedant-report.md`](docs/pedant-report.md). Baseline for the five tags run:

| Tag | Passed | Failed | Pending |
| --- | ---: | ---: | ---: |
| headers | 6 | 1 | 0 |
| users | 165 | 0 | 0 |
| organizations | 15 | 0 | 0 |
| clients | 139 | 0 | 3 |
| nodes | 326 | 0 | 5 |
| **Total** | **651** | **1** | **8** |

The harness is `scripts/run-pedant.sh`. Users, organizations, clients, and nodes now pass with zero failures in this tag set. The only failure is the known plain-HTTP `headers` artifact; the 8 pending examples are pedant's own skips.

Extended suites (final, including the PostgreSQL runtime, operations, TLS, keygen cache, compression, CLI surface, TLS reload, and dual-backend matrix): **3,280 passed / 0 failed / 7 pending**. All twelve extended tags are green; the historical 3,280 count covers the original eleven, and `compliance_proxy` adds 4 passing examples.

See `docs/superpowers/specs/2026-10-24-microwave-server-design.md` and `TODO.md` for the full target and remaining work.

## Layout

| Path | Contents |
|---|---|
| `chef-server/` | Vendored upstream server `15.10.127` source snapshot (commit `c38d169f083d064b8c542d912d072d8b88cf4c8c`). |
| `chef/` | Vendored upstream client `19.4.47` source snapshot (commit `46354347bc125edc98c5f080ca2592d55ec0e4bc`). |
| `crates/` | Rust workspace crates. |
| `bin/` | Server and compatibility binaries. |
| `docs/superpowers/specs/` | Architecture and design specifications. |
| `packaging/` | systemd unit, example configuration, Garage config/bootstrap for Compose. |
| `TODO.md` | Master implementation checklist. |

## Naming and Trademarks

"Chef", "Chef Infra", "Chef Infra Server", and "Chef Infra Client" are trademarks of Progress Software Corporation. They appear in this repository only to identify the upstream software and the compatibility surface.

This project's own crates, binaries, modules, services, and public types use the `microwave-*` prefix or domain-neutral names. The `chef-server-ctl` command name is retained only as a compatibility alias for `microwave-ctl`. Protocol-required identifiers such as the `X-Chef-Version` header, the `chef_type` JSON field, `/etc/opscode/chef-server.rb`, and `CHEF_SECRETS_DATA` are preserved because changing them would break compatibility.

## Vendored Git Histories

The original upstream `.git` directories are preserved locally at:

- `.git/vendor/chef-server.git`
- `.git/vendor/chef.git`

They are inside the repository's `.git` directory and therefore not tracked or included in clones. To inspect upstream history after this import:

```sh
git --git-dir=.git/vendor/chef-server.git --work-tree=chef-server log --oneline -10
git --git-dir=.git/vendor/chef.git --work-tree=chef log --oneline -10
```
