# docker-e2e — Microwave Server provisioning test

A self-contained Compose stack that exercises the parts of a Chef Server a
node actually needs:

1. **microwave** — `microwave-server` built from the repository root
   `Dockerfile`, with SQLite, embedded search, and the local `/bookshelf`
   store.
2. **seed** — knife + Berkshelf in a container. It creates the organization,
   downloads the newest `nginx` and `postgresql` cookbooks from Chef Supermarket,
   uploads them plus the local wrapper cookbooks to microwave-server, and creates
   a role whose `run_list` points at nginx and PostgreSQL 18.
3. **node-debian**, **node-ubuntu**, **node-fedora**, **node-amazon** — four
   distro containers with [Cinc Client](https://cinc.sh/) installed. Each one
   boots systemd as PID 1, registers with the validator, converges with
   `role[e2e_role]`, and stays running so the result can be inspected with
   `docker exec`.
4. **workstation** — Cinc Workstation and OpenCode, for driving the stack by
   hand. Not published; reach it with `docker compose exec` or `./shell`.
5. **cinc-console** — the official web UI, logged in with the seeded demo user.
   Published on `127.0.0.1:18081` only.

Nothing runs on the host except `docker compose`.

## Usage

```sh
cd docker-e2e
docker compose up -d --build
```

The workstation image is built locally, so a first start or a change to
`workstation/**` needs `--build`.

Follow the seed and the node output:

```sh
docker compose logs -f seed
docker compose logs -f node-debian node-ubuntu node-fedora node-amazon
```

The node services get a TTY, so systemd's console output (including the Cinc
run) appears in `docker compose logs`. The full run is also in the journal
inside each node.

`seed` must finish before the nodes start. Compose enforces that with
`condition: service_completed_successfully`.

The stack is safe to start twice: the seed reuses an existing organization and
deletes the node/client fixtures left behind by a previous run before the nodes
bootstrap (see "Repeated runs" below).

Tear everything down, including the server data and the generated client
keys:

```sh
docker compose down -v
```

## What the seed does

`seed/seed.sh`:

1. waits for `http://microwave:8000/_status` and the `pivotal.pem` the server
   creates on first start;
2. creates the organization with `knife org create e2e` — or reuses it if it
   already exists — and keeps the generated `e2e-validator` key on the shared
   volume;
3. removes and recreates the demo console user, adds it to the organization and
   `admins`;
4. deletes the `node-debian`, `node-ubuntu`, `node-fedora`, and `node-amazon`
   node and client objects with the `pivotal` superuser key so repeated runs
   can register again, and writes a new `run-id` generation marker;
5. writes a `Berksfile` pinning the two Supermarket cookbooks and runs
   `berks vendor`;
6. uploads every resolved cookbook with `knife cookbook upload --all`, and
   uploads the local `e2e_nginx`, `e2e_postgresql`, `e2e_timer`, and
   `e2e_marker` wrapper cookbooks;
7. writes `role.rb` and runs `knife role from file`; besides nginx and
   PostgreSQL, the role installs the periodic `cinc-client.timer` and the
   `/etc/e2e-marker` file used to verify timer runs:

   ```ruby
   name "e2e_role"
   run_list "recipe[e2e_nginx]", "recipe[e2e_postgresql]", "recipe[e2e_timer]", "recipe[e2e_marker]"
   ```

8. writes `/shared/client.rb` (`chef_server_url`, validator key) and
   `/shared/node.json` (`"run_list": ["role[e2e_role]"]`).

Each node runs `cinc-e2e-converge.service` on boot. The script behind it copies
the generated config to `/etc/cinc` (the shared volume is read-only inside the
node), re-registers when the seed's `run-id` changed, and then runs:

```sh
cinc-client -c /etc/cinc/client.rb -j /etc/cinc/node.json -N "$NODE_NAME"
```

The unit is `Type=oneshot` with `RemainAfterExit=yes`, so systemd stays up and
the container keeps running after the converge.

## Inspecting a converged node

```sh
# Cinc run output
docker compose exec node-debian journalctl -u cinc-e2e-converge -f

# Unit result and nginx service
docker compose exec node-debian systemctl status cinc-e2e-converge
docker compose exec node-debian systemctl status nginx
docker compose exec node-debian systemctl is-active nginx

# Shell inside the node
docker compose exec node-debian bash

# Converge again (same config, same run-id)
docker compose exec node-debian systemctl restart cinc-e2e-converge

# Or run the client directly; /etc/cinc/client.rb pins the node name and key
docker compose exec node-debian cinc-client
```

