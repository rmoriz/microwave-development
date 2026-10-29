# Deploying Microwave Server

This guide covers packaging and operations for the Rust server in this
repository: a single binary with embedded storage, Docker and systemd
packaging, external OpenSearch/S3 backends, backup/restore, migration, and the
operational caveats that matter in practice.

For the architecture and the compatibility target see
[`docs/superpowers/specs/2026-10-24-microwave-server-design.md`](superpowers/specs/2026-10-24-microwave-server-design.md).
For the current implementation gaps see
[`docs/remaining-work.md`](remaining-work.md) and [`TODO.md`](../TODO.md).

> **Reality check.** SQLite metadata is the default and the best-tested
> deployment: **one server process per database**. PostgreSQL metadata is
> implemented and selectable through `database.driver`; several replicas can
> point at one PostgreSQL database, but that topology has not been
> load-validated. The operational CLI, snapshot migration, and backup/restore
> support PostgreSQL too; backup/restore uses `pg_dump`/`pg_restore`. OpenSearch
> and S3/Garage can be used from either backend. The limitations are called out
> in place rather than hidden.

## Contents

1. [Artifacts and layout](#1-artifacts-and-layout)
2. [Configuration](#2-configuration)
3. [Single-node install](#3-single-node-install)
4. [Docker](#4-docker)
5. [Docker Compose](#5-docker-compose)
6. [systemd](#6-systemd)
7. [External backends (OpenSearch, S3/Garage)](#7-external-backends-opensearch-s3garage)
8. [HA layout](#8-ha-layout)
9. [Backup and restore](#9-backup-and-restore)
10. [Migration from a legacy server](#10-migration-from-a-legacy-server)
11. [Health, status, and logs](#11-health-status-and-logs)
12. [Maintenance caveats](#12-maintenance-caveats)
13. [Upgrades and rollback](#13-upgrades-and-rollback)
14. [Troubleshooting](#14-troubleshooting)

## 1. Artifacts and layout

The workspace builds four binaries:

| Binary | Purpose |
|---|---|
| `microwave-server` | HTTP daemon. `serve` (with `--check` for a preflight) and `config` (print resolved config). |
| `microwave-ctl` | Operational CLI. |
| `chef-server-ctl` | Compatibility alias built from the same CLI crate; identical command surface. |
| `microwave-migrate` | Versioned JSON snapshot export/import, live crawler, and offline `knife ec backup` directory import. |

Build them from source:

```sh
cargo build --release --locked -p microwave-server -p microwave-ctl-bin -p microwave-migrate-bin
# target/release/{microwave-server,microwave-ctl,chef-server-ctl,microwave-migrate}
```

Recommended installation paths:

| Path | Contents |
|---|---|
| `/usr/local/bin/` | The four binaries above. |
| `/etc/microwave/microwave.toml` | Configuration (mode `0640 root:microwave`). |
| `/etc/microwave/tls/` | Optional TLS certificate chain and private key; provisioned by the operator. |
| `/var/lib/microwave/` | Data directory (mode `0750`, owner `microwave`). |

`microwave-ctl pedant` runs the repository harness `scripts/run-pedant.sh`. It
is a development and verification tool rather than a server component: run it
from a checkout, install the script at
`/usr/local/share/microwave/run-pedant.sh`, or point `MW_PEDANT_SCRIPT` at it.
The harness itself needs Ruby, bundler, and the vendored pedant tree.

The data directory contains:

| Entry | Notes |
|---|---|
| `microwave.db` (+ `-wal`, `-shm`) | SQLite metadata and the embedded search source of truth. |
| `bookshelf/` | Cookbook blobs for the `local` object store (`organization-<org_id>/checksum-<md5>`). |
| `pivotal.pem` | Superuser private key, created on first start, mode `0600`. |
| `pivotal.password` | Generated superuser password, created on first start, mode `0600`. |
| `objectstore.secret` | Local `/bookshelf` HMAC secret, created on first start, mode `0600`. |
| `webui_priv.pem` / `webui_pub.pem` | Optional legacy web-UI keypair migrated by `import-knife-backup --secrets-dir`; `0600` / `0644`. |
| `private-chef-secrets.json` | Optional migrated upstream secret document, mode `0600`. |
| `ssl/cert.pem`, `ssl/key.pem`, `ca/**` | Optional migrated TLS/CA material; keys are `0600`, certificates `0644`, copied to configured TLS paths or this data directory. |

Nothing outside the data directory is written at runtime. The process does not
need root, a home directory, or a local search service, Java, or Erlang stack.

## 2. Configuration

Configuration is TOML plus `MW_*` environment overrides. Precedence, lowest to
highest:

1. built-in defaults,
2. the file passed with `--config`, else `$MW_CONFIG`, else `./microwave.toml`
   if it exists,
3. `MW_*` environment variables.

`microwave-ctl` takes the same global flag **before** the subcommand:

```sh
microwave-ctl --config /etc/microwave/microwave.toml status
```

A complete annotated example is
[`packaging/microwave.toml.example`](../packaging/microwave.toml.example).

### Keys and environment overrides

| TOML key | Environment | Default | Notes |
|---|---|---|---|
| `server.bind` | `MW_SERVER__BIND` | `0.0.0.0:8000` | Listener address. Plain HTTP unless `server.tls` is configured. |
| `server.worker_threads` | `MW_SERVER__WORKER_THREADS` | `0` | `0` = size from available CPUs. |
| `server.public_url` | `MW_SERVER__PUBLIC_URL` | unset | Public origin used to build password-reset links, e.g. `https://chef.example.com`. Required when mail is enabled. Must be an absolute origin with no credentials, path, query, or fragment. `https` is required except for loopback hosts, which may use `http`. |
| `server.tls.enabled` | `MW_SERVER__TLS__ENABLED` | unset | `true` requires both paths; `false` forces plain HTTP; unset enables TLS when either path is set (the other is then required and validated). |
| `server.tls.cert_path` | `MW_SERVER__TLS__CERT_PATH` | unset | PEM certificate chain, leaf first. Setting either path enables TLS. |
| `server.tls.key_path` | `MW_SERVER__TLS__KEY_PATH` | unset | PEM private key matching the certificate. Never printed or logged. |
| `server.tls.reload_interval_seconds` | `MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS` | `0` | Periodic certificate/key reload interval in seconds. `0` disables polling; `SIGHUP` still reloads immediately on Unix. Requires TLS, maximum `604800` (seven days). |
| `paths.data_dir` | `MW_PATHS__DATA_DIR` | `./data` | All mutable state. |
| `database.driver` | `MW_DATABASE__DRIVER` | `sqlite` | `sqlite` or `postgres`. |
| `database.url` | `MW_DATABASE__URL` | `sqlite://<data_dir>/microwave.db` | `sqlite://<path>` or a `postgres://` URL. Both backends are supported by the server, `microwave-ctl`, and `microwave-migrate`. |
| `storage.compression` | `MW_STORAGE__COMPRESSION` | `zlib` | `zlib` stores new `serialized_object` rows as `mwz1:` + base64(zlib(JSON)); `none` stores plain JSON. Reads accept both formats either way, so existing databases keep working. |
| `search.backend` | `MW_SEARCH__BACKEND` | `embedded` | `embedded`, `opensearch`, `elasticsearch`. |
| `search.url` | `MW_SEARCH__URL` | unset | Required for the remote backends, e.g. `http://opensearch:9200`. |
| `search.timeout_ms` | `MW_SEARCH__TIMEOUT_MS` | `30000` | Request timeout for remote backend HTTP calls. Must be greater than zero; bounds `_bulk`, `_delete_by_query`, and `_refresh` so an unresponsive cluster cannot blackhole the server or CLI. |
| `depsolver.timeout_ms` | `MW_DEPSOLVER__TIMEOUT_MS` | `20000` | Dependency solve deadline; must be greater than zero. |
| `depsolver.max_concurrent` | `MW_DEPSOLVER__MAX_CONCURRENT` | `4` | Concurrent solver permits; excess requests receive `503 Dependency solver overloaded`. |
| `search.strict_result_acls` | `MW_SEARCH__STRICT_RESULT_ACLS` | `true` | Hide search rows the caller cannot READ. Upstream calls this `strict_search_result_acls`; the reported `total` remains the pre-filter count. |
| `object_store.backend` | `MW_OBJECT_STORE__BACKEND` | `local` | `local`, `s3`, `garage`. |
| `object_store.bucket` | `MW_OBJECT_STORE__BUCKET` | unset | Required for `s3`/`garage`. |
| `object_store.endpoint` | `MW_OBJECT_STORE__ENDPOINT` | unset | Required for `s3`/`garage`. See the reachability caveat below. |
| `object_store.region` | `MW_OBJECT_STORE__REGION` | `us-east-1` | Must match the S3 gateway region. |
| `object_store.access_key` | `MW_OBJECT_STORE__ACCESS_KEY` | unset | Falls back to the `bookshelf.access_key_id` secret. |
| `object_store.secret_key` | `MW_OBJECT_STORE__SECRET_KEY` | unset | Falls back to the `bookshelf.secret_access_key` secret. |
| `object_store.path_style` | `MW_OBJECT_STORE__PATH_STYLE` | `true` | Keep `true` for Garage/MinIO. |
| `object_store.secret` | `MW_OBJECT_STORE__SECRET` | generated | Only used by the local `/bookshelf` backend. |
| `auth.skew_seconds` | `MW_AUTH__SKEW_SECONDS` | `900` | `X-Ops-Timestamp` tolerance. |
| `auth.superuser_name` | `MW_AUTH__SUPERUSER_NAME` | `pivotal` | Account that cannot be demoted and receives platform-superuser privileges. |
| `server.default_orgname` | `MW_SERVER__DEFAULT_ORGNAME` | unset | Rewrite upstream short paths (`/nodes`, `/search`, `/cookbooks`, …) to this organization. Signature verification still uses the client's original path. |
| `server.trusted_proxies` | `MW_SERVER__TRUSTED_PROXIES` | empty | Comma-separated IPs/CIDRs/socket addresses allowed to set `X-Forwarded-Host`/`X-Forwarded-Proto`. Untrusted peers fall back to `Host`. |
| `stats.username` / `stats.password` | `MW_STATS__USERNAME` / `MW_STATS__PASSWORD` | unset | Optional Basic auth for `/_stats`; when set, `/_status` remains public. |
| `auth.password_reset_token_in_response` | `MW_AUTH__PASSWORD_RESET_TOKEN_IN_RESPONSE` | `false` | Development-only: return the `/id/password-reset` URL in the HTTP response. Leave disabled in production; it allows account takeover. This still applies when mail is enabled. |
| `auth.password_reset_ttl_seconds` | `MW_AUTH__PASSWORD_RESET_TTL_SECONDS` | `3600` | Password-reset token lifetime in seconds. Must be greater than zero and at most `604800` (seven days). The reset email derives its expiry text from this value. |
| `mail.enabled` | `MW_MAIL__ENABLED` | `false` | Enable outbound password-reset mail. Disabled by default; all other mail keys are ignored while this is false or `mail.host` is empty. |
| `mail.host` | `MW_MAIL__HOST` | unset | SMTP relay hostname. Required when mail is enabled. |
| `mail.port` | `MW_MAIL__PORT` | derived | Defaults to `587` for `starttls`, `465` for `tls`, and `25` for `none`. Must be greater than zero. |
| `mail.security` | `MW_MAIL__SECURITY` | `starttls` | `starttls` (default), `tls` (implicit TLS/SMTPS), or `none` (explicit plaintext). `none` rejects SMTP AUTH for non-loopback hosts; remote authenticated relays must use `starttls` or `tls`. |
| `mail.username` | `MW_MAIL__USERNAME` | unset | SMTP AUTH username. Requires `mail.password`. |
| `mail.password` | `MW_MAIL__PASSWORD` | unset | SMTP AUTH password. Prefer the environment variable. Never logged or serialized, and absent from `microwave-config.json` backups. |
| `mail.auth` | `MW_MAIL__AUTH` | `auto` | `auto`, `plain`, `login`, or `none`. `auto`, `plain`, and `login` require a username and password; `none` rejects credentials. |
| `mail.from` | `MW_MAIL__FROM` | unset | `From` header, for example `Chef Infra Server <chef@example.com>`. Required when mail is enabled; a display name is accepted. |
| `mail.reply_to` | `MW_MAIL__REPLY_TO` | unset | Optional `Reply-To` header. |
| `mail.timeout_ms` | `MW_MAIL__TIMEOUT_MS` | `10000` | SMTP timeout in milliseconds. Must be greater than zero. |
| `mail.hello_name` | `MW_MAIL__HELLO_NAME` | unset | EHLO/HELO name. When unset the mail library's default is used. |
| `mail.tls_ca_path` | `MW_MAIL__TLS_CA_PATH` | unset | Extra PEM CA certificate used to verify a relay with a private CA; system roots remain trusted. Read when the mailer is built at startup, so an unreadable or unparseable file fails startup. |
| `keygen.size` | `MW_KEYGEN__SIZE` | `3` | Maximum number of pre-generated 2048-bit RSA keypairs kept in memory. `0` disables caching and generates on demand. Maximum `1024`. |
| `keygen.workers` | `MW_KEYGEN__WORKERS` | `1` | Background key-generation threads. Must not exceed `keygen.size`; maximum `8`. `0` leaves refilling to `warm_up`. |
| `keygen.warm_up` | `MW_KEYGEN__WARM_UP` | `false` | Fill the cache synchronously during startup. Requires `keygen.size > 0`. Leave `false` to keep startup fast and let workers refill in the background. |
| `data_collector.store` | `MW_DATA_COLLECTOR__STORE` | `forward` | `forward`, `local`, or `both`. `local`/`both` persist `run_converge` reports locally, and `local`/`both` also keep `POST /organizations/:org/data-collector` enabled without `root_url`. |
| `data_collector.keep_runs` | `MW_DATA_COLLECTOR__KEEP_RUNS` | `50` | Reports kept per `(organization, node)`; `0` keeps everything. Old reports are pruned after each successful insert. |
| `data_collector.root_url` | `MW_DATA_COLLECTOR__ROOT_URL` | unset | Enables forwarding of the payload to the external collector, e.g. `https://automate.example.com/data-collector/v0/`. |
| `data_collector.token` | `MW_DATA_COLLECTOR__TOKEN` | unset | Shared bearer token accepted inbound and sent upstream as `x-data-collector-token`. Falls back to the `data_collector.token` secret. |
| `data_collector.timeout` | `MW_DATA_COLLECTOR__TIMEOUT` (alias `MW_DATA_COLLECTOR__TIMEOUT_MS`) | `30000` | Upstream forwarding and response-read timeout in milliseconds (TOML alias `timeout_ms`). Must be greater than zero; `check-config` rejects `0`. |
| `data_collector.actions_enabled` | `MW_DATA_COLLECTOR__ACTIONS_ENABLED` | `false` | Opt in to `oc_chef_action` mutation notifications while `root_url` is configured. `root_url` alone only enables client payload forwarding. Actions are only sent when `store` allows forwarding (`forward`/`both`), and are disabled automatically when `root_url` is unset. |
| `data_collector.service_hostname` | `MW_DATA_COLLECTOR__SERVICE_HOSTNAME` | unset | `service_hostname` reported in action messages. Falls back to the request `X-Forwarded-Host` (or `Host`), then `localhost`. |
| `compliance_proxy.url` | `MW_COMPLIANCE_PROXY__URL` | unset | Enables the Automate compliance profile proxy routes. Non-empty value only; unset means every method returns `404`. |
| `compliance_proxy.token` | `MW_COMPLIANCE_PROXY__TOKEN` | unset | Service token sent upstream. Falls back to `data_collector.token` or the `data_collector.token` secret. |
| `compliance_proxy.token_header` | `MW_COMPLIANCE_PROXY__TOKEN_HEADER` | `x-data-collector-token` | Header that carries the service token. Invalid, framing, routing, and hop-by-hop header names are rejected by config validation. |
| `compliance_proxy.timeout` | `MW_COMPLIANCE_PROXY__TIMEOUT` (alias `MW_COMPLIANCE_PROXY__TIMEOUT_MS`) | `30000` | Request and response-read timeout in milliseconds (TOML alias `timeout_ms`). Must be greater than zero. |
| `compliance_proxy.insecure_skip_tls_verify` | `MW_COMPLIANCE_PROXY__INSECURE_SKIP_TLS_VERIFY` | `false` | Skip certificate verification for a test/private compliance upstream. Keep `false` in production. |
| `required_recipe.enabled` | `MW_REQUIRED_RECIPE__ENABLED` (alias `MW_REQUIRED_RECIPE__ENABLE`) | `false` | Enables `GET /organizations/:org/required_recipe`. TOML alias `enable`. |
| `required_recipe.path` | `MW_REQUIRED_RECIPE__PATH` | unset | Recipe file served as `text/plain` when the endpoint is enabled. |
| `telemetry.enabled` | `MW_TELEMETRY__ENABLED` | `false` | Reserved toggle. No outbound sender is implemented, so enabling it does not phone home. |
| `telemetry.reporting_url` | `MW_TELEMETRY__REPORTING_URL` | unset | Reserved reporting destination for a future opt-in exporter. |
| `telemetry.interval_seconds` | `MW_TELEMETRY__INTERVAL_SECONDS` | `86400` | Reserved exporter interval. |

`storage.compression` applies to new writes. Decompression is capped at 64 MiB
per value; corrupt or oversized compressed values fail with a storage error
instead of panicking. Imports through `microwave-ctl` inherit the server
configuration; the standalone `microwave-migrate import` and
`import-knife-backup` accept `--compression zlib|none` or `--config <path>`
(with `MW_STORAGE__COMPRESSION` as an environment override).

Validate a configuration file without starting the server, or validate it and
initialize/migrate the configured metadata backend:

```sh
microwave-ctl --config /etc/microwave/microwave.toml check-config
microwave-ctl --config /etc/microwave/microwave.toml reconfigure
microwave-server config --config /etc/microwave/microwave.toml
```

`check-config` validates paths, the database URL/driver, the bind address, the
search backend and URL, object-store credentials, the TLS certificate/key pair,
and the superuser name.
`reconfigure` performs the same checks, opens the configured store so the
schema is initialized or migrated, and reports the resolved SQLite/PostgreSQL,
search, and object-store backends plus user and organization counts. It does
**not** restart a running server and does not delete data; restart the service
yourself to apply configuration changes. It is safe to run repeatedly.
`microwave-server config` prints the **resolved configuration as JSON,
including object-store credentials** — treat its output as a secret.

### TLS listener

Plain HTTP is the default. To serve HTTPS directly from the server, configure a
PEM certificate chain and its matching private key:

```toml
[server]
bind = "0.0.0.0:8443"

[server.tls]
cert_path = "/etc/microwave/tls.crt"
key_path = "/etc/microwave/tls.key"
```

The same settings are available as environment variables, which is convenient
for containers:

```sh
MW_SERVER__BIND=0.0.0.0:8443 \
MW_SERVER__TLS__CERT_PATH=/etc/microwave/tls.crt \
MW_SERVER__TLS__KEY_PATH=/etc/microwave/tls.key \
microwave-server serve
```

`cert_path` may contain the leaf certificate followed by intermediate
certificates; `key_path` is the PEM private key (PKCS#8, PKCS#1, or SEC1) that
belongs to the leaf. The listener advertises `h2` and `http/1.1` via ALPN.
Both `microwave-server serve --check` and
`microwave-ctl check-config` verify that the files exist, are readable regular
files, contain the expected PEM objects, and form a matching pair. They print
only the paths, never key material. An explicitly configured but invalid pair
aborts startup before the listener binds.

Operational notes:

- Keep the private key mode `0600`, owned by the service user (or
  `root:microwave` with mode `0640`). The certificate may be world-readable.
- **Rotation does not require a restart.** Replace both PEM files, then send
  `SIGHUP` to the server process on Unix:

  ```sh
  kill -HUP "$(pidof microwave-server)"
  # or, with the packaged systemd unit:
  sudo systemctl reload microwave-server
  # or, in Docker:
  docker kill --signal=HUP microwave-tls
  ```

  The replacement pair goes through the same validation as startup. Only after
  it parses and the key matches the certificate is it swapped in atomically for
  new connections; existing connections keep using the pair they negotiated.
  If reading or validating the replacement fails, the server logs a clear
  error and continues serving with the previous certificate. Key material is
  never logged. On Unix the handler is installed before database bootstrap and
  the listener bind, so an early `SIGHUP` cannot terminate the process. The
  same handler is installed for plain-HTTP listeners and logs that TLS is
  disabled.
- Replace each file atomically (temporary file plus `rename`) so a reload never
  reads a partially written file. The two renames are not one atomic operation:
  a reload that races them may observe a new certificate with the old key, fail
  validation, and keep the previous pair. Send another `SIGHUP` or wait for the
  next polling interval; the completed rotation is then picked up.
- To poll instead of signalling, set `server.tls.reload_interval_seconds`
  (for example `60`). The timer reloads once at startup and then only when the
  certificate or key file metadata changed, using the same validation and
  failure behavior. `0` disables polling. SIGHUP remains available on Unix and
  is the fastest way to complete a rotation.
- SIGHUP is handled for every listener. On a plain-HTTP listener it logs
  `TLS is disabled; nothing to reload` and continues, so `systemctl reload` and
  `docker kill --signal=HUP` remain safe when TLS is not enabled. The signal
  still does not re-read the rest of the configuration.
- The certificate must be valid for the hostname clients use. `chef-client`
  verifies the server URL hostname and the issuing CA; a self-signed
  certificate is suitable only for tests or when the CA is installed on every
  client.
- When TLS is enabled, presigned URLs for the local `/bookshelf` backend use
  `https://<server.bind>` instead of `http://`, and API-generated URLs fall
  back to `https` when no `X-Forwarded-Proto` header is present. A proxy may
  still override the scheme with that header.
- A TLS-terminating load balancer or reverse proxy in front of plain HTTP
  remains fully supported and is still the simplest option when you need
  automatic certificate rotation, multiple hostnames, or centralized TLS
  policy.

### Mail (SMTP)

Outbound mail is disabled by default and currently serves only the
`POST /id/password-reset` flow. Enable it with a `[mail]` block plus
`server.public_url`:

```toml
[server]
public_url = "https://chef.example.com"

[auth]
password_reset_ttl_seconds = 3600

[mail]
enabled = true
host = "smtp.example.com"
port = 587
security = "starttls"       # starttls | tls | none
username = "chef-mailer"
password = "secret"         # prefer MW_MAIL__PASSWORD
auth = "auto"               # auto | plain | login | none
from = "Chef Infra Server <chef@example.com>"
reply_to = "ops@example.com"
```

`server.public_url` is mandatory when mail is enabled and must be an absolute
`https` origin with no credentials, path, query, or fragment; `http` is
accepted only for loopback development hosts (`localhost`, `127.0.0.1`, `::1`).
Reset links are built from it rather than from `Host` or forwarded headers.

Remote SMTP with authentication and TLS is the supported production path.
`security = "starttls"` is the default and uses port 587; `security = "tls"`
uses implicit TLS on port 465; `security = "none"` is explicit plaintext on port
25 and is intended for a trusted local relay. Plaintext AUTH is rejected for
non-loopback hosts, so a remote relay with credentials must use STARTTLS or
TLS. `mail.auth` defaults to `auto`; `plain`/`login` force a mechanism, and
`none` disables AUTH. `mail.from` is required; `mail.hello_name` overrides the
EHLO name, and `mail.tls_ca_path` adds a private CA certificate while keeping
the system roots.

The SMTP password may be set with `mail.password` or `MW_MAIL__PASSWORD`; the
environment variable wins. It is redacted from `Debug` and logs and is never
serialized, so `microwave-config.json` backups do not contain it. After a
restore or when replaying a configuration snapshot, provide the secret again
through `MW_MAIL__PASSWORD` (or re-add `mail.password` to the live file).

`check-config` loads and validates the section, including the required fields,
the public URL, and the token TTL, without sending mail. An enabled but invalid
configuration fails startup rather than silently disabling mail. `tls_ca_path`
is read when the mailer is constructed at startup, so an unreadable or
unparseable CA file fails startup as well.

On a reset request, the server creates a token valid for
`auth.password_reset_ttl_seconds` (default one hour, maximum seven days) and, if
mail is enabled and the account exists with a non-empty email address, enqueues
one plain-text message containing
`{server.public_url}/id/password-reset?token=...`. Delivery is asynchronous
through a bounded 64-message queue with one worker; there are no retries and no
durable outbox ([gap-mail-outbox](remaining-work.md#gap-mail-outbox)). A full
queue, stopped worker, or SMTP failure is logged without changing the response.
Unknown accounts and accounts without an email address send nothing.

The HTTP response is always the existing generic success message, regardless of
whether a message was sent. `auth.password_reset_token_in_response = true` still
adds a usable `reset_url` to that response even when mail is enabled; it is a
development escape hatch that lets any unauthenticated caller obtain a reset
link and is unsafe for production.

Email-change verification and organization invitation mail are not implemented
in this slice.

### MCP endpoint (experimental)

The optional MCP endpoint exposes 11 read tools and 8 write tools per
organization over Streamable HTTP at `/organizations/<org>/mcp`. It is
disabled by default and authenticates with a per-organization bearer token
instead of Chef request signing. Enable it with an `[mcp]` block plus at least
one `[mcp.organizations.<org>]` entry; see [`docs/mcp.md`](mcp.md) for token
setup (configuration file or `microwave-ctl set-secret mcp <org> <token>`),
principal and ACL guidance, `allowed_hosts` for remote agents, the full tool
reference, and limitations.

Two operational points:

- The run-report tools only see reports stored by this server. Local storage
  is opt-in: set `data_collector.store` to `local` or `both`. The default
  `forward` mode keeps no reports locally.
- `writes` defaults to `true`. With it, the endpoint includes destructive
  tools (`delete_node`, `delete_environment`, `delete_cookbook_version`) that
  run with the configured principal's ACLs and have no dry-run or
  confirmation step. Set `writes = false`, or use a dedicated non-superuser
  principal, for a read-only experiment.

## 3. Single-node install

```sh
# 1. Binary and service account
sudo install -m 0755 target/release/microwave-server  /usr/local/bin/
sudo install -m 0755 target/release/microwave-ctl     /usr/local/bin/
sudo install -m 0755 target/release/chef-server-ctl   /usr/local/bin/
sudo install -m 0755 target/release/microwave-migrate /usr/local/bin/
sudo useradd --system --uid 10001 --home-dir /var/lib/microwave \
     --shell /usr/sbin/nologin microwave

# 2. Directories and configuration
sudo install -d -m 0750 -o microwave -g microwave /var/lib/microwave
sudo install -d -m 0755 /etc/microwave
sudo install -m 0640 -g microwave packaging/microwave.toml.example \
     /etc/microwave/microwave.toml

# 3. Preflight and first start (foreground)
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml check-config
sudo -u microwave microwave-server serve --config /etc/microwave/microwave.toml
```

On first start the server creates the database, the `pivotal` superuser, its
keypair, and (for the local object store) the bookshelf secret. Read the
generated credentials once and move them to your secret manager:

```sh
sudo cat /var/lib/microwave/pivotal.password
sudo cat /var/lib/microwave/pivotal.pem
```

The private key is never printed again and is not stored in the database; it
lives only in `pivotal.pem`. Back it up, for example with
`microwave-ctl backup --include-private-keys`, and protect that backup.

Create an organization and an operator:

```sh
CTL="microwave-ctl --config /etc/microwave/microwave.toml"
$CTL user-create alice --email alice@example.com --admin
$CTL org-create acme --full-name "ACME"
$CTL org-user-add acme alice
$CTL org-list
$CTL status
```

`org-create` creates the `<org>-validator` client with a default key, but the
CLI does not print that private key. To get a bootstrap key for `knife`, add a
named key (the private key is printed exactly once):

```sh
$CTL add-client-key acme acme-validator --key-name bootstrap
```

Point existing clients at the server by changing the Chef server URL; the
API surface and signing protocol are unchanged. Either enable the [TLS
listener](#tls-listener) or put a TLS-terminating proxy in front of the
listener.

## 4. Docker

The [`Dockerfile`](../Dockerfile) is multi-stage: it builds the workspace on
`rust:<version>-trixie`, then copies only the four binaries into
`debian:trixie-slim` with `ca-certificates`, `curl`, and `sqlite3`.

Image properties:

- runs as UID/GID `10001:10001` (`microwave`), never as root;
- data volume at `/var/lib/microwave`;
- configuration at `/etc/microwave/microwave.toml` (baked-in example, meant to
  be bind-mounted over);
- `MW_CONFIG` and `RUST_LOG=info` are set;
- `HEALTHCHECK` probes `/_status` on port `${MW_HTTP_PORT:-8000}`; set
  `MW_HEALTHCHECK_SCHEME=https` for a TLS listener, `MW_HEALTHCHECK_HOST` when
  the certificate only covers a DNS name, and `MW_HEALTHCHECK_INSECURE=1` (or
  `true`) when the health probe cannot verify the certificate (self-signed or
  not valid for the probe hostname); any other value keeps verification on;
- no `ENTRYPOINT`, so `docker run <image> microwave-ctl ...` and
  `docker compose exec microwave microwave-ctl ...` work;
- the default command is `microwave-ctl service start --foreground --config
  /etc/microwave/microwave.toml`. The CLI validates the config path, resolves
  `microwave-server` from `MW_SERVER_BIN`, its own directory, or `PATH`, and
  `exec`s `microwave-server serve --config ...`. The result is that the server
  is PID 1, exactly as with a direct `exec`, so `docker stop`/`docker kill`
  signals reach it. `SIGTERM` and `SIGINT` trigger graceful shutdown;
  `SIGHUP` still reloads the TLS pair. `MW_SERVICE_FOREGROUND=1` is the
  environment alternative to `--foreground` for `service start`. The previous
  direct command
  `microwave-server serve --config /etc/microwave/microwave.toml` still works
  as a `command:`/`docker run` override.

```sh
docker build -t microwave-server:local .
docker run -d --name microwave \
  -p 8000:8000 \
  -v microwave-data:/var/lib/microwave \
  -v "$PWD/packaging/microwave.toml.example:/etc/microwave/microwave.toml:ro" \
  microwave-server:local

docker exec microwave microwave-ctl status
docker exec microwave cat /var/lib/microwave/pivotal.password
curl -fsS http://127.0.0.1:8000/_status
```

The image's default command is the supported container entrypoint. It can also
be passed explicitly, which is useful when a `docker run`/Compose `command:`
overrides the image default:

```sh
docker run -d --name microwave \
  -p 8000:8000 \
  -v microwave-data:/var/lib/microwave \
  -v "$PWD/packaging/microwave.toml.example:/etc/microwave/microwave.toml:ro" \
  microwave-server:local \
  microwave-ctl service start --foreground --config /etc/microwave/microwave.toml
```

Set `MW_SERVICE_FOREGROUND=1` instead of `--foreground` if that suits the
deployment template. `--foreground` is valid only for `start`; the other
actions still control a systemd unit. `microwave-ctl` validates the config
path and `exec`s the server, so the container's PID 1 is `microwave-server`.
The direct form
`microwave-server serve --config /etc/microwave/microwave.toml` remains
supported and behaves the same apart from skipping the CLI's path validation.

To serve HTTPS directly from the container, mount the certificate and key
read-only and enable TLS with environment variables. The files must be
readable by UID `10001`. The container has no shell-based entrypoint that
changes ownership, so set permissions before starting it:

```sh
docker run -d --name microwave-tls \
  -p 8443:8443 \
  -v microwave-data:/var/lib/microwave \
  -v /etc/microwave/tls:/etc/microwave/tls:ro \
  -e MW_SERVER__BIND=0.0.0.0:8443 \
  -e MW_SERVER__TLS__CERT_PATH=/etc/microwave/tls/tls.crt \
  -e MW_SERVER__TLS__KEY_PATH=/etc/microwave/tls/tls.key \
  -e MW_HEALTHCHECK_SCHEME=https \
  -e MW_HEALTHCHECK_INSECURE=1 \
  microwave-server:local

curl -kfsS https://127.0.0.1:8443/_status
```

`MW_HEALTHCHECK_INSECURE=1` (or `true`) is only for self-signed certificates
or probes that cannot match the certificate hostname; any other value keeps
certificate verification enabled. With a publicly trusted certificate, set
`MW_HEALTHCHECK_HOST` to a name covered by the certificate, or leave the
default port and scheme and let a proxy expose TLS.

To rotate the certificate of a running container, replace both files in the
mounted directory atomically and signal the container. The server does not
need to restart:

```sh
# copy the new tls.crt/tls.key into /etc/microwave/tls first
docker kill --signal=HUP microwave-tls
docker logs --tail 5 microwave-tls   # "reloaded TLS certificate and key"
curl -kfsS https://127.0.0.1:8443/_status
```

`docker kill --signal=HUP` delivers the signal to PID 1 of the container.
The default `microwave-ctl service start --foreground` command `exec`s
`microwave-server`, so PID 1 is the server itself even though the command line
starts with `microwave-ctl`. Alternatively set
`MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS=60` and let the server poll the mounted
files. Docker secrets are materialized as files, so rotating a secret requires
recreating the container (or a periodic `docker service update`); bind mounts
and Kubernetes projected volumes update in place and can be reloaded with
SIGHUP. A failed reload keeps the previous certificate and logs an error
without dropping the listener.

Use `--build-arg RUST_VERSION=<x.y>` to pin the compiler instead of tracking
the latest stable `rust:1-*` image.

`docker compose exec` runs the command *instead of* `CMD`, which is why the
image intentionally has no `ENTRYPOINT`.

## 5. Docker Compose

[`docker-compose.yml`](../docker-compose.yml) is the default embedded profile:
SQLite, in-process search, local `/bookshelf`, and a named `microwave-data`
volume.

```sh
cp packaging/microwave.toml.example microwave.toml
MW_CONFIG_FILE=./microwave.toml docker compose up -d --build
docker compose exec microwave microwave-ctl --config /etc/microwave/microwave.toml status
docker compose logs -f microwave
docker compose down            # volumes are kept
```

`MW_PORT` changes the published host port. If you change the listener port in
the mounted configuration, also change the container port mapping and
`MW_HTTP_PORT` so the healthcheck still targets the right port. For an HTTPS
listener also set `MW_HEALTHCHECK_SCHEME=https`, plus
`MW_HEALTHCHECK_INSECURE=1` only when the probe cannot verify the certificate.

[`docker-compose.ha.yml`](../docker-compose.ha.yml) is an override that adds
Garage (S3) and OpenSearch. The checked-in images are
`dxflrs/garage:v2.5.0` and `opensearchproject/opensearch:2.19.7`; both were
validated end-to-end with the Compose profile:

```sh
docker compose -f docker-compose.yml -f docker-compose.ha.yml up -d --build
```

This profile:

- keeps SQLite metadata on the local volume (the default backend; switch
  `MW_DATABASE__DRIVER=postgres` to use PostgreSQL instead);
- sets `MW_SEARCH__BACKEND=opensearch` and `MW_SEARCH__URL=http://opensearch:9200`;
- sets `MW_OBJECT_STORE__BACKEND=garage` with bucket `chef-cookbooks` and the
  placeholder credentials defined in the compose file and `packaging/garage.toml`;
- bootstraps Garage (layout, bucket, key) from
  [`packaging/garage-bootstrap.sh`](../packaging/garage-bootstrap.sh), using
  [`packaging/Dockerfile.garage`](../packaging/Dockerfile.garage) to add a shell
  to the otherwise static official Garage image;
- publishes Garage on host port `3900` and points
  `MW_OBJECT_STORE__ENDPOINT` at `host.docker.internal:3900`.

The endpoint caveat matters: the server uses the same endpoint for its own
S3 calls *and* for presigned URLs it hands to `chef-client`. The URL must
resolve from both the server container and the clients. `host.docker.internal`
works on the Docker host only; for remote nodes use a DNS name behind a
reverse proxy and set `MW_S3_ENDPOINT` to it.

The Garage credentials and `rpc_secret` in this repository are placeholders.
Replace them for anything that is not a throwaway environment. For production,
run a real Garage cluster (replication factor > 1, multiple nodes) or AWS
S3/MinIO and point the `MW_OBJECT_STORE__*` variables at it; no bootstrap
container is then needed.

The compose profile is not a multi-replica HA setup. Do not scale the
`microwave` service while it uses SQLite. See [HA layout](#8-ha-layout).

## 6. systemd

The unit is [`packaging/systemd/microwave-server.service`](../packaging/systemd/microwave-server.service).

```sh
sudo install -d -m 0750 -o microwave -g microwave /var/lib/microwave
sudo install -d -m 0755 /etc/microwave
sudo install -m 0640 -g microwave packaging/microwave.toml.example \
     /etc/microwave/microwave.toml
sudo install -m 0644 packaging/systemd/microwave-server.service \
     /etc/systemd/system/microwave-server.service
sudo systemctl daemon-reload
sudo systemctl enable --now microwave-server
```

The unit:

- runs as `microwave:microwave`;
- runs `microwave-ctl check-config` as `ExecStartPre`;
- creates `/var/lib/microwave` via `StateDirectory` and `/etc/microwave` via
  `ConfigurationDirectory`;
- writes only to the data directory (`ProtectSystem=strict` +
  `ReadWritePaths=/var/lib/microwave`);
- restarts on failure, not on clean exit;
- has no reload signal — restart the unit to apply configuration changes.

Check it:

```sh
systemctl status microwave-server
journalctl -u microwave-server -f
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml status
```

The CLI can drive the same unit when `systemctl` is available:

```sh
sudo microwave-ctl service start
sudo microwave-ctl service stop
sudo microwave-ctl service restart
microwave-ctl service status
microwave-ctl service tail --lines 100 --follow
```

`service` delegates to `systemctl` and, for `tail`, `journalctl`, using the
`microwave-server` unit. Override the unit with `--unit` or
`MW_SERVICE_UNIT`, and the tool paths with `MW_SYSTEMCTL` and `MW_JOURNALCTL`.
Unit names are validated before invocation: a leading `-`, whitespace or
control characters, or `/` is rejected so an environment override cannot become
an option. The systemd exit status is preserved. `tail` only requires
`journalctl`. If `systemctl` is not installed, the command fails with an
actionable error; it does not attempt to track or signal a process started
elsewhere. For a container or any host without systemd, use the foreground
mode (`service start --foreground`, or `MW_SERVICE_FOREGROUND=1 service start`)
or a direct `microwave-server serve` under your platform's supervisor. The
foreground mode resolves the binary from `MW_SERVER_BIN`, the directory of the
running `microwave-ctl`, or `PATH`, validates `--config`, and `exec`s
`microwave-server serve` on Unix so signals and the PID behave as a direct
launch.

The foreground/container mode is a launch convenience, not a replacement for
the hardened unit below. It does not apply `User`/`Group`, `StateDirectory`,
`UMask=0077`, `NoNewPrivileges`, `ProtectSystem`/`ReadWritePaths`,
`PrivateTmp`/`PrivateDevices`, the syscall/namespace filters, or the capability
bounds. In a container those controls must come from the image user and the
container runtime; on a host without systemd, run the foreground process under
a supervisor that supplies equivalent isolation. Keep the systemd unit for
normal host deployments.

If you bind a privileged port inside the unit, add a drop-in with
`AmbientCapabilities=CAP_NET_BIND_SERVICE` and
`CapabilityBoundingSet=CAP_NET_BIND_SERVICE`. Environment overrides go in a
drop-in as well:

```ini
# /etc/systemd/system/microwave-server.service.d/env.conf
[Service]
Environment=MW_AUTH__SKEW_SECONDS=1200
Environment=RUST_LOG=info,microwave_api=debug
```

## 7. External backends (OpenSearch, S3/Garage)

### Search

Microwave creates the `chef` index with the upstream mapping (whitespace analyzer;
keyword `X_CHEF_*` fields) on the first search or write and during reindex. The
cluster account therefore needs `create_index`/`PUT /chef` permission. A missing
index is treated as an empty result on the search path.

```toml
[search]
backend = "opensearch"          # or "elasticsearch"
url = "http://opensearch:9200"
```

The backend uses the upstream `chef` index, `_bulk` for writes and
`_search` with the organization/object-type constraints expressed as a filter
query. Anonymous access is assumed; put the cluster behind your normal network
controls.

**Backfilling after a switch or cluster loss.** `microwave-ctl reindex`
rebuilds remote documents from the relational store, which remains the source
of truth:

```sh
microwave-ctl --config /etc/microwave/microwave.toml reindex
microwave-ctl --config /etc/microwave/microwave.toml reindex --org acme
microwave-ctl --config /etc/microwave/microwave.toml reindex --org acme --purge
```

It indexes nodes, roles, environments, clients, and every data bag item for
all organizations (or the selected one) and prints per-type acknowledged
counts. Documents are sent through `index_many` in `_bulk` requests of up to
500 documents, and `POST /chef/_refresh` runs when the rebuild finishes so the
results are immediately searchable. Bulk responses are checked item by item:
HTTP 200 with `errors:true` or any failed item/status aborts the command, so it
cannot silently report success when the cluster rejected documents, and
rejected documents are not counted. With `search.backend = "embedded"` it
prints the relational document counts and reports that relational search is
authoritative instead of doing wasted work.

By default the reindex adds or replaces documents by store id. It does **not**
delete documents for objects that were removed while the remote backend was
inactive. Pass `--purge` to delete each visited organization's existing remote
documents with `_delete_by_query` before rebuilding them. A missing `chef`
index is treated as nothing to purge. The delete response is checked for
`timed_out`, `failures`, version conflicts, and fewer deleted documents than
matched; any of those aborts the command before the purge is reported as
complete. Purging is destructive: if the command is interrupted after the
delete, that organization stays unsearchable until the reindex is run again.
Running a reindex while the server is serving is supported, but concurrent
writes may be indexed by the server as well; both paths converge on the same
document id.

Remote backend HTTP calls use `search.timeout_ms` (default 30 seconds), so an
unreachable or stalled cluster fails the command instead of hanging
indefinitely.

### Object store

```toml
[object_store]
backend = "garage"              # or "s3"
endpoint = "https://cookbooks.example.com"
bucket = "chef-cookbooks"
region = "garage"
path_style = true
access_key = "GK..."
secret_key = "..."
```

- The key layout is `organization-<org_id>/checksum-<md5>` in both the local
  and S3 backends.
- Credentials may instead be stored as secrets:
  `microwave-ctl set-secret bookshelf access_key_id ...` and
  `set-secret bookshelf secret_access_key ...`.
- `endpoint` is used for server-side `PUT`/`HEAD`/`GET`/`DELETE` **and** for
  presigned URLs returned to clients. Use a name that resolves from both
  sides; a Docker network alias or `localhost` only works for same-host
  deployments.
- Presigned URL lifetime and clock skew matter: keep the server and clients
  within the `auth.skew_seconds` window.
- Deleting a cookbook garbage-collects unreferenced checksums; with a shared
  bucket, ensure no other writer is using the same prefix.

### The local object store caveat

With `backend = "local"`, presigned `/bookshelf` URLs are built from
`server.bind` (for example `http://0.0.0.0:8000/bookshelf/...`). They are not
derived from the incoming `Host`/`X-Forwarded-Host` the way API URLs are. For a
single host this is harmless; behind NAT, a proxy, or for remote
`chef-client` nodes it is not usable. Options today:

- bind the server to the address clients use, when that address is local to
  the host, or
- use S3/Garage for cookbook blobs (recommended for anything fronted by a
  proxy).

This is a server limitation, not a configuration mistake.

## 8. HA layout

### What is supported today

- **Embedded single node** (`SQLite` + in-process search + local bookshelf):
  the default. One process, one host, one writer.
- **Single node with shared OpenSearch and S3/Garage**: removes blob and
  search state from the host, but SQLite metadata is still on a local volume.
- **PostgreSQL metadata (experimental HA)**: several server replicas may point
  at one PostgreSQL database. OAuth tokens, sessions, keys, and ACLs are
  shared, and the schema migration is serialized with an advisory lock. Use
  S3/Garage for blobs and share nothing else; this topology has not been
  load-validated ([gap-multi-replica-ha](remaining-work.md#gap-multi-replica-ha)).
  On every server start, `PostgresStore::connect` applies the
  embedded idempotent schema under that advisory lock before bootstrap runs,
  so replicas can start concurrently without manual migration steps.
- **PostgreSQL operations from the CLI.** `microwave-ctl` runs user, org, key,
  secret, maintenance, status, and diagnostic commands against either backend.
  `backup` writes a custom-format `pg_dump` archive and `restore` replays it
  with `pg_restore`; both require the matching PostgreSQL client tools on the
  host. `microwave-migrate` exports and imports the same JSON snapshot for
  SQLite and PostgreSQL, and `microwave-ctl migrate-from-legacy` imports into
  either backend.
- **Active/passive**: keep a second host ready, replicate backups/config, and
  move traffic at the load balancer. With SQLite only one server may write the
  metadata database at a time.

### What is not supported

- **Multiple replicas sharing one SQLite database.** SQLite allows a single
  writer; network filesystems add locking hazards. Do not point two hosts at
  the same database file. Use PostgreSQL instead.
- **DRBD/keepalived style clustering.** `microwave-ctl ha-status` reports
  `mode: standalone`, `nodes: 1`, `high_availability: not configured`, and
  says so explicitly.
- **LDAP/Active Directory external authentication.** Chef's `external_authentication_uid`
  fields are stored and reported, but there is no LDAP/AD bind, no unlinked-account
  handling, and no `local` bypass authentication ([gap-ldap-fips](remaining-work.md#gap-ldap-fips)).
  Users with external UIDs can only be created and managed, not authenticated against a
  directory.
- **FIPS mode.** The Rust crypto stack is not configured or validated as FIPS
  ([gap-ldap-fips](remaining-work.md#gap-ldap-fips)).
- **Legacy `/etc/opscode/chef-server.rb` import.** Microwave uses TOML and `MW_*`
  environment variables. Existing Ruby configuration must be translated; the server
  does not execute or parse `chef-server.rb`
  ([gap-chef-server-rb-importer](remaining-work.md#gap-chef-server-rb-importer)),
  `CHEF_SECRETS_DATA` or `private-chef-secrets.json` at startup
  ([gap-secrets-file](remaining-work.md#gap-secrets-file)). Migration tools can
  carry explicit key material, but configuration is not imported automatically.
- **Embedded search indexing.** The default backend is a relational full scan,
  not Tantivy or another inverted index
  ([gap-embedded-search-index](remaining-work.md#gap-embedded-search-index)). It
  is correct for small and medium organizations, but scales linearly. Larger
  installations should use OpenSearch or Elasticsearch.
- **Additional mail flows.** Only password-reset email is implemented.
  Email-change verification, invitation mail, SMTP rate limiting, and a durable
  outbox/retry are not available ([gap-mail-outbox](remaining-work.md#gap-mail-outbox)).

### Target layout with PostgreSQL metadata

```text
clients ──TLS──> L7 load balancer ──HTTP──> microwave-server replica A ─┐
                                     └───> microwave-server replica B ─┤
                                                                        ├─> PostgreSQL (metadata + ACLs)
                                                                        ├─> OpenSearch (search index)
                                                                        └─> S3 / Garage (cookbook blobs)
```

Design notes that already hold:

- **No sticky sessions.** OAuth tokens, sessions, keys, and ACLs live in the
  metadata store, not in process memory. Any replica can serve any request.
- **URL construction** follows `Host`, `X-Forwarded-Host`, and
  `X-Forwarded-Proto` for API URLs. Preserve those headers at the load
  balancer; otherwise the API returns URLs pointing at the wrong scheme or
  host.
- **Clock sync** across replicas and clients is required by `X-Ops-Timestamp`;
  keep NTP healthy and only widen `auth.skew_seconds` as a last resort.
- **S3 endpoint** must be client-facing because it appears in presigned URLs.
- **Per-replica local data**: `pivotal.pem`/`pivotal.password` and (for the
  local backend) `bookshelf/` are node-local. In a future shared-backend
  topology, distribute the superuser key material through your secret manager
  and use S3 for blobs.

Until the PostgreSQL topology is load-tested, prefer vertical capacity,
`pg_dump` backups, and a warm standby over active/active.

## 9. Backup and restore

Two complementary mechanisms exist.

### Database backup (`microwave-ctl backup`)

```sh
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml \
  backup /var/backups/microwave/2026-10-24-db
```

`PATH` is optional. Without it the command writes a timestamped
`microwave-backup-<UTC>` directory in the current directory (or
`microwave-backup-<UTC>.tar` with `--tar`). `--yes` is accepted for upstream
runbook compatibility; it does not stop the service, and the backup remains
online. `--config-only` writes only a resolved `microwave-config.json` snapshot
and skips metadata and bookshelf data. `--pg-options VALUE` forwards validated
extra flags to `pg_dump`/`pg_restore`; options managed by this CLI (destination,
database, format, transaction mode, and so on) are rejected.

With the default directory format, the destination must be a directory
(created if missing). For SQLite it must not already contain `microwave.db`;
for PostgreSQL it must not already contain `microwave.dump`. For
`--config-only` it must not already contain `microwave-config.json`. All
destination conflicts (database snapshot/dump, configuration snapshot,
bookshelf, tar file, and a previous `private/` tree) are preflighted before
anything is replaced. Re-running a full backup into the same destination
therefore fails without modifying the existing private tree. The command
writes:

- `microwave.db` for SQLite — a `VACUUM INTO` snapshot, consistent including
  committed WAL contents, safe to take while the server is running;
- `microwave.dump` for PostgreSQL — a custom-format `pg_dump` archive, taken
  from a consistent database snapshot, safe while the server is running;
- `bookshelf/` — a recursive copy of the local blob store, if present. The
  copy refuses symlinks and other non-regular files (FIFOs, sockets, devices)
  instead of dereferencing or blocking on them; the tar builder also runs with
  `follow_symlinks(false)`;
- `microwave-config.json` when `--config-only` is given — the fully resolved
  configuration. It is a snapshot for inspection or re-provisioning, not an
  input consumed by `restore`. `--config-only` can be combined with
  `--include-private-keys`, in which case the backup contains the config
  snapshot and the private tree but no database or bookshelf.

The configuration snapshot includes the resolved `[mail]` settings but never
`mail.password`: the secret is not serialized into `microwave-config.json`. If
mail is enabled, supply it again with `MW_MAIL__PASSWORD` after a restore or
when re-provisioning from the snapshot.

For a single-file archive with the same payload, use the tar format:

```sh
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml \
  backup /var/backups/microwave/2026-10-24-db.tar --format tar
# `--tar` is equivalent shorthand
```

The archive is written atomically to the requested file and refuses to replace
an existing file. It is uncompressed, so it is large but inspectable with
`tar tf`; compress it with your backup tooling if needed. It contains exactly
the same database snapshot/dump and `bookshelf/` tree as the directory format,
plus `microwave-config.json` only when `--config-only` was given.
Symlinks and other non-regular files under `bookshelf/` abort the backup rather
than leaking link targets or blocking on a FIFO. Data-directory private
payload files that are symlinks are likewise refused. Configured *final TLS
targets* may be managed symlinks and are resolved explicitly, as described
below. On Unix, file copies also open with `O_NOFOLLOW`/`O_NONBLOCK`; see the
restore notes below for the residual directory-level TOCTOU caveat.

PostgreSQL backup requires `pg_dump` to be installed and on `PATH`. The
password is removed from the child process command line. A `password` query
parameter or authority password is supplied through `PGPASSWORD`; an
`sslpassword` is written to a temporary libpq service file with mode `0600`
and referenced through `PGSERVICEFILE`/`PGSERVICE`. Depending on the platform
and hardening, a child's environment or temporary files may still be
observable to the same UID or root; treat backups and the host account
accordingly. Other connection parameters (host, port, user, database,
`sslmode`) are preserved.

By default it does **not** include private key/certificate **files**:

- `pivotal.pem`, `pivotal.password`, `objectstore.secret`,
  `private-chef-secrets.json`/`private_chef_secrets.json`, WebUI/oc-id keys, or
  TLS files. Pass `--include-private-keys` to add them (see below).

The default backup is key-file-free, **not** DB-secret-free. Database-backed
secrets are always inside the database dump and are restored with it. That
includes the object-store secret copied into `kvpair`; file-level
`--include-private-keys` does not control database secrets.

It also does not include:

- S3/Garage objects when an external object store is configured;
- the OpenSearch index (it is derived data);
- `microwave.toml` or systemd drop-ins. Use `--config-only` to capture the
  resolved configuration as `microwave-config.json`; full backups do not
  include configuration automatically.

`--include-private-keys` adds a fixed, explicitly manifested `private/`
subtree to either the directory or tar format. Use a **new destination**; an
existing backup cannot be extended with keys:

```sh
# Default: database + bookshelf, no private key files.
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml \
  backup /var/backups/microwave/2026-10-24-db

# Full: also include the fixed private key/certificate file set.
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml \
  backup /var/backups/microwave/2026-10-24-full --include-private-keys
```

The full backup contains `private/manifest.json` and `private/files/`, with
entries only for these data-directory files when present:

- `pivotal.pem`, `pivotal.password`, `objectstore.secret`,
  `private-chef-secrets.json`, `private_chef_secrets.json`;
- `webui_priv.pem`, `webui_pub.pem`, `oc_id_priv.pem`, `oc_id_pub.pem`,
  `ocid_priv.pem`, `ocid_pub.pem`, `oc-id.pem`, `oc_id.pem`.

It also includes the configured `server.tls.cert_path` and
`server.tls.key_path`; those paths may be outside the data directory. There is
no directory glob: unknown files are never read. A configured `data_dir` that
is itself a symlink is resolved to its canonical directory, matching server
startup. Data-directory payload candidates that are symlinks are refused.
Explicitly configured TLS paths may be final-target managed symlinks (for
example Let's Encrypt `live/.../cert.pem`); backup follows them, and restore
replaces the canonical regular-file target while preserving the symlink.

The manifest records a logical kind, the suggested target (a data-relative
name or `tls-cert`/`tls-key`), and the canonical mode (`0600` for private
files, `0644` for public files such as certificates and public keys). File
contents are never printed; the command reports only payload counts. When no
allowlisted private file exists, it writes an empty manifest and does not
claim that key material was included.

A full backup is even more sensitive than a default backup: it contains
unencrypted private key files in addition to the database secrets. Protect the
destination and its transport accordingly. When a backup writes over a
destination that already has `private/`, it only replaces a fully recognizable
microwave-ctl private tree (expected manifest format/version, `manifest.json`,
`files/`, and only the referenced regular payload files); extra or unrelated
content is refused rather than deleted. A populated recognized tree is never
replaced by a default backup or by `--include-private-keys` with an empty
private set; choose a new destination or remove the old tree explicitly. An
empty recognized tree may still be replaced. The old private tree is kept until
the database and bookshelf backup has completed and the new private tree has
been fully staged, then swapped via a sibling directory with rollback on
failure.

A SIGKILL during that swap can leave the destination without `private/` and
the old keys at a sibling `.microwave-private-old-*` path. If a later failure
prevents restoring the old tree, the error reports that exact path; recover the
keys from it rather than deleting it.

The bookshelf copy and private-file collection are not atomic with the
database snapshot. Uploads or key rotations in flight during the copy can be
missed. For a strict backup, stop or drain traffic at the load balancer first,
or use S3 versioning/replication for blobs.

### Restore

Stop the server first. `restore` replaces the live database and bookshelf
directory in place.

```sh
sudo systemctl stop microwave-server
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml \
  restore /var/backups/microwave/2026-10-24-db
sudo systemctl start microwave-server
```

`restore` prints the target (without credentials) and asks for confirmation.
In scripts and other non-interactive environments, pass `--yes`. A directory
and a tar archive are accepted; the format is detected from the path, or can be
forced with `--format directory|tar` (`--tar` is shorthand). PostgreSQL
restores accept `--pg-options VALUE` with the same validation as `backup`.
Tar entries are validated before extraction: only `microwave.db`,
`microwave.dump`, `microwave-config.json`, `bookshelf/`, and the fixed
`private/manifest.json` + `private/files/<file>` layout are allowed, and
archive entries with absolute paths, `..`, symlinks, hard links, devices,
FIFOs, or unknown names are rejected. A `microwave-config.json` entry is
extracted for completeness but is not applied by `restore`; it never contains
`mail.password`, so set `MW_MAIL__PASSWORD` after restore when mail is enabled.
Private entries whose tar header exceeds the per-file or total size limit are
rejected even without `--restore-private-keys`. Extraction happens in a
temporary directory, so refusing confirmation never touches live data. For
PostgreSQL it reads the archive TOC first and reports the database the dump was
created from; a mismatch with the configured target is refused unless `--yes`
is given.

Private key/certificate restore is **opt-in and separate** from the database
and bookshelf restore:

```sh
sudo systemctl stop microwave-server
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml \
  restore /var/backups/microwave/2026-10-24-full --restore-private-keys
sudo systemctl start microwave-server
```

Without `--restore-private-keys`, a backup containing `private/` is restored
for database and bookshelf only: the private files are skipped with a warning
and existing target key files are left untouched. With the flag, the manifest
is validated (fixed format/version, allowlisted targets, canonical modes,
referenced files present, no unreferenced files, no symlinks or traversal) and
**every private destination is preflighted before live metadata is modified**.
That preflight resolves each destination exactly once and pins it: the
data-directory root may itself be a symlink and is canonicalized, data-dir
payload filenames are never re-canonicalized, and a configured final TLS target
symlink is resolved once to its canonical regular file. A missing TLS target is
pinned by canonicalizing its nearest existing ancestor (following a managed
`live/current` parent symlink) and appending the missing file name. A symlink
observed at the pinned final target during staging is rejected; any symlink that
appears later is atomically replaced by the final rename, never followed.
Unsafe parent types are refused, missing parents are created with mode `0700`,
and writability is verified by staging each payload and a temporary copy of any
previous target in a mode-`0700` staging directory in the target parent.
Keeping staging on the target filesystem makes the commit atomic; as a
fallback, a SIGKILL can leave a private staging directory beside the target,
which should be removed manually. The database and bookshelf are restored next,
and only then are the private files committed as a set. Private files are
`0600` and public files `0644`.

If an ordinary private commit fails, files already applied in that run are
rolled back from their temporary copies, and the error reports `applied X of
Y`, the failing target, and any rollback failure that leaves partial private
state. Old-content backups are retained until the whole set commits. If even
rollback cannot restore a file, the old contents are preserved at the exact
path reported in the error (and the staging directory is kept if a sibling
copy is not possible) for operator recovery. The already-restored database and
bookshelf remain intact. To retry just the keys without replaying metadata and
bookshelf, use:

```sh
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml \
  restore /var/backups/microwave/2026-10-24-full \
  --restore-private-keys --private-only
```

`--private-only` still validates the manifest/tar entries and requires
`--restore-private-keys` and the normal confirmation. It leaves the database
and bookshelf untouched. If the private manifest has no payload entries,
`--private-only` is a successful no-op and does not prompt.

Restoring `pivotal.pem`, WebUI/oc-id keys, or TLS material can invalidate
existing clients, break a running server until restart, or make the configured
TLS certificate/key pair inconsistent. The normal destructive confirmation
(interactive `yes` or `--yes`) is still required. Private restore does not
restart the server or validate that the restored TLS certificate and key form
a matching pair.

`restore`:

- for SQLite, verifies `microwave.db` with `PRAGMA integrity_check` before
  touching live files;
- for PostgreSQL, validates `microwave.dump` with `pg_restore --list` (the TOC
  is captured, not printed) before dropping any live object, then runs
  `pg_restore --clean --if-exists --single-transaction`. The target database
  must be reachable and the archive must match the schema; extra objects that
  are not in the archive are not removed;
- refuses a SQLite backup that is the live database;
- validates and copies `bookshelf/` from the backup into a staging directory
  **before** touching the live database or live bookshelf. A symlink, FIFO, or
  other bad entry fails the restore while live metadata and blobs are still
  intact;
- stages the SQLite database copy, then removes the live database and its
  `-wal`/`-shm` sidecars before renaming the staged file into place;
- swaps in the staged `bookshelf/` only after the database restore succeeds,
  keeping the previous tree under a sidecar name until the rename completes;
  a backup without a bookshelf still leaves the installation without one;
- opens the restored SQLite database once to run schema migrations, so an
  incompatible backup fails the command instead of the next start. PostgreSQL
  schema migrations run on the next `PostgresStore::connect`.

On Unix, regular bookshelf files are opened with `O_NOFOLLOW` and
`O_NONBLOCK`, so a symlink or FIFO swapped in after the directory-entry check is
rejected instead of followed or blocked on. A directory replaced by a symlink
between the type check and traversal remains a narrow TOCTOU window; run
backups and restores against a quiescent bookshelf, as required by the existing
non-atomic bookshelf snapshot caveat.

PostgreSQL restore requires `pg_restore` on `PATH` and is destructive: stop the
server (and any other replica using the database) before running it. Private
files are restored only when the backup contains them and
`--restore-private-keys` is given; S3 objects are never restored by this
command. Use bucket replication for those.

### Metadata snapshot (`microwave-migrate`)

```sh
microwave-migrate export --data-dir /var/lib/microwave --output /var/backups/snapshot.json
microwave-migrate import --data-dir /var/lib/microwave-new --input /var/backups/snapshot.json --yes

# Include kvpair (secrets/sessions/maintenance), OAuth grants/tokens, and
# OAuth application secrets. Treat the result as a credential.
microwave-migrate export --data-dir /var/lib/microwave \
  --output /var/backups/snapshot-with-secrets.json --include-secrets

# Self-contained archive: snapshot + local bookshelf blobs
microwave-migrate export --data-dir /var/lib/microwave \
  --archive /var/backups/microwave-migration.tar.gz
microwave-migrate import --data-dir /var/lib/microwave-new \
  --archive /var/backups/microwave-migration.tar.gz --yes

# Complete self-migration: database secrets plus on-disk private keys/TLS.
# --include-secrets and --include-private-keys are independent; use both for
# a truly complete copy. The archive is credential material.
microwave-migrate export --data-dir /var/lib/microwave \
  --archive /var/backups/microwave-full.tar.gz \
  --include-secrets --include-private-keys \
  --config /etc/microwave/microwave.toml
microwave-migrate import --data-dir /var/lib/microwave-new \
  --archive /var/backups/microwave-full.tar.gz \
  --config /etc/microwave/microwave.toml --yes

# Restore private files to a staging directory instead, or replace existing
# keys deliberately (overwriting can invalidate existing clients):
microwave-migrate import --data-dir /var/lib/microwave-new \
  --archive /var/backups/microwave-full.tar.gz \
  --secrets-dir /var/lib/microwave-new/restored-keys --yes
microwave-migrate import --data-dir /var/lib/microwave-new \
  --archive /var/backups/microwave-full.tar.gz \
  --config /etc/microwave/microwave.toml --force --yes

# Store imported serialized objects as plain JSON instead of the default MWZ1
# envelope (or point --config at the target server's microwave.toml):
microwave-migrate import --data-dir /var/lib/microwave-new \
  --input /var/backups/snapshot.json --compression none --yes

# PostgreSQL source and target
microwave-migrate export \
  --database-url postgres://microwave:secret@db/microwave \
  --output /var/backups/snapshot.json
microwave-migrate import \
  --database-url postgres://microwave:secret@db/microwave-new \
  --input /var/backups/snapshot.json \
  --yes

# PostgreSQL archive with a local bookshelf tree
microwave-migrate export \
  --database-url postgres://microwave:secret@db/microwave \
  --bookshelf-dir /var/lib/microwave/bookshelf \
  --archive /var/backups/microwave-migration.tar.gz
microwave-migrate import \
  --database-url postgres://microwave:secret@db/microwave-new \
  --bookshelf-dir /var/lib/microwave-new/bookshelf \
  --archive /var/backups/microwave-migration.tar.gz \
  --yes
```

`import` merges into live metadata and asks for confirmation. Use `--yes` in
scripts and other non-interactive environments. The target is printed without
authority or query-string passwords.

The snapshot is versioned JSON (`format: microwave-server-snapshot`,
`version: 1`) containing users (including password hashes), organizations,
clients, actor keys, ACLs, groups, containers, nodes, roles, environments, data
bags, checksums, cookbooks, cookbook artifacts, policies, policy groups,
OAuth applications, OAuth grants/tokens, explicit and empty sandboxes, telemetry
rows, and `kvpair`. Import applies in one transaction on either backend and
preserves ids, authz ids, timestamps, hashes, keys, and token expiry/revocation
state. Telemetry merges by `(property, event_timestamp)`, so target-local events
are preserved and repeated imports do not duplicate rows. PostgreSQL imports
advance the sequence-backed cookbook, artifact, and OAuth identifiers so later
inserts do not collide.

**Secret default.** Without `--include-secrets`, the export omits `kvpair`
(operator secrets, browser sessions, password-reset tokens, and the maintenance
flag), OAuth access grants/tokens, and OAuth application secrets. The envelope
records `secrets_included: false`. Use `--include-secrets` only when the output
is stored and transferred as a credential, and remember that the CLI prints
only whether secrets were included, never their values. Old snapshots without
the new collections or the `secrets_included` field still import unchanged.

**Private-key default.** `--include-private-keys` is a separate opt-in for
on-disk material under `secrets/`. It reads only an explicit manifest:

- `<data_dir>/pivotal.pem`, `pivotal.password`, `objectstore.secret`
- `<data_dir>/private-chef-secrets.json` or `private_chef_secrets.json`
- WebUI/oc-id files when present: `webui_{priv,pub}.pem`,
  `oc_id_{priv,pub}.pem`, `ocid_{priv,pub}.pem`, `oc-id.pem`, `oc_id.pem`
- configured `server.tls.cert_path` and `server.tls.key_path` (the certificate
  path may contain the full chain)

No glob is used, symlinked data-dir candidates abort the export, explicitly
configured TLS paths may be managed symlinks and are read through, and no file
is read unless the flag is set. The CLI prints only `Private keys:
included/excluded`.
For a complete migration pass both `--include-secrets` and
`--include-private-keys`.

Archives (`--archive PATH.tar.gz`) add the local `bookshelf/` tree and,
optionally, `secrets/manifest.json` plus `secrets/files/...` to the snapshot.
They are written atomically with mode `0600`. Archive size is capped at
1,000,000 entries and 16 GiB total uncompressed bytes. Import validates tar
entries before extraction (only `snapshot.json`, `bookshelf/`, and `secrets/`,
no absolute paths, `..`, symlinks, hard links, or special files), stages them
privately, checks private-file destinations, parent directories, and overwrite
conflicts, then imports the database in one transaction. It swaps the
bookshelf into place and commits the pre-staged private files as mode `0600`
with newly created parent directories mode `0700`. The previous bookshelf is
retained until the private-file commit succeeds and restored if that commit
fails. Import remains confirmation-gated; pass `--yes` non-interactively.

A `data-dir` private-file manifest entry is restricted to the fixed export
allowlist. Nested paths such as `nested/evil.conf`, `microwave.db`, absolute
paths, `..`, and inconsistent `suggested_name` values are rejected.

An empty archive `bookshelf/` will not replace a populated live bookshelf
without `--force`. If the live `bookshelf` path is a symlink, the symlink
itself is moved aside and replaced with the restored directory; its target is
never followed or modified.

Private files restore to the target data directory and the target config's TLS
paths by default. Pass `--secrets-dir DIR` to put every private file under one
directory instead, or `--force` to replace an existing key file or a populated
bookshelf with an empty archive one. Overwriting a key can invalidate clients
that trust the old key, so use `--force` only for a deliberate replacement.

A safe snapshot (`secrets_included: false`) prints a warning during import;
re-export with `--include-secrets` for a complete database-secret restore.

For S3/Garage/MinIO there is no local bookshelf to archive. Replicate the
bucket with your object-store tooling (`rclone`, `aws s3 sync`, Garage
replication, versioning) and use a JSON snapshot for metadata, or provide a
local mirror via `--bookshelf-dir` for the archive. Trying to archive a
PostgreSQL database without `--bookshelf-dir` fails with that instruction
rather than silently omitting blobs.

Import into a fresh database when possible. The same unique-name rule applies
on both backends: importing a `server-admins` or other global group with a
different id into a database that already has that name fails rather than
rewriting live ACL references.

Still not included unless explicitly requested: private key files are excluded
by default and included with `--include-private-keys`; remote object-store
objects and the search index are never archived. The snapshot and archive still
contain password hashes, public keys, ACLs, and user data, so even a safe export
is sensitive. `microwave-ctl backup` is a database-file/dump plus bookshelf
backup by default, so it always contains database-backed secrets (including
the object-store secret in `kvpair`) but no private key files. Pass
`--include-private-keys` for the fixed server key/certificate file set, use a
new destination (an existing backup cannot be extended), and pass
`--restore-private-keys` on restore. The `microwave-migrate` archive remains
the path for a cross-installation migration that also carries metadata
secrets.

## 10. Migration from a legacy server

**Current state:** the live crawler is implemented. `microwave-migrate
from-api` reads a legacy Chef Infra Server through its read-only HTTP API and
writes the same versioned snapshot; see the
[`microwave-migrate` README](../crates/microwave-migrate/README.md) for the
crawled collections and the API limitations (notably password hashes, which
the legacy API never returns). `microwave-ctl migrate-from-legacy --input
<snapshot>` imports that snapshot into the configured SQLite or PostgreSQL
database.

Pass `--bookshelf-dir DIR` to also copy cookbook file blobs into a local
object-store tree at `DIR/organization-<org_id>/checksum-<md5>`. Existing
checksums are skipped, relative file URLs are signed and resolved against
`--url`, absolute presigned URLs are fetched directly, and every payload is
MD5-verified before an atomic write. A blob is limited to 64 MiB and one crawl
to 4 GiB. Without the flag `from-api` remains metadata-only.

### Migrating configuration from `/etc/opscode/chef-server.rb`

Microwave does not parse or execute the legacy Ruby configuration. There is no
`chef-server.rb` importer
([gap-chef-server-rb-importer](remaining-work.md#gap-chef-server-rb-importer)) and no
`CHEF_SECRETS_DATA` reader at server startup
([gap-secrets-file](remaining-work.md#gap-secrets-file)).
Treat the migration as a translation step: read the old file, keep only the
settings that have a Microwave equivalent, and write them as TOML or
`MW_*` overrides.

A minimal translation looks like this:

```ruby
# legacy /etc/opscode/chef-server.rb
api_fqdn "chef.example.com"
postgresql['max_connections'] = 350
data_collector['root_url'] = "https://automate.example.com/data-collector/v0/"
data_collector['token'] = "secret-token"
```

```toml
# /etc/microwave/microwave.toml
[server]
bind = "0.0.0.0:443"
public_url = "https://chef.example.com"

[database]
driver = "postgres"
url = "postgres://microwave:secret@db/microwave"

[data_collector]
root_url = "https://automate.example.com/data-collector/v0/"
token = "secret-token"
```

Common mappings:

| Legacy `chef-server.rb` setting | Microwave equivalent | Notes |
| --- | --- | --- |
| `api_fqdn` | `server.public_url` for links; reverse-proxy `Host`/`X-Forwarded-*` for request routing | Microwave builds API URLs from the request host and forwarded headers. Set `server.public_url` for password-reset links and any absolute URL that must not depend on the request. |
| `postgresql['max_connections']`, pool tuning | PostgreSQL server-side tuning plus `database.url` | Microwave does not expose a connection-pool size. Tune PostgreSQL itself. |
| `postgresql['work_mem']` | PostgreSQL server setting | Unchanged concept; configure in PostgreSQL. |
| `nginx['ssl_protocols']`, TLS settings | `server.tls.cert_path`, `server.tls.key_path`, and reverse-proxy TLS settings | Microwave terminates TLS directly or behind a proxy. |
| `data_collector['root_url']`, `data_collector['token']` | `data_collector.root_url`, `data_collector.token` | Direct equivalent. |
| `compliance_proxy.url` | `compliance_proxy.url` | Direct equivalent. |
| `search_provider` | `search.backend` (`embedded`, `opensearch`, `elasticsearch`) | Solr is not supported. Use OpenSearch/Elasticsearch for remote search. |
| `search.url` | `search.url` | Direct equivalent. |
| `object_store` S3/Garage settings | `[object_store]` | Direct equivalent for backend, bucket, endpoint, region, credentials, and path style. |
| `storage.compression` | `storage.compression` | `zlib` or `none`. |
| `auth.skew_seconds` | `auth.skew_seconds` | Direct equivalent. |
| `oc_id` settings | `[auth]` superuser and OAuth routes | Microwave uses its own oc-id-compatible identity surface; most oc-id Ruby settings have no equivalent. |
| `rabbitmq[...]`, `opscode_solr4[...]`, analytics settings | none | These services are EOL/removed in Chef Infra Server 14+. Microwave has no RabbitMQ, Solr, or analytics service. |
| `dark_launch[...]` | none | Not used. |
| `telemetry[...]` | `telemetry.*` | Microwave stores telemetry configuration but does not send telemetry. |

Secrets and keys are not imported from `chef-server.rb`. Use `microwave-ctl set-secret` or `MW_*` environment variables, and migrate server-private material with
`microwave-migrate import-knife-backup --secrets-dir ...` or `microwave-ctl backup --include-private-keys`.

For an agent-assisted migration, give the agent the old file plus this table and ask it to produce a Microwave TOML file plus a list of settings that have no equivalent. Review that list manually before starting the server.

### Importing a `knife ec backup` repository

If the legacy server was backed up with `knife ec backup`, import that
directory directly. The importer needs no HTTP server, writes cookbook blobs
through the local object store, and imports metadata in one store transaction:

```sh
# SQLite; cookbook blobs default to <data-dir>/bookshelf
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --data-dir /var/lib/microwave \
  --yes

# PostgreSQL
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --database-url postgres://microwave:secret@db/microwave \
  --bookshelf-dir /var/lib/microwave/bookshelf \
  --yes

# Validate and count first; writes nothing and does not ask for confirmation
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --data-dir /var/lib/microwave \
  --dry-run
```

Server-owned private material is not part of a knife backup. Supply it
explicitly with `--secrets-dir` or `--secrets-archive` when migrating an
existing installation's `pivotal.pem`, `pivotal.password`, web UI keys,
`objectstore.secret`, `private-chef-secrets.json`, TLS certificate/key, and CA
files:

```sh
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --data-dir /var/lib/microwave \
  --secrets-dir /root/chef-server-secrets \
  --yes

# target paths for TLS normally come from --config server.tls.cert_path/key_path
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --database-url postgres://microwave:secret@db/microwave \
  --bookshelf-dir /var/lib/microwave/bookshelf \
  --secrets-target-dir /var/lib/microwave \
  --secrets-archive /root/chef-server-secrets.tar.gz \
  --config /etc/microwave/microwave.toml \
  --yes
```

Private files are written `0600`, public certificates/keys `0644`, and newly
created parent directories `0700`. CA-tree `.key` files and PEM private keys
are private; `nginx/ca/<fqdn>.crt`/`.key` map to the TLS certificate/key
targets, while additional `nginx/ca` pairs are preserved as CA material.
Private detection is extension/PEM-marker based, not DER inference. Existing
targets are refused unless `--force` is supplied. `.tar`/`.tar.gz` archives are
allowlisted and reject absolute paths, `..`, symlinks, hardlinks, devices, and
FIFOs; a single legacy top-level directory wrapper is accepted. File, archive,
expanded-total size, and file-count (4096) limits are enforced while reading.
No file contents are printed. Without a secrets source the importer reads and
writes no private files.

Secret files are staged before any target is replaced and rolled back as a set
on an ordinary commit error. A process crash during commit can still leave a
partial set; keep an operator backup and re-run with `--force` after recovery.
`webui_priv.pem`/`webui_pub.pem` and `private-chef-secrets.json` are preserved
for external tooling; Microwave does not consume the web-UI keypair or merge
the private-chef secret document at runtime.

**Client/user private keys remain client-owned.** Chef Infra Server stores
only public keys for clients and users, so no migration tool can recover their
private keys from the server. Clients keep their existing keys or generate new
ones, then point at the new server URL; update their local client configuration
separately.

The supported layout covers users and user ACLs, optional
`key_dump.json`/`key_table_dump.json`, organizations, members/invitations,
clients, containers, groups, nodes, roles, environments, data bags/items,
per-object ACLs, and ChefFS cookbook directories (`metadata.rb`/`metadata.json`,
`status.json`, MD5-addressed files). See the
[`microwave-migrate` README](../crates/microwave-migrate/README.md#knife-ec-backup-directory-import)
for the exact supported subset and remaining gaps (arbitrary Ruby in
`metadata.rb`, cookbook artifacts/policies/policy groups, OAuth applications,
S3/Garage blobs, purge semantics, and the absence of client/user private keys).
Server-secret import copies explicitly supplied files; it does not validate a
TLS certificate/key pairing or restart a running server.

The command merges/upserts into an existing store, so it asks for an
interactive `yes` or `--yes`. Re-importing the same backup is idempotent.
Objects that were deleted from the backup but still exist in the target are
not removed.

A bootstrapped target normally already has a `pivotal` user with a different
id. The importer fails with a clear error by default; pass `--merge-users` to
reuse the existing id/authz id. Merged users keep non-empty target
credentials, admin/recovery state, profile fields, and an existing default key
unless `--replace-user-credentials` is also passed; a warning reports merged
users with no backup key material. The importer then reconciles the effective
`server-admins` membership to each merged user's final admin flag. With
`--replace-user-credentials`, a merged user for whom the backup supplies no
default key loses the target default key so revocation is complete; that flag
requires `--merge-users`. ACL and group-membership import is
additive by default; pass `--replace-acls` to replace ACLs and membership for
targets/groups present in the backup, which is required to remove revoked
grants. Groups inferred by the importer, such as global `users`, are not
cleared; a failed import restores the previous ACL/group state. Neither flag
makes the whole metadata/secret operation atomic.

### Importing a snapshot

For a snapshot-based workflow, use `from-api` to produce the snapshot and
then import it. The manual path below remains useful when you already have a
snapshot or need to stage the copy yourself:

1. **Produce a snapshot from a Microwave installation** (for example a staging
   instance populated by your migration tooling):
   ```sh
   microwave-migrate export --data-dir /var/lib/microwave-staging \
     --output /var/backups/acme-snapshot.json
   ```
   or, if you already have a snapshot, skip this step.
2. **Stop the target server** (and, for PostgreSQL, any other replica using
   the database). SQLite import takes a write lock; PostgreSQL import runs in
   one transaction.
3. **Import into the configured database:**
   ```sh
   microwave-ctl --config /etc/microwave/microwave.toml \
     migrate-from-legacy --input /var/backups/acme-snapshot.json --yes
   ```
   `microwave-migrate import` is the standalone equivalent for SQLite when
   given the same storage compression (`--config` or `--compression`); it does
   not implicitly read the server's configuration file. For PostgreSQL use
   `microwave-migrate import --database-url postgres://... --input ... --yes`
   with the same compression flags. Both import paths ask for confirmation
   unless `--yes` is given.
4. **Copy cookbook blobs.** For a live legacy server, pass
   `--bookshelf-dir /var/lib/microwave/bookshelf` to `from-api` so the crawl
   copies and verifies local blobs. If the source is a local-bookshelf
   Microwave installation, use `microwave-migrate export --archive` /
   `import --archive` to carry `snapshot.json` plus `bookshelf/` in one
   validated archive. For a `knife ec backup` source, `import-knife-backup`
   copies cookbook blobs into the local object store itself. For other
   snapshot sources, place files under
   the object key layout `organization-<org_id>/checksum-<md5>` in
   `/var/lib/microwave/bookshelf/` for the local backend, or upload them to the
   configured S3 bucket under the same keys. The legacy Chef Server bookshelf
   layout is not converted automatically by the snapshot tools; for an external
   S3/Garage bucket, replicate the bucket and keep the object keys
   `organization-<org_id>/checksum-<md5>`.
5. **Verify** with `microwave-ctl status`, `user-list`, `org-list`,
   `org-show <org>`, and a `knife` or `chef-client` read against a test org.
6. **Start the server** and drain clients over gradually.

What the import preserves: users with their password hash format (legacy
bcrypt/SHA1 variants are verified and upgraded on login), client and user
keys, ACLs and group graph, containers, nodes, roles, environments, data bags,
cookbook metadata and checksum references, policyfiles/policy groups, and
OAuth applications. Chef clients keep their existing keys and URLs, so no
client reconfiguration is needed if the endpoint stays the same.

For a legacy Chef Server source whose bookshelf layout is not the Microwave
`organization-<org_id>/checksum-<md5>` tree, keep the legacy server
authoritative, move one organization at a time, and keep the legacy server
read-only until the new one is verified. For large or S3/Garage-backed
deployments, the same parallel-run approach applies.

## 11. Health, status, and logs

### Endpoints

| Endpoint | Auth | Purpose |
|---|---|---|
| `GET /_status` | none | `{"status":"pong","upstreams":{"chef_sql":"pong","chef_index":"pong","bookshelf":"pong"},"upstream_details":{...},"keygen":{"status":"ok","cached":3,"capacity":3,"workers":1,"worker_healthy":true,"worker_failures":0,"warm_up_failures":0,"generated_on_demand":0},"indexing":{"mode":"embedded"}}`. |
| `GET /_stats` | none | Request counters, authentication failures, uptime, maintenance state, and node count. JSON by default, Prometheus text with `Accept: text/plain` or `?format=prometheus`. |
| `GET /server_api_version` | none | `min_api_version` / `max_api_version` (`0`/`2`). |
| `GET /license` | signed | Node license counters: configured limit, current node count, `limit_exceeded`, and the upgrade URL. Telemetry is disabled by default. |
| `POST /organizations/:org/data-collector` | signed or `x-data-collector-token` | Present when `data_collector.root_url` is configured or `data_collector.store` is `local`/`both`. When `store` is `forward`/`both` and `root_url` is configured, forwards the JSON body and preserves the collector's status and body. `local` returns `200 {}` without forwarding while still storing `run_converge` reports; `both` also stores them. Timeouts map to `504`, other upstream failures to `502`; a disabled endpoint returns `404`. |
| `GET /organizations/:org/owners/:owner/compliance[/profile]` | signed | Present only when `compliance_proxy.url` is configured. Rewrites to `/compliance/profiles/:owner[/profile]` and preserves the upstream status and body. Timeouts map to `504`, other upstream failures to `502`. |
| `GET /compliance/organizations/:org/owners/:owner/compliance[/profile]` | signed | Legacy chef-gate spelling of the same compliance proxy route. |
| `GET /organizations/:org/required_recipe` | signed | Present only when `required_recipe.enabled` is true. Returns the configured recipe file as `text/plain`; `POST` returns `405`, and the disabled endpoint returns `404` for every method. |

### Data Collector, required recipe, and telemetry

Data Collector forwarding is disabled until `data_collector.root_url` is set.
The optional `data_collector.token` is accepted from clients as
`x-data-collector-token` (or resolved from the `data_collector.token` secret)
and is always replaced with the server-side value when forwarding upstream.
Clients without a token may authenticate with a normal Chef signature instead.
The forwarder uses a shared connection pool, applies the configured timeout to
both the request and response body, caps upstream responses at 1 MiB, and does
not hold database locks across the outbound request.

`data_collector.store` can be `forward` (default), `local`, or `both`.
`local`/`both` persist Chef `run_converge` reports in the metadata database,
one row per `(organization, node, run)` with 50 runs kept per node by default.
The metadata database grows with retained reports (bounded by `keep_runs` per
node); size `keep_runs` accordingly.
Reports are not included in logical snapshots or `microwave-migrate`.
Set `keep_runs = 0` to keep every report; only `run_converge` payloads are
stored (`run_start` and `action` payloads are not).

While `data_collector.root_url` is set, `data_collector.actions_enabled` is
explicitly true (the default is false), and `data_collector.store` allows
forwarding (`forward` or `both`; `local` disables actions), successful node,
role, environment, data bag/item, client, key, policy, membership, ACL,
cookbook, and organization mutations also produce an `oc_chef_action` message
(`message_type: action`, `message_version: 0.1.1`) to the same collector URL.
The payload follows
upstream's entity/task fields, uses the server-side token, and recursively
redacts `password`, `private_key`, `secret`, and `token` values. Payload
construction and the token-secret lookup happen in the worker, not on the
request path. Notifications are off the request path: a single background
worker drains a bounded 256-entry queue with the configured timeout. A full
queue, timeout, refused connection, or non-2xx collector response is logged and
dropped without changing the API response. Failed (4xx/5xx) mutations never
emit notifications, and no collector connection is made while `root_url` is
unset. Request bodies larger than 256 KiB are not retained at all; smaller
bodies are parsed and scrubbed before they enter the queue, and queued jobs
share an 8 MiB retained-data budget. Delete notifications omit `data` because
the request has no body.

The compliance proxy is independent of client payload forwarding. Setting
`compliance_proxy.url` enables the Automate profile routes, rewrites
`owners/:owner/compliance[/profile]` to
`/compliance/profiles/:owner[/profile]`, sends
`x-data-collector-auth: version=1.0` and the configured service token, and
relays the upstream status, body, and content type. It never forwards
client-supplied Chef authorization headers. Owner and profile segments are
rejected if they contain `.`/`..`, slashes, or control characters; the final
URL is parsed and must stay under the configured base path's
`/compliance/profiles/` prefix before any request is sent. One trailing slash
after a profile is normalized away (a lone `/` becomes the owner-level path);
empty internal segments (double slashes) are rejected. Responses are capped
at 1 MiB and use the configured timeout; upstream failures map to `502` and
timeouts to `504`. `insecure_skip_tls_verify` exists for test or private-CA
setups and should stay `false` in production. The legacy
`/compliance/organizations/:org/...` spelling resolves the organization for
authentication, so client principals are checked against the same org as the
standard route.

The optional `required_recipe` endpoint is off by default. Enabling it requires
`required_recipe.path`; the file is read on each request and served as plain
text. The file must be a regular file no larger than 1 MiB, must not be a
symlink, must not be group- or world-writable, and must be owned by root or by
the server's effective user. The feature is independent of telemetry.

Telemetry is off by default and there is no outbound sender. The `telemetry`
table exists in both SQLite and PostgreSQL for schema compatibility, and the
`telemetry.*` configuration keys are parsed, but Microwave does not contact an
external telemetry service. This is intentional: no phone-home.

API key creation (`create_key`/`private_key` paths and organization validator
provisioning) takes a pre-generated 2048-bit RSA keypair from a bounded,
thread-safe in-memory cache. `keygen.size` controls how many keypairs are kept
ready (default `3`), `keygen.workers` controls the background generators
(default `1`, capped at `size`), and `keygen.warm_up` optionally fills the cache
synchronously at startup. With the default `warm_up = false`, startup does not
wait for RSA generation. A cache miss generates a keypair on Tokio's blocking
pool, so the async runtime is not stalled. Private keys are only held in memory
until handed to the API response and are never logged.

`/_status` reports keygen health and probes the metadata database, search
backend, and object store. The `upstreams` object keeps upstream's
`{"service": "pong"|"fail"}` shape with `chef_sql`, `chef_index` (or
`chef_opensearch`/`chef_elasticsearch`), and `bookshelf`; `upstream_details`
adds the probe status and detail. A failing dependency sets the top-level
`status` to `fail` (and the matching upstream to `fail`) but the endpoint still
returns HTTP 200, so a reverse proxy or orchestrator can inspect the payload.

Each probe is bounded to 1.5 seconds. SQLite's health query runs on Tokio's
blocking pool using an owned connection handle. It retries `try_lock` for up to
200ms so a normal request briefly holding the metadata connection does not make
the database look unhealthy; if the lock is still held after that budget the
probe reports `busy` instead of queuing blocking tasks indefinitely. PostgreSQL
uses an async `SELECT 1` against the pool. OpenSearch and Elasticsearch call
`/_cluster/health`; only an explicit `green` or `yellow` status is healthy. A
red cluster, an unparseable 200 response, a missing status, or an unknown status
is reported as a failure (`yellow` is valid for a single node). Local object
storage verifies read/write access with a unique probe file per call; orphan
probe files older than one hour are swept at most once per hour, so a crash does
not grow the directory and health checks do not rescan it on every request. S3
and Garage perform a signed, bounded `HEAD` request on the bucket. A custom
object store that blocks past the deadline leaves one detached blocking task
until it returns; the built-in local and S3 stores bound their own I/O.

The completed snapshot is cached for one second. Concurrent cold requests share
a single in-flight probe. An expired snapshot is served immediately while
exactly one background refresh task runs (stale-while-refresh); additional
stale requests reuse the same snapshot without spawning more tasks. A snapshot
is never served stale beyond five seconds: after that a request waits for a
real probe instead of reporting arbitrarily old health data. These guarantees
are process-local; each replica probes its own dependencies.

The S3/Garage credentials must allow `s3:HeadBucket` (and therefore
`s3:ListBucket`) on the configured bucket; object read/write permissions alone
make the `bookshelf` probe fail with `403`. The `keygen` object reports the
cached keypair count, capacity, worker count, and worker health (`status`
becomes `degraded` after a background or warm-up generation failure and returns
to `ok` after the next successful background generation). `worker_failures` and
`warm_up_failures` are cumulative counters.

```sh
curl -fsS http://127.0.0.1:8000/_status
curl -fsS http://opensearch:9200/_cluster/health?wait_for_status=yellow
microwave-ctl --config /etc/microwave/microwave.toml check-config
```

When the [TLS listener](#tls-listener) is enabled, probe `https://` on the
configured bind address instead. For a self-signed certificate add `-k` to the
manual check (the container healthcheck uses `MW_HEALTHCHECK_INSECURE` for the
same purpose).

`/_stats` exposes process-local counters for requests, response classes,
authentication failures, and uptime, plus the maintenance flag and node count.
It does not require authentication and remains reachable during maintenance:

```sh
curl -fsS http://127.0.0.1:8000/_stats
curl -fsS -H 'Accept: text/plain' http://127.0.0.1:8000/_stats
curl -fsS 'http://127.0.0.1:8000/_stats?format=prometheus'
```

### CLI

```sh
microwave-ctl --config /etc/microwave/microwave.toml status
microwave-ctl --config /etc/microwave/microwave.toml check-config
microwave-ctl --config /etc/microwave/microwave.toml reconfigure
microwave-ctl --config /etc/microwave/microwave.toml reindex --org acme
microwave-ctl --config /etc/microwave/microwave.toml ha-status
microwave-ctl --config /etc/microwave/microwave.toml psql 'select count(*) from nodes;'
microwave-ctl --config /etc/microwave/microwave.toml pedant users nodes
microwave-ctl --config /etc/microwave/microwave.toml user-edit alice
microwave-ctl --config /etc/microwave/microwave.toml oc-id-show-app supermarket
microwave-ctl --config /etc/microwave/microwave.toml filtered-dump --output /tmp/dump --org acme
microwave-ctl --config /etc/microwave/microwave.toml gather-logs --output /tmp/microwave-logs.tar.gz
microwave-ctl --config /etc/microwave/microwave.toml cleanup-bifrost --estimate-only
microwave-ctl --config /etc/microwave/microwave.toml rotate-credentials bookshelf
microwave-ctl --config /etc/microwave/microwave.toml rotate-shared-secrets
microwave-ctl service status
```

`status` reports the data directory, driver, database path, whether the
database is readable, and whether the superuser exists. For PostgreSQL it
connects without changing the schema and reports `database_readable: true`
when the schema already exists and the superuser query succeeds; an empty or
unreachable database reports `false` without failing the command. `reconfigure`
validates and migrates the backend, then reports the resolved backends without
restarting a server. `ha-status` always reports a single standalone node.
`psql` execs `sqlite3` for a SQLite database (install `sqlite3` on the host;
the container image includes it), or `psql` for a configured PostgreSQL URL,
and passes trailing arguments through. `reindex` validates the embedded
relational counts or backfills a remote search cluster; see
[Search](#search). `pedant` runs `scripts/run-pedant.sh` with the supplied tags
(tags are passed as separate arguments, output is streamed, and the harness exit
status is preserved; set `MW_PEDANT_SCRIPT` when the script is not in the
current tree or beside the binary, and discovery stops at the repository root).
`user-edit` writes the editable user JSON to a `0600` temporary file, opens
`$VISUAL`/`$EDITOR`, hashes a non-empty `password` field, supports `null` to
clear profile fields, and refuses to remove the configured superuser's admin
flag; `--input FILE` makes the edit non-interactive. `oc-id-show-app` prints
the named OAuth application's `uid`, `secret`, and `redirect_uri` (this is an
intentional secret-revealing command). `filtered-dump` mirrors upstream's
debug dump: organization/client metadata is preserved, but node, role, and
environment attribute maps and data-bag values are replaced with placeholders;
`--org` limits the output. `gather-logs` builds a `0600` `.tar.gz` with a
redacted config summary, system/status reports, the last journald lines, and a
data-directory name/size listing; database contents, bookshelf blobs, keys,
and environment variables are excluded. `cleanup-bifrost` counts or deletes
`authz_entities` rows no identity or object table references
(`--estimate-only` avoids changes, `--wait-time` delays the start, and
`--force-cleanup` reports that Microwave has no legacy tracking tables).
Credential rotation updates the `kvpair`-backed service secrets and never
prints generated values; `rotate-shared-secrets` also resets the shared
credential version and requires redistributing the secrets and restarting or
reconfiguring the server. `service` controls the systemd unit; in a container
or on a host without systemd, `service start --foreground` (or
`MW_SERVICE_FOREGROUND=1`) runs `microwave-server serve` in the foreground
instead. `backup`/`restore` use
`VACUUM INTO`
for SQLite and `pg_dump`/`pg_restore` for PostgreSQL, in directory or tar
format. `microwave-migrate` exports and imports both backends; see
[Backup and restore](#9-backup-and-restore).

### Logs

The server logs to stderr via `tracing`, filtered by `RUST_LOG` (`info` by
default). The startup line reports the configured bind address, the resolved
local address, and `scheme=http` or `scheme=https`. Examples:

```sh
RUST_LOG=info,microwave_api=debug microwave-server serve ...
journalctl -u microwave-server -f
docker compose logs -f microwave
```

There is no file logging or in-process rotation; use journald or the container
log driver. `SIGTERM` and `SIGINT` trigger graceful shutdown: the listener
stops accepting new connections and in-flight requests are allowed to finish.
The TLS listener caps the drain at 5 seconds; plain HTTP waits for active
requests to complete. `SIGTERM`/`SIGINT` exit with status 0 when the drain
finishes, so `docker stop` does not need to escalate to `SIGKILL`. Process
teardown can also wait for one in-progress background keypair generation to
finish (the keygen workers are joined, which is CPU-bounded). SQLite WAL is
crash-safe, but still drain traffic at the load balancer before stopping or
restarting. `SIGHUP` remains the TLS reload signal and does not shut down the
server.

## 12. Maintenance caveats

- **Maintenance mode rejects API traffic.** `microwave-ctl maintenance on`
  persists `maintenance:enabled` in the `kvpair` table. While it is set, the
  HTTP middleware returns `503 Service Unavailable` with a JSON error before
  authentication runs, so unsigned requests cannot mutate the store either.
  `/_status` and `/_stats` remain reachable for health checks and monitoring.
  Turning the flag off takes effect on the next request; no restart is needed.
- **Reindex semantics.** `microwave-ctl reindex` backfills a remote
  OpenSearch/Elasticsearch cluster from the relational store and reports
  per-type acknowledged counts; it fails loudly on the first transport error or
  failed bulk item, including when `_bulk` returns HTTP 200 with `errors:true`.
  By default it does not delete documents for objects removed while the remote
  backend was inactive. Pass `--purge` to delete each visited organization's
  remote documents with `_delete_by_query` before rebuilding; the command
  refreshes the index when it finishes. Purging is destructive, so an
  interrupted run leaves that organization unsearchable until it is re-run.
  With the embedded backend it only prints relational counts, because the store
  is the index.
- **PostgreSQL client tools.** `microwave-ctl backup`/`restore` shell out to
  `pg_dump`/`pg_restore`; install the matching major version on the host or run
  them from a maintenance container. `psql` likewise needs the client tool.
  A PostgreSQL restore is destructive and must be coordinated across replicas.
- **Single writer.** SQLite is opened in WAL mode with a 5 s busy timeout. A
  heavy `microwave-ctl` write (import, key rotation, user creation) can
  contend with request writes; run bulk operations in a window.
- **Backup consistency.** The database snapshot is consistent; the bookshelf
  copy is not atomic with it. S3 blobs are not in the backup. The tar format
  wraps the same payload and does not add atomicity.
- **Service management is systemd-based or explicit foreground.** `microwave-ctl
  service` delegates to `systemctl`/`journalctl` and fails clearly when they are
  missing. `service start --foreground` (also `MW_SERVICE_FOREGROUND=1`) runs
  `microwave-server serve` in the foreground for containers; on Unix the CLI
  `exec`s the server so PID 1/signal handling is direct. It does not reproduce
  the systemd unit's user, filesystem, capability, or syscall hardening; in a
  container that isolation comes from the image and runtime. There is no
  detached process supervisor for Docker daemons or launchd.
- **Local object store URLs** are derived from `server.bind` and the listener
  scheme (`http`/`https`), not from request headers. Use S3/Garage behind a
  proxy or NAT.
- **Set `RUST_LOG` deliberately.** `debug` includes request-level detail and
  can be verbose during converge storms.
- **Clock skew.** Widening `auth.skew_seconds` weakens replay protection;
  fix NTP first.

## 13. Upgrades and rollback

1. Take a backup (`microwave-ctl backup --include-private-keys`) or copy
   `pivotal.pem` with your own key backup procedure.
2. Drain the server at the load balancer.
3. Stop the service.
4. Replace `/usr/local/bin/microwave-*`, `chef-server-ctl`, and the unit file if
   it changed; run `systemctl daemon-reload`.
5. Start the service. Schema migrations run automatically and idempotently
   when the database is opened.
6. Verify `/_status`, `microwave-ctl status`, `check-config`, and a signed API
   request.
7. Return traffic.

Migrations are forward-only: there is no down-migration path. Keep the
pre-upgrade backup until the new version has been verified. Downgrading
requires restoring that backup.

## 14. Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| `failed to read config file` on start | `MW_CONFIG`/`--config` points at a missing file. `check-config` reports the same. |
| `failed to read TLS certificate file` / `do not form a matching pair` | Fix `server.tls.cert_path`/`key_path`, make the files readable by the service user, and ensure the key belongs to the leaf certificate. `check-config` performs the same check without binding. |
| `TLS certificate reload failed; keeping the previous certificate` | The replacement pair is unreadable, malformed, or mismatched. The server is still serving the previous certificate. Fix both files (write them atomically) and send `SIGHUP` again or wait for the next polling interval. |
| `curl: (60) SSL certificate problem` against a direct HTTPS listener | The client does not trust the issuing CA, or the certificate does not cover the hostname used. Install the internal CA on clients; reserve `-k`/`MW_HEALTHCHECK_INSECURE` for probes and tests. |
| `server.public_url` rejected or missing while mail is enabled | Set an absolute `https` origin (or `http` only for a loopback host) with no credentials, path, query, or fragment. It is required whenever `mail.enabled` is true. |
| Password-reset email is not delivered | Confirm `mail.enabled`, `mail.host`, `mail.from`, and `server.public_url`, and that the user has an email address. Delivery is asynchronous; relay and queue failures are logged (`Failed to send email`, `password reset mail failed`) while the HTTP response stays generic. |
| `mail.security = "none"` rejected with credentials | Plaintext SMTP AUTH is loopback-only. Use STARTTLS or implicit TLS for a remote relay. |
| `mail.password` missing after restore | Expected: the password is never serialized into `microwave-config.json`. Set `MW_MAIL__PASSWORD` for the restored server. |
| `Permission denied` creating `microwave.db` | Data dir not writable by the service user. `chown -R microwave:microwave /var/lib/microwave`. |
| `Address already in use` | Another process holds `server.bind`. Change the port or stop the other process. |
| `check-config` complains about object store | `s3`/`garage` needs endpoint, bucket, and credentials (config or `bookshelf.*` secrets). |
| Sandbox completion returns `503` | The object store reports the uploaded checksum as missing. Check `endpoint` reachability from the server and bucket permissions. |
| Cookbook download URLs point at `0.0.0.0` or an internal name | Local object store uses `server.bind`; switch to S3/Garage or bind a client-reachable address. |
| `401` from `chef-client` after migration | Client/server clock skew beyond `auth.skew_seconds`, or the client key was not imported. Check `microwave-ctl list-user-keys` / `list-client-keys`. |
| Search returns no rows after switching backends | Run `microwave-ctl reindex` to backfill the remote cluster; check `search.url`, `search.timeout_ms`, and the cluster health. Add `--purge` only when stale documents for already-deleted objects must be removed. |
| `maintenance on` still serves requests | Check `microwave-ctl maintenance status` and that all replicas share the same database; `/_status` and `/_stats` are intentionally exempt. |
| `psql` fails with `failed to run sqlite3` | Install `sqlite3` on the host (present in the container image). |
| Server takes seconds to stop or `docker stop` waits | Active requests are draining after `SIGTERM`/`SIGINT`. The TLS listener caps the drain at 5 seconds; plain HTTP waits for active requests. Check for a hung handler rather than disabling graceful shutdown. |
