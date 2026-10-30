# Microwave Server

A glittering rainbow microwave under neon light

A Rust reimplementation of the Chef Infra Server API, designed as a drop-in replacement for existing `chef-client` fleets and operations tooling.

- [Why](#why)
- [Try it in five minutes](#try-it-in-five-minutes)
- [What works](#what-works)
- [Known gaps](#known-gaps)
- [Migrate from Chef Infra Server](#migrate-from-chef-infra-server)

![A glittering rainbow microwave under neon light](docs/images/microwave-hero.png)

## Why

- **Drop-in** — it implements the Chef Infra Server API, request signing, and authorization that
  existing `chef-client` runs and operations tooling already speak.
- **Proven with a real client** — a stock `chef-client` converges against it end to end: the
  harness starts a throwaway server, uploads a cookbook, runs the client, and checks the marker
  file the recipe wrote.
- **Pedant-checked** — the full `scripts/run-pedant.sh strict` set passes: 652 passed / 0 failed /
  8 pending out of 660 core examples in [`docs/pedant-report.md`](docs/pedant-report.md) and
  3,284 passed / 0 failed / 7 pending out of 3,291 extended examples in
  [`docs/pedant-extended-report.md`](docs/pedant-extended-report.md). The pending examples are
  pedant's own skips.
- **Independent** — not affiliated with or endorsed by Progress, so compatibility follows the API
  rather than a vendor roadmap.

## Try it in five minutes

You need Rust, curl, a `chef-client` 18+ binary (or `MW_E2E_CHEF_CLIENT` pointing at one), and Ruby with `mixlib-authentication` (or `MW_E2E_RUBY`). A missing piece exits `2` and names itself.

```sh
scripts/run-chef-client-e2e.sh
```

The harness builds a throwaway server on a free loopback port with a temporary data directory, so
it leaves anything you already run untouched. Success is exit `0` with `chef-client e2e: PASS`,
the converge recipe's `marker.txt` left in the artifact directory, and the saved node the harness
verifies. Set `MW_E2E_COMPAT=1` to grant the default environment's read ACE and synthesize the v2
`all_files` manifest for a server that returns neither.

To drive the server yourself instead:

```sh
cargo run -p microwave-server -- serve --check
cargo run -p microwave-server -- serve
```

The first run writes the superuser credentials to `./data/pivotal.pem`, `./data/pivotal.password`
and `./data/objectstore.secret`.

## What works

Everything below points at the file in this repository that backs it; nothing here is a roadmap
promise.

| Surface | Proof |
| --- | --- |
| Signed HTTP API — `/_status`, `/_stats`, `/server_api_version`, `/license`, users, organizations, clients, keys, nodes, roles, environments, data bags, sandboxes, cookbooks, policies, ACLs, `/validate/*`, search, data collector, compliance proxy | [`http_parity_test.rs`](crates/microwave-api/tests/http_parity_test.rs), [`data_collector_test.rs`](crates/microwave-api/tests/data_collector_test.rs), [`compliance_proxy_test.rs`](crates/microwave-api/tests/compliance_proxy_test.rs) |
| `X-Ops-Sign` signing — 1.0/1.1/1.3 canonicalization, RSA verification, key reassembly | [`docs/pedant-report.md`](docs/pedant-report.md) |
| Both metadata backends — SQLite and PostgreSQL behind one store contract | [`crates/microwave-store/tests/dual_backend.rs`](crates/microwave-store/tests/dual_backend.rs) |
| Operations CLI `microwave-ctl` — user/org administration, key rotation, backup/restore, reindex, diagnostics | [`TODO.md`](TODO.md) |
| Migration — snapshot import and `knife ec backup` import | [`docs/deployment.md`](docs/deployment.md#10-migration-from-a-legacy-server) |
| MCP endpoint — `experimental`, off by default, writes off with `writes = false` | [`docs/mcp.md`](docs/mcp.md) |

A stock `chef-client` converge against the result is the end-to-end check: see
[Try it in five minutes](#try-it-in-five-minutes).

## Known gaps

- No `chef-server.rb` importer. The legacy Ruby configuration is translated by hand; the mapping table is in [`docs/deployment.md`](docs/deployment.md#migrating-configuration-from-etcopscodechef-serverrb).
- Multi-node HA and automatic failover are not validated. `microwave-ctl ha-status` reports a single node, and `docker-compose.ha.yml` is an evaluation topology rather than a replica set.
- Embedded search is a relational scan, not an inverted index. It is correct for small and medium organizations and needs no extra service, but it scales linearly; move to OpenSearch or Elasticsearch before a large catalog makes queries hurt.

The open inventory below is generated from `docs/claims.toml` and includes the three above:

<!-- generated:remaining-work:start -->
Open gaps: 17 (migration: 6, mail: 1, search: 3, ha: 2, auth: 2, tooling: 3). By area: [migration](docs/remaining-work.md#area-migration), [mail](docs/remaining-work.md#area-mail), [search](docs/remaining-work.md#area-search), [ha](docs/remaining-work.md#area-ha), [auth](docs/remaining-work.md#area-auth), [tooling](docs/remaining-work.md#area-tooling).
<!-- generated:remaining-work:end -->

## Migrate from Chef Infra Server

The supported route is a snapshot copy, and it runs today. Export a versioned JSON snapshot with
`microwave-migrate from-api` against the old server, or with `microwave-migrate export` from a staging
installation. Pass `--bookshelf-dir /var/lib/microwave/bookshelf` to `from-api` so it also copies
cookbook file blobs into the local object store; without the flag the crawl is metadata-only. Then
import the snapshot into the configured SQLite or PostgreSQL database:

```sh
microwave-ctl --config /etc/microwave/microwave.toml \
  migrate-from-legacy --input /var/backups/acme-snapshot.json --yes
```

If the old server was backed up with `knife ec backup`, `import-knife-backup` reads that directory
directly, needs no running HTTP server, and writes cookbook blobs into the local object store. The
form below is the SQLite one: it defaults to `<data-dir>/bookshelf`. With `--database-url` the
importer requires an explicit `--bookshelf-dir`:

```sh
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --data-dir /var/lib/microwave \
  --yes
```

Neither path is lossless. A metadata-only `from-api` crawl does not copy cookbook blobs, and the
legacy API cannot return password hashes or the `admin` flag: crawled users start without a password,
with admin off, and need a password reset or an external identity provider. The knife importer carries
no cookbook artifacts, no policies or policy groups, and no OAuth applications; no migration path
recovers client or user private keys, because the legacy server never stored them. A migration also
does not reindex, so remote OpenSearch/Elasticsearch documents stay missing until
`microwave-ctl reindex` runs afterwards.

Stop the target, import, start the server, then verify with a `knife` or `chef-client` read against a
test organization and drain clients over gradually.
[`docs/deployment.md` section 10](docs/deployment.md#10-migration-from-a-legacy-server) has the full
runbook, the `chef-server.rb` translation table, and the knife importer's exact supported subset.

## Status

- **Configuration** — TOML plus `MW_*` environment overrides, including an optional TLS listener.
- **Authentication** — `X-Ops-Sign` 1.0/1.1/1.3 canonicalization, RSA signing/verification, key reassembly, and legacy password hash compatibility.
- **Key generation** — API-created keys use a bounded, thread-safe cache of pre-generated 2048-bit RSA keypairs. Background workers refill it, `keygen.size` / `keygen.workers` / `keygen.warm_up` (and `MW_KEYGEN__*`) control it, and `/_status` reports cached count, capacity, worker health, and bounded, single-flight probes of the metadata database, search backend, and object store (1s snapshot cache, stale-while-refresh).
- **Storage** — one metadata contract with two backends: SQLite (default) and PostgreSQL, selected by `database.driver` / `MW_DATABASE__DRIVER`. The HTTP API, OAuth/identity surface, embedded search, authorization policy, bootstrap, operational CLI, and snapshot migration run on either backend. SQLite remains the default and the only backend with an in-process database file. `serialized_object` documents are stored transparently compressed as `mwz1:` + base64(zlib(JSON)); legacy plain JSON remains readable, and `storage.compression = "none"` restores plain writes without breaking existing rows.
- **Authorization** — recursive ACL/group checks, cycle rejection, the default organization policy, and ACL read/write endpoints with granular `users`/`clients` responses.
- **HTTP API** — `/_status`, `/_stats`, `/server_api_version`, `/license`, users, organizations, clients, principals, key management, organization memberships/invitations, nodes, node `_identifiers`, environments, environment recipes, roles (including environment run lists), data bags, sandboxes, bookshelf uploads, cookbooks, cookbook resolution, policyfiles/policy groups, ACL endpoints, signed-request validation (`/validate/*`), search (node/role/environment/client/data-bag indexes with a query subset and partial search), data-collector forwarding and opt-in mutation action notifications, the compliance profile proxy, the removed analytics `controls` endpoint (`410 Gone`), and the optional server-enforced `required_recipe` endpoint. `/validate/*` and `_identifiers` are internal upstream APIs that Chef Server only routes on its internal listener; Microwave exposes them on the main listener to signed organization members, so restrict them at the reverse proxy if the upstream network split is required.
- **Operations CLI** — `microwave-ctl` with a `chef-server-ctl` compatibility alias (a symlink): user/org administration (`user-create`, `user-list`, `user-show`, `user-delete`, `user-edit`), server-admin grant/remove/list, org membership, password reset, key rotation (add/list/delete user and client keys), service credential rotation (`rotate-credentials`, `rotate-all-credentials`, `rotate-shared-secrets`), secrets management, OAuth application inspection (`oc-id-show-app`), filtered metadata dumps (`filtered-dump`), diagnostic log bundles (`gather-logs`), orphan authorization cleanup (`cleanup-bifrost`), maintenance state, `check-config`, `reconfigure`, `ha-status`, `psql`, status, directory or tar backup/restore, `reindex` (embedded count verification or a remote OpenSearch/Elasticsearch backfill), the `oc-chef-pedant` runner, systemd service management with a container-friendly `service start --foreground` mode, and `migrate-from-legacy` snapshot import. The same data commands work against either configured metadata backend.
- **Migration** — `microwave-migrate` exports a versioned JSON snapshot from SQLite or PostgreSQL (users with password hashes, organizations, clients, keys, authz ACLs, nodes, roles, environments, data bags, checksums, cookbooks, policies, OAuth applications/grants/tokens, telemetry, explicit/empty sandboxes, and `kvpair`) and imports it into either backend with original ids, authz ids, timestamps, hashes, keys, and token state intact. Default exports redact secrets; `--include-secrets` includes `kvpair`, OAuth grants/tokens, and OAuth application secrets, while the independent `--include-private-keys` adds explicit on-disk key/certificate material (`pivotal.pem`, `pivotal.password`, `objectstore.secret`, private-chef secrets, WebUI/oc-id keys, and configured TLS files) under `secrets/` with a manifest. `--archive PATH.tar.gz` adds local `bookshelf/` blobs with tar allowlist, traversal, symlink, absolute-path, and special-file rejection; import is confirmation-gated, restores private files mode `0600`, refuses existing keys or an empty-over-populated bookshelf without `--force`, supports `--secrets-dir`, and pre-stages private files before database changes. Data-dir private manifest entries are limited to the fixed export filename allowlist, telemetry merges without deleting target-local events, and partial-secret imports warn. Use both secret flags for a truly complete self-migration. Snapshots always contain decoded plain JSON even when the database stores compressed values. PostgreSQL imports also advance the sequence-backed identifiers. The live legacy API crawler (`from-api`) is implemented and can copy cookbook file blobs into a local object-store tree with `--bookshelf-dir` (64 MiB per blob, 4 GiB per crawl, checksum-verified and idempotent); without the flag it remains metadata-only. `import-knife-backup` imports a `knife ec backup` directory directly — users, password/key dumps, organizations, memberships, clients, containers, groups, nodes, roles, environments, data bags, ACLs, and ChefFS cookbooks with MD5 addressed blobs — without an HTTP server. With an explicit `--secrets-dir` or `--secrets-archive` it also copies server-owned private material (`pivotal.pem`/`pivotal.password`, web UI keys, `objectstore.secret`, `private-chef-secrets.json`, TLS certificate/key including Chef `nginx/ca/<fqdn>.crt|.key`, and CA files) with overwrite protection, modes, archive validation, staged/rollback writes, and no content logging. Chef client/user private keys are never stored server-side and stay client-owned. `--merge-users` reuses an existing same-named target user (including a bootstrapped `pivotal`) while preserving target credentials/profile/default keys unless `--replace-user-credentials` is given; merged users are reconciled into `server-admins` according to their final admin flag, and replacement removes a target default key the backup does not supply. `--replace-acls` removes revoked grants and membership for groups actually present in the backup, restoring prior ACL state if the import fails. The command supports `--dry-run` and requires `--yes` for non-interactive writes.
- **MCP endpoint (experimental)** — opt-in Streamable HTTP endpoint at `/organizations/<org>/mcp`, disabled by default, with a per-organization bearer token (configuration file or the `mcp` secret group) and 11 read plus 8 write tools covering nodes, environments, cookbooks, and locally stored run reports. The eight write tools are registered unless the organization sets `writes = false`, which hides them and makes the endpoint read-only. See [`docs/mcp.md`](docs/mcp.md).
- **Configuration migration** — There is no automatic `chef-server.rb` importer. Translate `/etc/opscode/chef-server.rb` to TOML/`MW_*` using `docs/deployment.md` section 10; data and key material can be migrated with the tools above.
- **OAuth/identity** — `/id` sign-in/sessions, OAuth application management, authorization-code and refresh-token flows, `/id/v1/me`, `/id/v1/me/organizations`, `/id/v1/status`, profile/password/key regeneration, password-reset tokens, and password-reset email via SMTP.
- **Mail** — optional outbound SMTP for password-reset email (`[mail]` / `MW_MAIL__*`), with STARTTLS or implicit TLS for remote relays, AUTH PLAIN/LOGIN/auto, and bounded asynchronous dispatch. Disabled by default; enabling it requires `server.public_url`. Email-change verification and invitation mail are not implemented.
- **Dependency solving** — Chef version constraints (`=`, `>`, `<`, `>=`, `<=`, `~>`), deterministic backtracking, environment constraints, role expansion, timeouts, and upstream-shaped 412 failures.
- **External backends** — S3/Garage object storage with SigV4 signing and presigned URLs, and OpenSearch/Elasticsearch search with bulk indexing and query translation. The local object store and embedded search remain the defaults.
- **Server** — `microwave-server serve` selects the configured metadata backend, bootstraps a `pivotal` superuser and listens on the configured address. It serves plain HTTP by default and HTTPS directly when `server.tls.cert_path` and `server.tls.key_path` are configured. `serve --check` initializes/migrates the backend and prints its type without exposing credentials.

Testing and formatting are green locally, and the required PR gate runs the same
suites: `.github/workflows/ci.yml` (`fmt`, `clippy`, `test`, `claims`,
`homepage`) and `.github/workflows/postgres.yml` (`postgres`). A strict stock
`chef-client` converge passes without compatibility shims:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
scripts/run-chef-client-e2e.sh
scripts/run-postgres-tests.sh
```

## Run Locally

The quickstart above has the two commands and the credentials the first run writes; use
`MW_PATHS__DATA_DIR` to choose another data directory.

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

### Mail (SMTP)

Mail is disabled by default and is used only for password-reset messages.
Enable it with a `[mail]` block and a public origin:

```toml
[server]
public_url = "https://chef.example.com"

[auth]
password_reset_ttl_seconds = 3600

[mail]
enabled = true
host = "smtp.example.com"
port = 587
security = "starttls"   # starttls | tls | none
username = "chef-mailer"
password = "..."        # prefer MW_MAIL__PASSWORD
auth = "auto"           # auto | plain | login | none
from = "Chef Infra Server <chef@example.com>"
reply_to = "ops@example.com"
```

`server.public_url` is required when mail is enabled and must be an absolute
`https` origin with no credentials, path, query, or fragment; loopback
development hosts may use `http`. Reset links are built from it instead of the
unauthenticated `Host` header.

Remote SMTP with authentication and TLS is the supported production path: use
STARTTLS (the default, port 587) or implicit TLS (port 465). `security = "none"`
is explicit plaintext for a trusted local relay and rejects SMTP AUTH for
non-loopback hosts. `mail.auth` defaults to `auto`; `plain` and `login` force a
mechanism, and `none` disables AUTH for an unauthenticated relay. `mail.from`
is required. All other mail keys are ignored while mail is disabled.

The password is never logged or serialized. `MW_MAIL__PASSWORD` overrides
`mail.password`, and `microwave-config.json` backups omit the value, so supply
it again after a restore. `auth.password_reset_token_in_response = true` still
adds `reset_url` to the response even with mail enabled; it is a development
escape hatch and is unsafe for production.

Delivery is asynchronous: the request returns the same generic success message
whether the account does not exist, has no email address, the bounded queue is
full, or the SMTP relay rejects the message. Dispatcher and SMTP failures are
logged and never change the HTTP response. There are no retries and no durable
outbox. Email-change verification and organization invitation mail are not part
of this slice.

See [`docs/deployment.md`](docs/deployment.md#mail-smtp) for the full key table
and backup/restore notes.

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
internal node identifiers, signed-request validation, analytics `controls`,
environment recipes, role environment run lists, and depsolver. The PostgreSQL half skips only when
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
The experimental MCP endpoint and its security model are documented in
[`docs/mcp.md`](docs/mcp.md); the enablement notes are in
[`docs/deployment.md`](docs/deployment.md#mcp-endpoint-experimental).
Packaging artifacts: `Dockerfile`, `.dockerignore`, `docker-compose.yml`,
`docker-compose.ha.yml`, and `packaging/`. A local Debian/Ubuntu package can be
built with [`scripts/build-deb.sh`](scripts/build-deb.sh); see
[`docs/debian-package.md`](docs/debian-package.md).

Pushing a `v*` tag builds release binaries for linux/amd64 and linux/arm64 in
`.github/workflows/release.yml` and attaches them, with SHA-256 checksums, to
the GitHub release. Only a tag whose remainder is a Debian version also produces
a `.deb`; other `v*` tags warn and ship the tarballs only.

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

- **Server admins.** `grant-server-admin-permissions`, `remove-server-admin-permissions`, and `list-server-admins` use the `users.admin` column as the source of truth and keep the global `server-admins` group membership in sync. HTTP organization listing accepts members of that group; user/org ACLs continue to use it. The configured superuser cannot be demoted.
- **Key rotation.** `add-client-key`, `add-user-key`, `list-client-keys`, `list-user-keys`, `delete-client-key`, and `delete-user-key` manage `actor_keys`. New keys are 2048-bit RSA keypairs; the private key is printed exactly once at creation and is not stored. When `--key-name` is omitted, the key name is the Chef-compatible SHA-1 public-key fingerprint. `--public-key-path` imports an existing PEM public key instead of generating one, and `--expiration-date` accepts `infinity` or an ISO-8601 timestamp.
- **Secrets.** `set-secret`, `show-secret`, and `remove-secret` store values in the `kvpair` table under `secret:<group>:<name>`. Only `show-secret` prints a value. The supported credential names mirror upstream, including `object_store secret`, `bookshelf access_key_id`, `bookshelf secret_access_key`, `data_collector token`, and the SQL password groups. At startup the server resolves the local `/bookshelf` HMAC secret from configuration, then `object_store secret`, then `<data_dir>/objectstore.secret`, and copies file-backed values into the secret store. S3/Garage access and secret keys fall back to the `bookshelf` secrets when configuration omits them.
- **Maintenance.** `maintenance on`, `maintenance off`, and `maintenance status` persist a flag in `kvpair` (`maintenance:enabled`). While the flag is on, the HTTP middleware returns `503 Service Unavailable` with a JSON error for normal API requests; `/_status` and `/_stats` remain reachable so health checks and monitoring keep working.
- **Diagnostics.** `check-config` loads the resolved configuration and validates the data directory, database URL/driver, bind address, search backend, object-store credentials, TLS certificate/key pair, and superuser name. `reconfigure` runs the same validation, opens the configured metadata backend so the schema is initialized or migrated, and reports the resolved database, search, and object-store backends plus user/org counts. It never restarts a running server and never deletes data; restart the server yourself to apply configuration changes. `ha-status` reports a single-node deployment; multi-node, DRBD, and keepalived HA are not implemented. `psql` execs `sqlite3` against the configured database, or `psql` with a configured PostgreSQL URL, and passes trailing arguments through.
- **Search reindex.** `reindex [--org ORG] [--purge]` rebuilds remote search documents from the relational store. With the embedded backend it reports that relational search is authoritative and prints the acknowledged document counts. With `opensearch`/`elasticsearch` it iterates organizations and indexes nodes, roles, environments, clients, and every data bag item through the same document builder the API uses, then reports per-type counts of documents the cluster acknowledged. Documents are written through `index_many` in `_bulk` requests of up to 500 documents, and the command calls `POST /chef/_refresh` when it finishes so results are immediately visible. A bulk response is parsed item by item: a 200 response containing `errors:true` or any failed item/status aborts the command, and rejected documents are never counted. By default it only adds or replaces documents by id and leaves documents for objects deleted while the remote backend was inactive in place. Pass `--purge` to delete each visited organization's existing remote documents with delete-by-query before rebuilding. A missing `chef` index is treated as nothing to purge, and a delete response that times out, reports failures or version conflicts, or deletes fewer documents than matched aborts the command instead of claiming a complete purge. Purging is destructive, so an interrupted run can leave an organization unsearchable until it is re-run. Both paths converge on the same document ids; running a reindex while the server is serving is supported, though concurrent writes may also be indexed by the server.
- **Pedant runner.** `pedant [TAGS...]` (also available as the upstream `test` alias) locates `scripts/run-pedant.sh` in the current tree or next to the executable, searching only up to the repository root (`.git` or the workspace `Cargo.toml`; override with `MW_PEDANT_SCRIPT`). It runs the harness with the tags as separate arguments, streams output, and exits with the harness status. No shell string is constructed, so tag values cannot inject commands.
- **Service management.** `service start|stop|restart|status|tail` delegates to `systemctl` (and `journalctl` for `tail`) for the `microwave-server` unit, overridable with `--unit`, `MW_SERVICE_UNIT`, `MW_SYSTEMCTL`, and `MW_JOURNALCTL`. Unit names are validated (no leading `-`, whitespace/control characters, or `/`) before being passed as an argument, and the systemd exit status is preserved. `tail` needs only `journalctl`. For containers and other environments without systemd, `service start --foreground` (or `MW_SERVICE_FOREGROUND=1`) resolves `microwave-server` from `MW_SERVER_BIN`, the current executable's directory, or `PATH`, validates the `--config` path, and runs `microwave-server serve --config PATH`. On Unix the CLI `exec`s the server, so the server keeps the PID and receives `SIGTERM`/`SIGINT`/`SIGHUP` directly; `SIGTERM`/`SIGINT` drain and exit cleanly, while `SIGHUP` reloads TLS. `--foreground` is rejected for the other actions. Foreground mode does not reproduce the systemd unit's user, filesystem, capability, or syscall hardening; in a container that isolation must come from the image and runtime. When `systemctl` is unavailable and foreground mode was not requested, the command fails with an actionable error that suggests `service start --foreground`.
- **Backup formats.** `backup [PATH]` writes a directory containing the SQLite `VACUUM INTO` snapshot or PostgreSQL custom-format `pg_dump` plus the local `bookshelf/` copy; PATH defaults to a timestamped `microwave-backup-<UTC>` directory in the current directory (`microwave-backup-<UTC>.tar` with `--tar`). `backup PATH.tar --format tar` (or `--tar`) writes the same payload as one uncompressed tar archive. `--config-only` writes only `microwave-config.json`, `--pg-options` forwards validated extra flags to `pg_dump`/`pg_restore` (managed options are rejected), and `--yes` is accepted for runbook compatibility because the backup runs online. With `--include-private-keys`, either format also writes an explicit `private/` subtree containing a manifest and the fixed server-private set (`pivotal.pem`, `pivotal.password`, `objectstore.secret`, `private-chef-secrets.json`/`private_chef_secrets.json`, WebUI/oc-id keys including `oc-id.pem`/`oc_id.pem`) plus configured `server.tls.cert_path`/`key_path`; there is no glob discovery, private files are `0600`, public files are `0644`, and contents are never printed. `--config-only` can be combined with `--include-private-keys`. A configured `data_dir` that is itself a symlink and a final TLS target symlink (Let's Encrypt `live/.../cert.pem|key.pem`) are resolved to canonical destinations; symlinked data-dir payload files are refused. Private restore is opt-in as well: `restore ... --restore-private-keys` validates the manifest, resolves each destination once, and preflights/stages every destination before touching live metadata, then commits them as a set after the database and bookshelf restore succeeds. Old-content backups are kept until the whole set commits and are used to roll back an ordinary failure; the message reports applied/partial state, and old contents are preserved at an exact reported path if rollback itself fails. Staging uses private mode-`0700` directories in the target parent so renames stay on one filesystem (a SIGKILL may leave that directory for manual cleanup). `restore ... --restore-private-keys --private-only` applies only `private/` for retrying keys without replaying metadata; an empty private set is a no-op without a prompt. Backup destinations are fully preflighted: re-running a full backup into the same destination fails before the existing private tree is touched, only a fully recognizable ctl private tree is ever replaced, and a populated tree is never replaced by a default backup or an empty include. A SIGKILL during the private-tree swap can leave old keys at a reported `.microwave-private-old-*` sibling path. Without `--restore-private-keys`, private files are skipped with a warning and existing target keys are untouched. Restoring keys can invalidate existing clients, so the normal destructive confirmation still applies. Restore validates and copies the backup bookshelf into staging before replacing the live database or bookshelf, so a symlink/FIFO in a directory backup fails without changing live data; tar restore validates its entry allowlist the same way, including refusing archive traversal, absolute paths, symlinks, hard links, devices, FIFOs, and oversized private entries (even without `--restore-private-keys`). The default backup is key-**file**-free, not DB-secret-free: database-backed secrets (including the object-store secret copied into `kvpair`) are always inside the database dump and are restored with it. Neither format includes S3/Garage objects or the remote search index.
- **PostgreSQL operations.** `status`, user/org/key/secret/maintenance commands, `check-config`, `reconfigure`, `ha-status`, `psql`, `backup`, `restore`, `reindex`, and `migrate-from-legacy` all use the configured backend. PostgreSQL `backup` writes `microwave.dump` with `pg_dump --format=custom`; `restore` validates the archive with `pg_restore --list`, prints the target without credentials, compares the archive's source database, and then runs `pg_restore --clean --if-exists --single-transaction`. `restore` is destructive and requires confirmation or `--yes`. PostgreSQL passwords are removed from client-tool command lines; `password` is passed through `PGPASSWORD`, while `sslpassword` is placed in a temporary `0600` libpq service file referenced by `PGSERVICEFILE`/`PGSERVICE`. Environment variables and temporary files may still be observable to the same UID on some platforms. Install `pg_dump`/`pg_restore` on the host. `migrate-from-legacy`, `microwave-migrate import`, and `microwave-migrate import-knife-backup` also ask for confirmation unless `--yes` is given. The knife importer requires an explicit `--bookshelf-dir` with `--database-url` so cookbook blobs are written to a known local object-store root. Commands that need the store apply the embedded schema on connect; `status` and `check-config` use `connect_existing` so read-only probes do not create tables. SQLite backup/restore keeps the existing `VACUUM INTO` and staged-file path.

## Remaining Work

The open inventory is generated under [Known gaps](#known-gaps); this section keeps the pedant record.

### Pedant Compatibility

A full `scripts/run-pedant.sh strict` run on 2026-10-30 is recorded in
[`docs/pedant-report.md`](docs/pedant-report.md) and
[`docs/pedant-extended-report.md`](docs/pedant-extended-report.md). The core set passes with
**652 passed / 0 failed / 8 pending of 660**; the 8 pending examples are pedant's own skips.

| Tag | Passed | Failed | Pending |
| --- | ---: | ---: | ---: |
| headers | 7 | 0 | 0 |
| users | 165 | 0 | 0 |
| organizations | 15 | 0 | 0 |
| clients | 139 | 0 | 3 |
| nodes | 326 | 0 | 5 |
| **Total** | **652** | **0** | **8** |

Every core tag passes. The one historical core failure, the plain-HTTP `headers` artifact, is fixed
at the harness rather than an allowlist; see `docs/pedant-report.md`.

Extended suites: **3,284 passed / 0 failed / 7 pending of 3,291**. All twelve extended tags are green, as are the `environments` (610 examples, 23 pending) and `roles` (78 examples) contract suites.

See `docs/superpowers/specs/2026-10-24-microwave-server-design.md` and `TODO.md` for the full target and remaining work.

## Layout

| Path | Contents |
|---|---|
| `chef-server/` | Vendored upstream server `15.10.127` source snapshot (commit `c38d169f083d064b8c542d912d072d8b88cf4c8c`). |
| `chef/` | Vendored upstream client `19.4.47` source snapshot (commit `46354347bc125edc98c5f080ca2592d55ec0e4bc`). |
| `crates/` | Rust workspace crates. |
| `bin/` | Server and compatibility binaries. |
| `.github/workflows/` | Pull-request, PostgreSQL, and nightly pedant gates. |
| `docs/superpowers/specs/` | Architecture and design specifications. |
| `packaging/` | systemd unit, Debian package metadata, example configuration, Garage config/bootstrap for Compose. |
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

## License

Microwave Server is licensed under the GNU Affero General Public License, version 3 only (AGPL-3.0-only). See [LICENSE](LICENSE) for the full text. Vendored upstream sources under `chef/` and `chef-server/` keep their original Apache-2.0 licensing.

