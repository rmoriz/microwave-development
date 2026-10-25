# Deploying Microwave Server

This guide covers packaging and operations for the Rust server in this
repository: a single binary with embedded storage, Docker and systemd
packaging, external OpenSearch/S3 backends, backup/restore, migration, and the
operational caveats that matter in practice.

For the architecture and the compatibility target see
[`docs/superpowers/specs/2026-10-24-microwave-server-design.md`](superpowers/specs/2026-10-24-microwave-server-design.md).
For the current implementation gaps see "Not Yet Implemented" in
[`README.md`](../README.md).

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
| `microwave-migrate` | Versioned JSON snapshot export/import. |

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
| `object_store.backend` | `MW_OBJECT_STORE__BACKEND` | `local` | `local`, `s3`, `garage`. |
| `object_store.bucket` | `MW_OBJECT_STORE__BUCKET` | unset | Required for `s3`/`garage`. |
| `object_store.endpoint` | `MW_OBJECT_STORE__ENDPOINT` | unset | Required for `s3`/`garage`. See the reachability caveat below. |
| `object_store.region` | `MW_OBJECT_STORE__REGION` | `us-east-1` | Must match the S3 gateway region. |
| `object_store.access_key` | `MW_OBJECT_STORE__ACCESS_KEY` | unset | Falls back to the `bookshelf.access_key_id` secret. |
| `object_store.secret_key` | `MW_OBJECT_STORE__SECRET_KEY` | unset | Falls back to the `bookshelf.secret_access_key` secret. |
| `object_store.path_style` | `MW_OBJECT_STORE__PATH_STYLE` | `true` | Keep `true` for Garage/MinIO. |
| `object_store.secret` | `MW_OBJECT_STORE__SECRET` | generated | Only used by the local `/bookshelf` backend. |
| `auth.skew_seconds` | `MW_AUTH__SKEW_SECONDS` | `900` | `X-Ops-Timestamp` tolerance. |
| `auth.superuser_name` | `MW_AUTH__SUPERUSER_NAME` | `pivotal` | Account that cannot be demoted. |
| `keygen.size` | `MW_KEYGEN__SIZE` | `3` | Maximum number of pre-generated 2048-bit RSA keypairs kept in memory. `0` disables caching and generates on demand. Maximum `1024`. |
| `keygen.workers` | `MW_KEYGEN__WORKERS` | `1` | Background key-generation threads. Must not exceed `keygen.size`; maximum `8`. `0` leaves refilling to `warm_up`. |
| `keygen.warm_up` | `MW_KEYGEN__WARM_UP` | `false` | Fill the cache synchronously during startup. Requires `keygen.size > 0`. Leave `false` to keep startup fast and let workers refill in the background. |
| `data_collector.root_url` | `MW_DATA_COLLECTOR__ROOT_URL` | unset | Enables `POST /organizations/:org/data-collector` and receives the forwarded payload, e.g. `https://automate.example.com/data-collector/v0/`. |
| `data_collector.token` | `MW_DATA_COLLECTOR__TOKEN` | unset | Shared bearer token accepted inbound and sent upstream as `x-data-collector-token`. Falls back to the `data_collector.token` secret. |
| `data_collector.timeout` | `MW_DATA_COLLECTOR__TIMEOUT` (alias `MW_DATA_COLLECTOR__TIMEOUT_MS`) | `30000` | Upstream forwarding and response-read timeout in milliseconds (TOML alias `timeout_ms`). Must be greater than zero; `check-config` rejects `0`. |
| `data_collector.actions_enabled` | `MW_DATA_COLLECTOR__ACTIONS_ENABLED` | `false` | Opt in to `oc_chef_action` mutation notifications while `root_url` is configured. `root_url` alone only enables client payload forwarding. Disabled automatically when `root_url` is unset. |
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
configuration; the standalone `microwave-migrate import` accepts
`--compression zlib|none` or `--config <path>` (with
`MW_STORAGE__COMPRESSION` as an environment override).

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
lives only in `pivotal.pem`. Back it up.

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
`rust:<version>-bookworm`, then copies only the four binaries into
`debian:bookworm-slim` with `ca-certificates`, `curl`, and `sqlite3`.

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
  load-validated. On every server start, `PostgresStore::connect` applies the
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
  backup /var/backups/microwave/2026-10-24
