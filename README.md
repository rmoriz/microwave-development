# Microwave Server

A Rust reimplementation of the Chef Infra Server API, designed as a drop-in replacement for existing clients and operations tooling. This is an independent project, not affiliated with or endorsed by Progress Software Corporation.

## Status

The repository contains a working vertical slice:

- **Configuration** — TOML plus `MW_*` environment overrides.
- **Authentication** — `X-Ops-Sign` 1.0/1.1/1.3 canonicalization, RSA signing/verification, key reassembly, and legacy password hash compatibility.
- **Storage** — one metadata contract with two backends: SQLite (default) and PostgreSQL, selected by `database.driver` / `MW_DATABASE__DRIVER`. The HTTP API, OAuth/identity surface, embedded search, authorization policy, bootstrap, operational CLI, and snapshot migration run on either backend. SQLite remains the default and the only backend with an in-process database file.
- **Authorization** — recursive ACL/group checks, cycle rejection, the default organization policy, and ACL read/write endpoints with granular `users`/`clients` responses.
- **HTTP API** — `/_status`, `/server_api_version`, `/license`, users, organizations, clients, principals, key management, organization memberships/invitations, nodes, environments, roles, data bags, sandboxes, bookshelf uploads, cookbooks, cookbook resolution, policyfiles/policy groups, ACL endpoints, and search (node/role/environment/client/data-bag indexes with a query subset and partial search).
- **Operations CLI** — `microwave-ctl` with a `chef-server-ctl` compatibility binary: user/org administration, server-admin grant/remove/list, org membership, password reset, key rotation (add/list/delete user and client keys), secrets management, maintenance state, `check-config`, `ha-status`, `psql`, status, local or PostgreSQL backup/restore, `migrate-from-legacy` snapshot import, and a clear not-implemented response for reindex. The same commands work against either configured metadata backend.
- **Migration** — `microwave-migrate` exports a versioned JSON snapshot from SQLite or PostgreSQL (users with password hashes, organizations, clients, keys, authz ACLs, nodes, roles, environments, data bags, checksums, cookbooks, policies, and OAuth applications) and imports it into either backend with original ids, authz ids, timestamps, and keys intact. PostgreSQL imports also advance the sequence-backed identifiers. The live legacy API crawler (`from-api`) is implemented.
- **OAuth/identity** — `/id` sign-in/sessions, OAuth application management, authorization-code and refresh-token flows, `/id/v1/me`, `/id/v1/me/organizations`, `/id/v1/status`, profile/password/key regeneration, and password-reset tokens.
- **Dependency solving** — Chef version constraints (`=`, `>`, `<`, `>=`, `<=`, `~>`), deterministic backtracking, environment constraints, role expansion, timeouts, and upstream-shaped 412 failures.
- **External backends** — S3/Garage object storage with SigV4 signing and presigned URLs, and OpenSearch/Elasticsearch search with bulk indexing and query translation. The local object store and embedded search remain the defaults.
- **Server** — `microwave-server serve` selects the configured metadata backend, bootstraps a `pivotal` superuser and listens on the configured address. `serve --check` initializes/migrates the backend and prints its type without exposing credentials.

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

The full workspace suite is green.

## Deployment

Docker, Docker Compose, systemd, backup/restore, migration, and the supported
HA topology are documented in [`docs/deployment.md`](docs/deployment.md).
Packaging artifacts: `Dockerfile`, `.dockerignore`, `docker-compose.yml`,
`docker-compose.ha.yml`, and `packaging/`.

## Operational CLI Notes

`microwave-ctl` and the `chef-server-ctl` alias share one dispatch, so every command below is available through both names.