## Workstation

`workstation` is a Cinc Workstation plus OpenCode box for driving the stack by
hand. It publishes no port; use `docker compose exec`, or `./shell` for the
common case of an interactive shell:

```sh
./shell
docker compose exec workstation bash
docker compose exec workstation opencode
```

`knife` is preconfigured for the `e2e` organization: `knife node list`,
`knife cookbook list`, and the other commands work without a `-c` flag or a
`--server-url`. A bare `knife list` works because `/root/work` is pinned as the
`chef_repo_path`; from any other directory use `knife list /nodes` or
`knife node list`. Exec sessions start in `/root/work`, where OpenCode picks up
`opencode.json`. It needs a model provider once; the login is kept in the
`workstation-opencode` volume:

```sh
docker compose exec workstation opencode auth login
```

The container runs everything as root, including OpenCode; there is no
unprivileged user and no init system in the image. The staged knife config is
`/root/work/.chef` (`KNIFE_HOME`), so `knife` works from any shell in the
container.

Check the MCP connection with `docker compose exec workstation opencode mcp list`.

The MCP endpoint is `http://microwave:8000/organizations/e2e/mcp`. The bearer
token is already set as `MW_MCP_TOKEN` in the container, so a client needs no
extra setup:

```sh
docker compose exec workstation bash -c '
  curl -sS http://microwave:8000/organizations/e2e/mcp \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "Authorization: Bearer $MW_MCP_TOKEN" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}"'
```

Without the header (or with a wrong token) the endpoint answers `401`.

MCP is experimental. The token and the `pivotal` superuser principal are e2e
fixtures, not production credentials, and the MCP write tools (`delete_node`,
`delete_environment`, `delete_cookbook_version`, …) take effect immediately. To
use a different token, change it in `docker-compose.yml` (or the environment)
and in `microwave/microwave.toml` under `[mcp.organizations.e2e]`.

## Console