```

With the default directory format, the destination must be a directory
(created if missing). For SQLite it must not already contain `microwave.db`;
for PostgreSQL it must not already contain `microwave.dump`. The command
writes:

- `microwave.db` for SQLite — a `VACUUM INTO` snapshot, consistent including
  committed WAL contents, safe to take while the server is running;
- `microwave.dump` for PostgreSQL — a custom-format `pg_dump` archive, taken
  from a consistent database snapshot, safe while the server is running;
- `bookshelf/` — a recursive copy of the local blob store, if present. The
  copy refuses symlinks and other non-regular files (FIFOs, sockets, devices)
  instead of dereferencing or blocking on them; the tar builder also runs with
  `follow_symlinks(false)`.

For a single-file archive with the same payload, use the tar format:

```sh
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml \
  backup /var/backups/microwave/2026-10-24.tar --format tar
# `--tar` is equivalent shorthand
```

The archive is written atomically to the requested file and refuses to replace
an existing file. It is uncompressed, so it is large but inspectable with
`tar tf`; compress it with your backup tooling if needed. It contains exactly
the same database snapshot/dump and `bookshelf/` tree as the directory format.
Symlinks and other non-regular files under `bookshelf/` abort the backup rather
than leaking link targets or blocking on a FIFO. On Unix, file copies also open
with `O_NOFOLLOW`/`O_NONBLOCK`; see the restore notes below for the residual
directory-level TOCTOU caveat.

PostgreSQL backup requires `pg_dump` to be installed and on `PATH`. The
password is removed from the child process command line. A `password` query
parameter or authority password is supplied through `PGPASSWORD`; an
`sslpassword` is written to a temporary libpq service file with mode `0600`
and referenced through `PGSERVICEFILE`/`PGSERVICE`. Depending on the platform
and hardening, a child's environment or temporary files may still be
observable to the same UID or root; treat backups and the host account
accordingly. Other connection parameters (host, port, user, database,
`sslmode`) are preserved.

It does **not** include:

- `pivotal.pem`, `pivotal.password`, or `objectstore.secret` files (the object
  store secret value is also in the database, the superuser private key is
  not);
- S3/Garage objects when an external object store is configured;
- the OpenSearch index (it is derived data);
- `microwave.toml` or systemd drop-ins.

Back those up separately:

```sh
sudo tar -C /var/lib/microwave -czf /var/backups/microwave/keys-2026-10-24.tgz \
  pivotal.pem pivotal.password
```

The bookshelf copy is not atomic with the database snapshot. Uploads that are
in flight during the copy can be missed. For a strict backup, stop or drain
traffic at the load balancer first, or use S3 versioning/replication for blobs.

### Restore

Stop the server first. `restore` replaces the live database and bookshelf
directory in place.

```sh
sudo systemctl stop microwave-server
sudo -u microwave microwave-ctl --config /etc/microwave/microwave.toml \
  restore /var/backups/microwave/2026-10-24