- **Server admins.** `grant-server-admin-permissions`, `remove-server-admin-permissions`, and `list-server-admins` use the `users.admin` column as the source of truth. The unified schema has no separate global `server-admins` group, and the HTTP/OAuth surfaces already treat `users.admin` as the administrator flag. The configured superuser cannot be demoted.
- **Key rotation.** `add-client-key`, `add-user-key`, `list-client-keys`, `list-user-keys`, `delete-client-key`, and `delete-user-key` manage `actor_keys`. New keys are 2048-bit RSA keypairs; the private key is printed exactly once at creation and is not stored. When `--key-name` is omitted, the key name is the Chef-compatible SHA-1 public-key fingerprint. `--public-key-path` imports an existing PEM public key instead of generating one, and `--expiration-date` accepts `infinity` or an ISO-8601 timestamp.
- **Secrets.** `set-secret`, `show-secret`, and `remove-secret` store values in the `kvpair` table under `secret:<group>:<name>`. Only `show-secret` prints a value. The supported credential names mirror upstream, including `object_store secret`, `bookshelf access_key_id`, `bookshelf secret_access_key`, `data_collector token`, and the SQL password groups. At startup the server resolves the local `/bookshelf` HMAC secret from configuration, then `object_store secret`, then `<data_dir>/objectstore.secret`, and copies file-backed values into the secret store. S3/Garage access and secret keys fall back to the `bookshelf` secrets when configuration omits them.
- **Maintenance.** `maintenance on`, `maintenance off`, and `maintenance status` persist a flag in `kvpair` (`maintenance:enabled`). **HTTP enforcement is pending:** running servers do not return `503` while maintenance mode is on, and the command prints that limitation.
- **Diagnostics.** `check-config` loads the resolved configuration and validates the data directory, database URL/driver, bind address, search backend, object-store credentials, and superuser name. `ha-status` reports a single-node deployment; multi-node, DRBD, and keepalived HA are not implemented. `psql` execs `sqlite3` against the configured database, or `psql` with a configured PostgreSQL URL, and passes trailing arguments through.
- **PostgreSQL operations.** `status`, user/org/key/secret/maintenance commands, `check-config`, `ha-status`, `psql`, `backup`, `restore`, and `migrate-from-legacy` all use the configured backend. PostgreSQL `backup` writes `microwave.dump` with `pg_dump --format=custom`; `restore` validates the archive with `pg_restore --list`, prints the target without credentials, compares the archive's source database, and then runs `pg_restore --clean --if-exists --single-transaction`. `restore` is destructive and requires confirmation or `--yes`. PostgreSQL passwords are removed from client-tool command lines; `password` is passed through `PGPASSWORD`, while `sslpassword` is placed in a temporary `0600` libpq service file referenced by `PGSERVICEFILE`/`PGSERVICE`. Environment variables and temporary files may still be observable to the same UID on some platforms. Install `pg_dump`/`pg_restore` on the host. `migrate-from-legacy` and `microwave-migrate import` also ask for confirmation unless `--yes` is given. Commands that need the store apply the embedded schema on connect; `status` and `check-config` use `connect_existing` so read-only probes do not create tables. SQLite backup/restore keeps the existing `VACUUM INTO` and staged-file path.

## Remaining Work

- Keep the full extended `oc-chef-pedant` suite green in CI. All eleven extended tags pass; the remaining 7 pending examples are Pedant's own skips. The core suite is also near-green, with only the known plain-HTTP header artifact.
- PostgreSQL operational tooling. The HTTP runtime, OAuth, authorization, search, bootstrap, `microwave-ctl`, `microwave-migrate`, and backup/restore run against PostgreSQL. PostgreSQL backups use `pg_dump`/`pg_restore` and require the matching client tools on the host. Remaining gaps are the snapshot's intentional omission of `kvpair` secrets, no multi-replica HA load validation, and no automated failover.
- HTTP enforcement of maintenance mode. `microwave-ctl maintenance on` persists the flag, but the request middleware does not return `503` yet; the command says so explicitly.
- The remaining `chef-server-ctl` surface (`reconfigure`, real `reindex`, pedant runner, service management, full tar backup format).
- TLS listener, `_stats`/metrics, key-generation cache, transparent `serialized_object` compression, and the dual-backend test matrix.
- OpenSearch 3.x evaluation and multi-node HA/performance validation.
- Data Collector forwarding, `required_recipe`, and telemetry export.

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

Extended suites (final, including the PostgreSQL runtime): **3,280 passed / 0 failed / 7 pending**. All eleven extended tags are green.

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
