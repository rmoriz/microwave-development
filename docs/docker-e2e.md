# Docker e2e stack

`docker-e2e/` is a Compose stack that runs a Chef Server-shaped test end to end:
a Microwave server, a seeded organization with cookbooks and a role, four Linux
nodes that converge with Cinc Client, a web console, and a workstation with
knife and OpenCode. Everything runs in containers; the host only needs Docker.

Use it to watch a real `chef-client` run against the server, to try the console
or MCP, or to reproduce a provisioning bug. It is a test fixture, not a
production deployment: every login below is a fixed fixture, and the node
containers run privileged.

## Prerequisites

- Docker Engine with Compose v2.
- A few GB of free disk for the images, the volumes and the cookbook downloads.
- Network access on the first start: the seed downloads cookbooks from Chef
  Supermarket and the nodes install Cinc Client.

No Rust toolchain is needed, because the server image is pulled. A source build
is a separate path and compiles inside Docker.

## Quick start

```sh
git clone https://github.com/rmoriz/microwave-development.git
cd microwave-development
cd docker-e2e
docker compose up -d
```

Compose starts `microwave` first and runs `seed` once the server is healthy. The
nodes, the console and the workstation wait for the seed to finish, then the
nodes register and converge on their own.

Follow the seed, then the node output:

```sh
docker compose logs -f seed
docker compose logs -f node-debian node-ubuntu node-fedora node-amazon
```

The API answers on `http://127.0.0.1:18080`; the console on
`http://127.0.0.1:18081`. The first converge takes a few minutes because it
installs nginx and PostgreSQL.

## Containers and ports

Compose prefixes every container with the project name, so the names below are
`microwave-e2e-<service>-1`. Only two services publish a port.

| Container | Purpose | Published port |
| --- | --- | --- |
| `microwave-e2e-microwave-1` | The server: API, `/bookshelf`, MCP | `18080` on all interfaces |
| `microwave-e2e-seed-1` | Creates the organization, cookbooks, role and demo login, then exits | none |
| `microwave-e2e-console-key-1` | Copies the WebUI key for the console, then exits | none |
| `microwave-e2e-cinc-console-1` | Web console | `127.0.0.1:18081` |
| `microwave-e2e-node-debian-1` | Debian node running Cinc Client against the role | none |
| `microwave-e2e-node-ubuntu-1` | Ubuntu node running Cinc Client against the role | none |
| `microwave-e2e-node-fedora-1` | Fedora node running Cinc Client against the role | none |
| `microwave-e2e-node-amazon-1` | Amazon Linux node running Cinc Client against the role | none |
| `microwave-e2e-workstation-1` | knife and OpenCode against the stack, for `docker compose exec` | none |

The API port is reachable from the whole network; the console is loopback only.
`MW_E2E_PORT` and `MW_CONSOLE_PORT` move both.

## Logins

| Access | User | Secret |
| --- | --- | --- |
| Console at `http://127.0.0.1:18081` | `e2e-console` | `e2e-console-password` |
| Server superuser | `pivotal` | `docker compose exec microwave cat /var/lib/microwave/pivotal.password` |
| MCP endpoint | — | bearer `e2e-mcp-token`, already exported as `$MW_MCP_TOKEN` in `workstation` |

The nodes need no login: the seed writes a validator key and a client config to
the shared volume, and each node registers itself. The console user is recreated
on every seed run, and `MW_CONSOLE_USER` / `MW_CONSOLE_PASSWORD` change it.

## Getting a shell

The workstation is preconfigured for the `e2e` organization, so `knife node
list` and friends work without flags. Use it through Compose:

```sh
./shell
docker compose exec workstation bash
docker compose exec workstation opencode
```

A node takes the same shape:

```sh
docker compose exec node-debian bash
docker compose exec node-debian journalctl -u cinc-e2e-converge -f
docker compose exec node-debian systemctl status nginx
```

Check the server from the host:

```sh
curl -fsS http://127.0.0.1:18080/_status | jq .
```

## Pinning a version

The default tag is `latest`, which moves with every stable release. Pin a
version or digest when a run has to be reproducible:

```sh
MW_IMAGE=ghcr.io/rmoriz/microwave-server:0.3.1 docker compose up -d
```

## Building from source

To test a change from this checkout, use the build overlay. It compiles the
repository root `Dockerfile` and tags the result locally, so the pulled `latest`
image stays untouched:

```sh
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build
```

The seed, node and workstation images have no published counterpart and are
always built locally.

## Tearing down

```sh
docker compose down -v
```

`-v` also removes the server data volume and the generated client keys, so the
next start begins from scratch. Without `-v`, a later `up` reuses the
organization and the seed resets the node fixtures.

## Limits

- Fixture credentials, fixed ports and a seeded demo organization; change them
  before exposing the stack to anything but localhost.
- `latest` is moving. Use `MW_IMAGE` for a reproducible run.
- The node containers run `systemd` as PID 1 and are `privileged`, with the host
  cgroup mounted read-write.
- The nodes converge nginx and PostgreSQL only; there is no application data and
  no PostgreSQL tuning.

## Where the details are

- [`docker-e2e/README.md`](../docker-e2e/README.md) — the stack's mechanics,
  every knob, and the repeated-run and console-key notes.
- [`deployment.md`](deployment.md) — running the server outside this stack.