sudo systemctl start microwave-server
```

`restore` prints the target (without credentials) and asks for confirmation.
In scripts and other non-interactive environments, pass `--yes`. A directory
and a tar archive are accepted; the format is detected from the path, or can be
forced with `--format directory|tar` (`--tar` is shorthand). Tar entries are
validated before extraction: only `microwave.db`, `microwave.dump`, and
`bookshelf/` are allowed, and absolute paths, `..`, symlinks, and special files
are rejected. Extraction happens in a temporary directory, so refusing
confirmation never touches live data. For PostgreSQL it reads the archive TOC
first and reports the database the dump was created from; a mismatch with the
configured target is refused unless `--yes` is given.

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
server (and any other replica using the database) before running it. It does
not restore `pivotal.pem`/`pivotal.password`, and it does not restore S3
objects. Restore those from your separate copies.

### Metadata snapshot (`microwave-migrate`)

```sh
microwave-migrate export --data-dir /var/lib/microwave --output /var/backups/snapshot.json
microwave-migrate import --data-dir /var/lib/microwave-new --input /var/backups/snapshot.json --yes

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
```

`import` merges into live metadata and asks for confirmation. Use `--yes` in
scripts and other non-interactive environments. The target is printed without
authority or query-string passwords.

The snapshot is versioned JSON (`format: microwave-server-snapshot`,
`version: 1`) containing users (including password hashes), organizations,
clients, actor keys, ACLs, groups, containers, nodes, roles, environments, data
bags, checksums, cookbooks, cookbook artifacts, policies, policy groups, and
OAuth applications. Import applies in one transaction on either backend and
preserves ids, authz ids, timestamps, and keys. PostgreSQL imports advance the
sequence-backed cookbook, artifact, and OAuth identifiers so later inserts do
not collide.

Import into a fresh database when possible. The same unique-name rule applies
on both backends: importing a `server-admins` or other global group with a
different id into a database that already has that name fails rather than
rewriting live ACL references.

Not included: cookbook blobs, `kvpair` secrets (including the maintenance
flag), empty sandboxes, and the superuser key files. Copy `bookshelf/` (or the
S3 bucket) in addition. The snapshot file is written with mode `0600`; it still
contains password hashes and public keys, so treat it as sensitive. Use
`microwave-ctl backup` instead when you need the full database including
`kvpair`.

## 10. Migration from a legacy server

**Current state:** the live crawler is implemented. `microwave-migrate
from-api` reads a legacy Chef Infra Server through its read-only HTTP API and
writes the same versioned snapshot; see the
[`microwave-migrate` README](../crates/microwave-migrate/README.md) for the
crawled collections and the API limitations (notably password hashes, which
the legacy API never returns). `microwave-ctl migrate-from-legacy --input
<snapshot>` imports that snapshot into the configured SQLite or PostgreSQL
database.

For a full export/import workflow, use `from-api` to produce the snapshot and
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
4. **Copy cookbook blobs.** The snapshot is metadata only. Place files under
   the object key layout `organization-<org_id>/checksum-<md5>` in
   `/var/lib/microwave/bookshelf/` for the local backend, or upload them to the
   configured S3 bucket under the same keys. The legacy bookshelf directory
   layout is not converted by any tool here; this step is manual today.
5. **Verify** with `microwave-ctl status`, `user-list`, `org-list`,
   `org-show <org>`, and a `knife` or `chef-client` read against a test org.
6. **Start the server** and drain clients over gradually.

What the import preserves: users with their password hash format (legacy
bcrypt/SHA1 variants are verified and upgraded on login), client and user
keys, ACLs and group graph, containers, nodes, roles, environments, data bags,
cookbook metadata and checksum references, policyfiles/policy groups, and
OAuth applications. Chef clients keep their existing keys and URLs, so no
client reconfiguration is needed if the endpoint stays the same.

Until a blob importer exists, the practical migration is a parallel run: keep
the legacy server authoritative, move one organization at a
time, and keep the legacy server read-only until the new one is verified.

## 11. Health, status, and logs

### Endpoints

| Endpoint | Auth | Purpose |
|---|---|---|
| `GET /_status` | none | `{"status":"pong","upstreams":{"chef_sql":"pong","chef_index":"pong","bookshelf":"pong"},"upstream_details":{...},"keygen":{"status":"ok","cached":3,"capacity":3,"workers":1,"worker_healthy":true,"worker_failures":0,"warm_up_failures":0,"generated_on_demand":0},"indexing":{"mode":"embedded"}}`. |
| `GET /_stats` | none | Request counters, authentication failures, uptime, maintenance state, and node count. JSON by default, Prometheus text with `Accept: text/plain` or `?format=prometheus`. |
| `GET /server_api_version` | none | `min_api_version` / `max_api_version` (`0`/`2`). |
| `GET /license` | signed | Static license payload; telemetry is disabled. |
| `POST /organizations/:org/data-collector` | signed or `x-data-collector-token` | Present only when `data_collector.root_url` is configured. Forwards the JSON body to the configured collector and preserves its status and body. Timeouts map to `504`, other upstream failures to `502`; a disabled endpoint returns `404`. |
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

While `data_collector.root_url` is set and `data_collector.actions_enabled` is
explicitly true (the default is false), successful node, role, environment,
data bag/item, client, key, policy, membership, ACL, cookbook, and organization
mutations also produce an `oc_chef_action` message (`message_type: action`,
`message_version: 0.1.1`) to the same collector URL. The payload follows
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
`service` controls the systemd unit; in a container or on a host without
systemd, `service start --foreground` (or `MW_SERVICE_FOREGROUND=1`) runs
`microwave-server serve` in the foreground instead. `backup`/`restore` use
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

1. Take a backup (`microwave-ctl backup`) and copy `pivotal.pem`.
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