`cinc-console` is the official [Cinc Console](https://github.com/cinc-project/cinc-console)
web UI at <http://127.0.0.1:18081>. The browser logs in here, and the console
then acts on the server as that user while the server enforces the user's
ACLs. This fixture runs it without TLS against the seeded demo login; see
[Connecting cinc-console](../docs/deployment.md#connecting-cinc-console) for
the production shape (reverse proxy, secret files, cookie and CA settings).

The URLs below use the `18081` default; if you set `MW_CONSOLE_PORT`,
substitute the new port. The seed creates the demo login `e2e-console` by
default, with the password `e2e-console-password`; both are fixtures, not
production credentials.

The root URL answers `307` to `/login` while no session exists. After logging
in, the organization pages live under `/orgs/<org>`, for example `/orgs/e2e`
or `/orgs/e2e/nodes`. They are **not** under `/organizations/<org>`; that is
the API prefix the console builds internally from `CINC_SERVER_URL`.

The console signs its impersonation with the WebUI keypair (the private half
mounted read-only from the `console-key` volume at
`/key/webui_priv.pem`), not with `pivotal.pem`. It never mounts the server's
data volume.

The service runs as uid 1000 on a read-only root filesystem with all
capabilities dropped, and publishes only on `127.0.0.1`. Its health endpoint
is `http://127.0.0.1:<MW_CONSOLE_PORT>/api/healthz`. Like the other e2e
services it has no `restart:` policy, so a crash stays down;
`docker compose up -d cinc-console` brings it back.

Overrides: `MW_CONSOLE_PORT` moves the host port; `MW_CONSOLE_USER`,
`MW_CONSOLE_PASSWORD`, and `MW_CONSOLE_EMAIL` change the seeded login, but only
on the next seed run (`docker compose up -d seed`, or a full `up`); changing
them on a running stack alone changes nothing. The email is derived from the
user unless `MW_CONSOLE_EMAIL` is set. A seed run also writes a new `run-id`,
so the nodes re-register and re-converge.
`MW_CONSOLE_SESSION_SECRET` changes the session signing secret; the console
rejects anything shorter than 32 characters. The shipped default is a fixture,
just like the login; override it with at least 32 characters for anything but
local use. Keep one fixed value if sessions should survive a restart.
`CINC_CONSOLE_IMAGE` changes the image.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `MW_E2E_PORT` | `18080` | Host port for the microwave API |
| `MW_MCP_TOKEN` | `e2e-mcp-token` | Bearer token for the MCP endpoint |
| `MW_WORKSTATION_IMAGE` | `microwave-workstation:e2e` | Workstation image tag |
| `MW_CONSOLE_PORT` | `18081` | Loopback host port for the console |
| `MW_CONSOLE_USER` | `e2e-console` | Demo console user created by the seed |
| `MW_CONSOLE_PASSWORD` | `e2e-console-password` | Password for the demo console user |
| `MW_CONSOLE_EMAIL` | `<MW_CONSOLE_USER>@example.com` | Email for the demo console user |
| `MW_CONSOLE_SESSION_SECRET` | `e2e-console-session-secret-32-chars-min` | Console session signing secret |
| `CINC_CONSOLE_IMAGE` | `ghcr.io/cinc-project/cinc-console:0.9.0` | Console image |
| `ALPINE_IMAGE` | `alpine:3.22` | Base image for the key handoff service |
| `MW_ORG` | `e2e` | Organization name |
| `ROLE_NAME` | `e2e_role` | Role created by the seed |
| `NGINX_COOKBOOK` / `NGINX_VERSION` | `nginx` / `12.3.4` | Supermarket cookbook |
| `POSTGRES_COOKBOOK` / `POSTGRES_VERSION` | `postgresql` / `13.0.5` | Supermarket cookbook |
| `NGINX_RECIPE` | `e2e_nginx` | nginx recipe in the role run_list |
| `POSTGRES_RECIPE` | `e2e_postgresql` | PostgreSQL recipe in the role run_list |
| `NODE_NAMES` | `node-debian node-ubuntu node-fedora node-amazon` | Client names the seed resets |
| `DEBIAN_IMAGE` | `debian:trixie` | Debian node base image |
| `UBUNTU_IMAGE` | `ubuntu:26.04` | Ubuntu node base image |
| `FEDORA_IMAGE` | `fedora:45` | Fedora node base image |
| `AMAZON_IMAGE` | `amazonlinux:2023` | Amazon Linux node base image |
| `CINC_VERSION` | `19` | Cinc Client major version |

## Notes and trade-offs

- **Listener address.** `microwave/microwave.toml` binds to
  `microwave:8000`, not `0.0.0.0:8000`. microwave-server builds the
  presigned `/bookshelf` upload and download URLs from `server.bind`, so
  binding to `0.0.0.0` would hand knife and the nodes URLs they cannot reach.
  The Compose healthcheck uses the same name.
- **Cinc, not Chef Infra Client.** Debian and Ubuntu install through
  `omnitruck.cinc.sh`; Amazon Linux uses the `amazon/2023` RPM from the same
  service. Cinc has no Fedora package, so the Fedora image installs Cinc's
  compatible `el9` RPM (the omnibus package is self-contained). All paths are
  pinned by `CINC_VERSION`.
- **Newest cookbook versions.** `nginx` 12.3.4 is the newest `nginx` release,
  but it is resource-only: it ships no recipes. The seed therefore also
  uploads the local `e2e_nginx` wrapper cookbook
  (`seed/site-cookbooks/e2e_nginx`), whose default recipe runs
  `nginx_install "nginx"` (distro package) and `nginx_service "nginx"`
  (`enable` + `start`); the role's run_list references that wrapper.
  `postgresql` 13.0.5 is the newest maintained, resource-only release of the
  `postgresql` cookbook. The local `e2e_postgresql` wrapper runs
  `postgresql_install "postgresql"` with `version "18"` (the newest major in
  the pgdg archive) and `postgresql_service` with `enable` + `start` on
  Debian/Ubuntu; Fedora is not a supported platform of the cookbook, so the
  wrapper installs the distro `postgresql-server` there. No PostgreSQL
  configuration, users, or databases are managed.
- **systemd in the nodes only.** The node containers run `/sbin/init` as PID 1.
  That keeps the container alive after the converge and lets the service
  resource manage nginx on all distros. It needs `privileged: true`, a shared
  cgroup mount, `/run` and `/tmp` tmpfs, and `stop_signal: SIGRTMIN+3`; all of
  that is set in `docker-compose.yml`. A TTY is attached so the container has a
  `/dev/console` and `docker compose logs` is useful. The workstation needs
  none of it: its entrypoint runs the prepare script once and then sleeps.
- **Fedora package provider.** Fedora ships dnf5 without Python in the base
  image, but Chef's `dnf_package` provider drives libdnf through a Python
  helper. The Fedora image installs `python3`, `python3-libdnf5`, and
  `python3-rpm` so `nginx_install` works.
- **Amazon Linux package provider.** Amazon Linux 2023 ships dnf4 with its
  Python bindings built in, so its image only needs `python3`; there is no
  `python3-libdnf5` package. It also only publishes versioned PostgreSQL
  packages, which is why the wrapper installs `postgresql16-server` there.
- **Fedora DNS.** Fedora's preset enables `systemd-resolved`, which does not
  resolve Compose service names such as `microwave` through Docker's embedded
  DNS (glibc does). The node image masks `systemd-resolved` so
  `/etc/resolv.conf` is used directly.
- **Cookbook gems.** `deepsort` and `inifile` are pre-installed into Cinc's
  embedded Ruby because the `postgresql` cookbook libraries require them on
  every run, including the Fedora fallback.
- **MCP endpoint.** `microwave/microwave.toml` enables `[mcp]` with one org
  entry, a fixed token, and `allowed_hosts = ["microwave:8000"]`; requests from
  the Compose network carry that `Host` header. `data_collector.store = "local"`
  keeps run reports so the MCP run tools return data. See "Workstation" for the
  client side.
- **Console key rotation.** `rotate-credentials webui` changes the stored
  keypair, and a server restart materializes the new key. The console then
  keeps the old copy in the `console-key` volume until
  `docker compose up -d console-key` re-runs that service, and it reads the key
  only at boot, so restart `cinc-console` afterwards. Existing console sessions
  keep working, but every server call fails with `401` while the console holds
  the old key. A broken or unreadable key does not show in `/api/healthz`
  (still `200`); logins fail with `500`; restore the file and restart
  `cinc-console`. A **full** `docker compose up -d` also works, but it re-runs
  the seed, writes a new `run-id`, and makes every node re-register and
  re-converge, so it is the heavier option, not the default recovery.
- **Repeated runs.** A validator may create clients but not update them.
  `chef-client` turns the resulting `409 Client already exists` into a `PUT`,
  which the validator is not allowed to do, so a node whose previous converge
  failed cannot simply register again. The seed deletes the node and client
  objects with the superuser key and publishes a new `run-id`; a node whose
  stored `run-id` differs removes its client key and registers again. A plain
  `docker compose restart node-*` keeps the existing key because the
  generation did not change.
- **Re-converging.** A `cinc-e2e-converge.path` unit watches `/shared/run-id`.
  The seed publishes a new generation marker on every start, so a repeat
  `docker compose up -d` makes the already-running nodes re-register and
  converge again. A manual `systemctl restart cinc-e2e-converge` on a node
  re-runs the client with the current config (same generation, no
  re-registration).
- **Periodic converge.** The `e2e_timer` cookbook enables `cinc-client.timer`,
  which runs `cinc-client.service` every ten minutes with one minute of splay;
  the first run follows ten minutes after the unit starts. The timer is in
  addition to the boot converge and the path trigger. Changes to the role, the
  environment, or node attributes made in cinc-console or over MCP therefore
  arrive within one interval. A failed run leaves the timer active and the next
  run starts on schedule. Node-level run lists are an exception: every run
  passes the seed's `node.json`, whose `run_list` (`role[e2e_role]`) replaces
  the server-side node run list, so put run-list changes in the role.

## Inspecting the server

```sh
# Status probe
curl -fsS http://127.0.0.1:18080/_status | jq .

# Validate the server configuration. `check-config` parses `server.bind` as a
# literal address, so the Compose name in the file always fails it:
docker compose exec microwave sh -c 'sed "s/^bind = .*/bind = \"127.0.0.1:8000\"/" \
  /etc/microwave/microwave.toml > /tmp/c.toml && microwave-ctl --config /tmp/c.toml check-config'
```

The seed summary is in the `chef-shared` volume:

```sh
docker compose exec node-debian cat /shared/seed-summary.txt
```
