# docker-e2e workstation container with knife and OpenCode

## Intent

The e2e stack proves node convergence, but there is no place to drive the Chef
API by hand or to watch the MCP endpoint from a real agent. Add a `workstation`
service that is reachable with `docker exec`, has `knife` and an org-scoped
config ready, and carries an OpenCode install whose `microwave-e2e` MCP server
points at the stack. Reports stay local so the MCP run tools have data.

Alongside, the stack moves off the pinned gem toolchain: the seed and the
workstation build on plain Debian with the Cinc Workstation package, and the
stale `postgres` cookbook is replaced by the maintained, resource-oriented
`postgresql` cookbook with a wrapper that installs the newest PostgreSQL.

## Goals

- One interactive box with plain `knife …` and `opencode` commands, no `-c`
  flags or key copying by hand.
- MCP enabled on the e2e organization with a fixed local token, reachable from
  the workstation only.
- `data_collector.store = "local"` so `list_runs` / `latest_run` /
  `get_run` see the node converges.
- Same container plumbing as the nodes: systemd as PID 1, `privileged`,
  shared cgroup, `/run` and `/tmp` tmpfs, `SIGRTMIN+3`, TTY.
- Seed and workstation both use `debian:trixie` plus the pinned Cinc
  Workstation `.deb`; no Ruby base image and no `gem install`.
- The role installs PostgreSQL 18 through `postgresql_install` on Debian and
  Ubuntu, with a distro fallback on Fedora.

## Non-goals

- Publishing an OpenCode or knife port to the host.
- A scoped non-superuser MCP principal, ACL demos, or token rotation.
- TLS, internet-facing MCP hardening, or a client sweep beyond OpenCode.
- A tuned PostgreSQL config, users, databases, or replication. The wrapper
  installs and starts the server only.

## Anti-goals

- An admin box that needs `-c /shared/knife.rb` on every call.
- A second knife setup path that drifts from the seed's.
- Any new host prerequisite outside `docker compose`.
- MCP that works only after undocumented manual config.
- A seed that fails on a cookbook the run never uses, or a role that breaks
  one distro because the cookbook has no branch for it.

## Constraints

- The seed is the only writer of the pivotal key and the org `knife.rb`;
  the workstation mounts `chef-shared` read-only and stages copies.
- Docker official `debian:trixie` is the base for seed and workstation.
  Verified: `cinc-workstation_26.4.0-1_<arch>.deb` (arm64 and amd64 published)
  installs cleanly, `/sbin/init` resolves, and its bundled `knife` 19.3.4 and
  `berks` 8.1.23 run. Its bundled minitar 1.1.0 vendors a Supermarket cookbook,
  so the seed's 0.9 pin and its uninstall step go away.
- Per-organization MCP entries have no environment override, so the token is a
  fixed e2e value in the mounted TOML. The container reads the same value from
  `MW_MCP_TOKEN`.
- The MCP transport checks `Host`; the allowlist needs `microwave:8000`.
- OpenCode needs a model or login at runtime; the image only ships the client.
- `postgresql` 13.0.5 is the newest release and needs Chef/Cinc 18+. It
  supports Debian and Ubuntu and has `apt_repository`/`apt_update` as Chef core
  resources, so it needs no `apt` cookbook. It has no Fedora branch: the
  repository action raises on `platform_family` `fedora`.
- The resource derives the apt distribution from `node['lsb']['codename']`, so
  Debian and Ubuntu nodes need `lsb-release`. Key import needs `gnupg`.
- The cookbook libraries `require 'deepsort'` and `require 'inifile'`. Without
  the gems the cookbook fails to load even for a Fedora run that skips the
  resource.
- Latest available major in the pgdg archives for bookworm and noble, arm64
  included, is 18. No 19 package exists yet.

## Approaches

1. **Workstation on `debian:trixie` with the Cinc Workstation package**
   (chosen) and the same package in the seed image. One toolchain for both
   boxes, a single `dpkg -i`, no Ruby base, no gem pin. Costs a 119–133 MB
   package per image and a knife version that differs from the old seed gem.
2. `ruby:4-trixie` with `gem install knife` and berkshelf. Same knife family
   as today, but keeps the toolchain and the minitar pin.
3. Extend the `client` node image. No new Dockerfile, but every node then ships
   OpenCode, knife, and systemd masking it does not use.

Staging knife config in a systemd oneshot unit beats `-c` flags and beats
mounting `/shared` as the home: manual commands stay plain, and the key is not
read from a hand-over directory that Compose may recreate.

For PostgreSQL, wrapping the cookbook resource in a local `e2e_postgresql`
cookbook keeps the role uniform across distros and confines the Fedora
workaround. Replacing the role entry with a raw `package` resource would lose
the pgdg repo and the newest version.

## Decisions

- Service `workstation`, `depends_on: seed: service_completed_successfully`.
- Both Dockerfiles take `ARG BASE_IMAGE=debian:trixie`, `ARG
  CINC_WORKSTATION_VERSION=26.4.0`, and a downloads base URL, then
  `dpkg -i` the arch-matched `cinc-workstation_<version>-1_<dpkg arch>.deb`.
- `docker-e2e/workstation/Dockerfile` also installs `systemd`, `systemd-sysv`,
  `curl`, `ca-certificates`, `procps`, `git`, `less`, `vim-tiny`, OpenCode from
  the installer moved to `/usr/local/bin/opencode`, masks getty units, sets
  `STOPSIGNAL SIGRTMIN+3` and `CMD ["/sbin/init"]`.
- `docker-e2e/seed/Dockerfile` keeps `WORKDIR /seed`, the script copies, and
  the entrypoint; the `gem install`, `build-essential`, and minitar
  uninstall lines are removed.
- `workstation-prepare.service` (oneshot, `RemainAfterExit`, enabled): waits
  for `/shared/knife.rb` and `/shared/pivotal.pem`, copies both to
  `/root/.chef/` mode `0600`/`0700`, and rewrites the `client_key` path.
- `/work` carries `opencode.json` (remote MCP, `oauth: false`, header
  `Authorization: Bearer {env:MW_MCP_TOKEN}`) and an `AGENTS.md` naming the
  knife config and the MCP tools. No secrets beyond the e2e token.
- Named volumes keep `/root/.local/share/opencode` and
  `/root/.config/opencode` across rebuilds, so a login survives.
- `microwave.toml`: `[mcp] enabled = true`, `allowed_hosts =
  ["microwave:8000"]`, `[mcp.organizations.e2e]` with the fixed token,
  `principal = "pivotal"`, `writes = true`; `[data_collector] store = "local"`.
- Compose env `MW_MCP_TOKEN` is an optional override; the image itself sets
  `ENV MW_MCP_TOKEN=e2e-mcp-token`, so an exec session has the token without
  the user passing it. The README documents the exec workflow, the login step,
  and that the token is e2e-only.
- Seed Berksfile pins `postgresql` 13.0.5 instead of `postgres` 2.17.3. The
  `paths` attribute shim disappears; nothing in the new dependency set uses the
  legacy `platform` helper.
- New local wrapper `seed/site-cookbooks/e2e_postgresql` with
  `depends 'postgresql'`: on Debian family it runs `postgresql_install` with
  `version '18'` and `postgresql_service` with `enable` + `start`; on Fedora it
  installs `postgresql-server`, runs `postgresql-setup --initdb` guarded by the
  data directory, and enables/starts `postgresql`.
- `POSTGRES_RECIPE` defaults to `e2e_postgresql`. The seed summary and README
  name cookbook 13.0.5 and PostgreSQL 18.
- Client image adds `lsb-release` and `gnupg` to the apt branch and installs
  `deepsort` and `inifile` into Cinc's embedded Ruby. `scanf` and
  `ipaddr_extensions` are dropped with the old cookbook chain.

## Left to the builder

- Exact package list, installer invocation, and unit file content.
- Whether `workstation-prepare` also seeds a sample cookbook repo or stops at
  config staging.
- Whether the Fedora fallback runs `postgresql-setup --initdb` through the
  package's own unit preset instead.
- README wording and whether the seed summary names the MCP endpoint.
