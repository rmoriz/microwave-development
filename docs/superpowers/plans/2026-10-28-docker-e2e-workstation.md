# docker-e2e Workstation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an exec-only `workstation` container with preconfigured knife and an OpenCode MCP client, move both tool images to plain Debian plus the Cinc Workstation package, and replace the `postgres` cookbook with a `postgresql` wrapper that installs PostgreSQL 18.

**Architecture:** Both tool images derive from `debian:trixie` and install the pinned Cinc Workstation `.deb`; the workstation runs systemd like the nodes and stages its knife config from the seed's read-only hand-over volume. The server enables its MCP endpoint for the e2e org with a fixed token and stores run reports locally. A local `e2e_postgresql` wrapper drives `postgresql_install` on Debian/Ubuntu and a distro fallback on Fedora.

**Tech Stack:** Docker Compose, Debian trixie, systemd, Cinc Workstation 26.4.0 (knife 19.3.4, berks 8.1.23), Cinc Client 19, Chef cookbooks, OpenCode V2, microwave-server MCP.

**Spec:** `docs/superpowers/specs/2026-10-28-docker-e2e-workstation-design.md`

## Global Constraints

- Base image everywhere in this plan: `debian:trixie`. No Ruby base image, no
  `gem install` of knife or berk shelf.
- Cinc Workstation version `26.4.0`, package
  `cinc-workstation_26.4.0-1_<dpkg --print-architecture>.deb` from
  `https://downloads.cinc.sh/files/stable/cinc-workstation/26.4.0/debian/13`.
- MCP token value `e2e-mcp-token`, header form `Authorization: Bearer <token>`,
  organization `e2e`.
- `allowed_hosts` value: `["microwave:8000"]`. No published workstation port.
- `data_collector.store = "local"`.
- Workstation gets the node systemd flags: `privileged: true`, `cgroup: host`,
  `stop_signal: SIGRTMIN+3`, `tmpfs: /run /tmp`, `tty: true`, cgroup mount.
- OpenCode remote MCP entry sets `oauth: false`.
- PostgreSQL major `18` on Debian/Ubuntu; Fedora fallback uses the distro
  package. No config, users, or databases.
- Node image keeps Cinc 19 and the existing masking; only packages and gems
  change.

## Review Focus

- A request from the workstation arrives with `Host: microwave:8000`; a missing
  allowlist entry must not yield a 403 loop.
- A wrong or missing bearer token must answer 401 while the correct one reaches
  `tools/list`.
- OpenCode must not attempt OAuth against the bearer-only endpoint.
- MCP write tools run as `pivotal`; `delete_node` on a scratch node must work.
- A Fedora converge must not hit `Unsupported platform_family fedora` from the
  postgresql resource.
- `latest_run` must return data after a node converges (local store).
- A second `docker compose up -d` must still converge (seed reset, role
  idempotency).

---

### Task 1: Seed image on Debian plus Cinc Workstation

**Files:**
- Modify: `docker-e2e/seed/Dockerfile`

**Interfaces:**
- Produces: an image whose `berks` and `knife` come from
  `/opt/cinc-workstation`, used by Task 2.

- [ ] **Step 1: Confirm the current image lacks the packaged toolchain**

Run: `cd docker-e2e/seed && docker build -t mw-seed-before . && docker run --rm --entrypoint bash mw-seed-before -c 'command -v berks'`
Expected: PASS builds; `berks` resolves under `/usr/local/bin` from the gem.

- [ ] **Step 2: Rewrite the Dockerfile**

Replace the `FROM ruby:3.4-bookworm` header and the two `RUN gem …` blocks.
Keep the comment block, `WORKDIR /seed`, the `COPY` lines, and the
`ENTRYPOINT`. New body:

```dockerfile
ARG BASE_IMAGE=debian:trixie
FROM ${BASE_IMAGE}

ARG CINC_WORKSTATION_VERSION=26.4.0
ARG CINC_WORKSTATION_URL=https://downloads.cinc.sh/files/stable/cinc-workstation

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
    && rm -rf /var/lib/apt/lists/*

# The omnibus package carries knife and Berkshelf on its own Ruby; no distro
# Ruby and no gem installs are needed. Pick the Debian architecture package.
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    url="${CINC_WORKSTATION_URL}/${CINC_WORKSTATION_VERSION}/debian/13/cinc-workstation_${CINC_WORKSTATION_VERSION}-1_${arch}.deb"; \
    curl -fsSL "$url" -o /tmp/cinc-workstation.deb; \
    dpkg -i /tmp/cinc-workstation.deb; \
    rm -f /tmp/cinc-workstation.deb; \
    knife --version; \
    berks --version
```

Update the header comment to say the image installs knife and Berkshelf from
the Cinc Workstation package.

- [ ] **Step 3: Build and verify the toolchain**

Run: `cd docker-e2e/seed && docker build -t mw-seed-after . && docker run --rm --entrypoint bash mw-seed-after -c 'knife --version && berks --version && ruby -v'`
Expected: `knife: 19.3.4`, `8.1.23`, no gem install step, and `berks` resolves.

- [ ] **Step 4: Verify Supermarket vendoring still works without the minitar pin**

Run: `docker run --rm -w /tmp/v mw-seed-after bash -c 'mkdir -p /tmp/v/out && printf "source \"https://supermarket.chef.io\"\ncookbook \"nginx\", \"12.3.4\"\n" > /tmp/v/Berksfile && berks vendor /tmp/v/out && test -d /tmp/v/out/nginx'`
Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git add docker-e2e/seed/Dockerfile
git commit -m "build(e2e): move seed image to Debian and Cinc Workstation"
```

---

### Task 2: Replace the postgres cookbook with a postgresql wrapper

**Files:**
- Create: `docker-e2e/seed/site-cookbooks/e2e_postgresql/metadata.rb`
- Create: `docker-e2e/seed/site-cookbooks/e2e_postgresql/recipes/default.rb`
- Modify: `docker-e2e/seed/seed.sh`
- Modify: `docker-e2e/client/Dockerfile`
- Modify: `docker-e2e/README.md` (postgres notes and defaults)

**Interfaces:**
- Consumes: Task 1's seed image.
- Produces: role run_list `recipe[e2e_nginx], recipe[e2e_postgresql]`; the
  workstation in Task 5 receives role name `e2e_role`.

- [ ] **Step 1: Add the wrapper cookbook**

`metadata.rb`:

```ruby
name 'e2e_postgresql'
maintainer 'Microwave E2E'
license 'Apache-2.0'
description 'Installs the newest PostgreSQL through the resource-only postgresql cookbook'
version '0.1.0'
chef_version '>= 18.0'

depends 'postgresql'
```

`recipes/default.rb`:

```ruby
# Debian/Ubuntu: the resource adds the pgdg archive and installs the newest
# major. Fedora is not a supported platform of the cookbook (its repository
# action raises), so use the distro package there.
if platform_family?('debian')
  postgresql_install 'postgresql' do
    version '18'
    action :install
  end

  postgresql_service 'postgresql' do
    action %i(enable start)
  end
else
  package 'postgresql-server'

  execute 'postgresql-setup --initdb' do
    creates '/var/lib/pgsql/data/PG_VERSION'
  end

  service 'postgresql' do
    action %i(enable start)
  end
end
```

- [ ] **Step 2: Switch the seed defaults and drop the paths shim**

In `seed.sh` set `POSTGRES_COOKBOOK="${POSTGRES_COOKBOOK:-postgresql}"`,
`POSTGRES_VERSION="${POSTGRES_VERSION:-13.0.5}"`, and
`POSTGRES_RECIPE="${POSTGRES_RECIPE:-e2e_postgresql}"`. Delete the
`paths/attributes/default.rb` `sed` block and its comment. Keep the
`recipe[$POSTGRES_RECIPE]` interpolation in `role.rb` and in the summary.

Replace the single `knife cookbook upload e2e_nginx` call with a loop over
`e2e_nginx e2e_postgresql`, so the second wrapper is uploaded too. A wrapper
that is only in `site-cookbooks` is otherwise missing on the server and the
node run fails with `412 Precondition Failed` / "Missing Cookbooks".

- [ ] **Step 3: Prepare the node image for the new cookbook**

In `client/Dockerfile` apt branch add `gnupg` and `lsb-release` (the resource
derives the apt distribution from `node['lsb']['codename']` and imports a key).
Replace the scanf/ipaddr_extensions gem line with:

```dockerfile
# `postgresql` library files require these gems on every run, including the
# Fedora fallback where the resource itself is not used.
RUN /opt/cinc/embedded/bin/gem install --no-document deepsort:0.5.0 inifile:3.0.0
```

- [ ] **Step 4: Build and verify the image prep**

Run:
`docker build --build-arg BASE_IMAGE=ubuntu:24.04 -t mw-node-check docker-e2e/client && docker run --rm --entrypoint bash mw-node-check -c '/opt/cinc/embedded/bin/ruby -e "require \"deepsort\"; require \"inifile\"" && command -v lsb_release && command -v gpg'`
Expected: exit 0, `lsb_release` and `gpg` resolve.

Run: `docker build --build-arg BASE_IMAGE=fedora:41 -t mw-node-check-fedora docker-e2e/client`
Expected: exit 0.

- [ ] **Step 5: Verify the wrapper resolves and vendors**

Run: `docker run --rm --entrypoint bash mw-seed-after -c 'mkdir -p /tmp/out && cp -a /seed/site-cookbooks/e2e_postgresql /tmp/ && printf "source \"https://supermarket.chef.io\"\ncookbook \"postgresql\", \"13.0.5\"\ncookbook \"e2e_postgresql\", path: \"/tmp/e2e_postgresql\"\n" > /tmp/Berksfile && cd /tmp && berks vendor /tmp/out && test -d /tmp/out/postgresql && test -d /tmp/out/e2e_postgresql'`
Expected: `postgresql`, `yum`, and `e2e_postgresql` vendor without errors.

- [ ] **Step 6: Commit**

```bash
git add docker-e2e/seed docker-e2e/client/Dockerfile
git commit -m "feat(e2e): install PostgreSQL 18 through the postgresql cookbook"
```

---

### Task 3: Workstation image, prepare unit, and OpenCode config

**Files:**
- Create: `docker-e2e/workstation/Dockerfile`
- Create: `docker-e2e/workstation/workstation-prepare.sh`
- Create: `docker-e2e/workstation/workstation-prepare.service`
- Create: `docker-e2e/workstation/opencode.json`
- Create: `docker-e2e/workstation/AGENTS.md`

**Interfaces:**
- Produces: image `mw-workstation-e2e`, unit `workstation-prepare.service`,
  `/work/opencode.json`, and `/root/.chef/knife.rb` for Task 4 and Task 7.

- [ ] **Step 1: Write the Dockerfile**

Follow `client/Dockerfile` structure: same header comment style, `ARG
BASE_IMAGE=debian:trixie`, Cinc Workstation install as in Task 1, plus:

```dockerfile
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git gnupg less procps systemd systemd-sysv vim-tiny \
    && apt-get clean

ARG OPENCODE_VERSION=2.2.0

RUN curl -fsSL https://opencode.ai/v2/install \
        | bash -s -- --no-modify-path --version "$OPENCODE_VERSION" \
    && mv /root/.opencode/bin/opencode /usr/local/bin/opencode \
    && opencode --version | grep -qx "opencode v${OPENCODE_VERSION}" \
    && opencode --version

COPY workstation-prepare.sh /usr/local/bin/workstation-prepare
COPY workstation-prepare.service /etc/systemd/system/workstation-prepare.service
RUN chmod 0755 /usr/local/bin/workstation-prepare \
    && mkdir -p /etc/systemd/system/multi-user.target.wants /work \
    && ln -sf /etc/systemd/system/workstation-prepare.service \
              /etc/systemd/system/multi-user.target.wants/workstation-prepare.service

COPY opencode.json /work/opencode.json
COPY AGENTS.md /work/AGENTS.md

# The e2e token is baked in so an exec session needs no prefix. Compose can
# still override it with MW_MCP_TOKEN.
ENV MW_MCP_TOKEN=e2e-mcp-token

# knife 19 finds ~/.chef/knife.rb only through KNIFE_HOME.
ENV KNIFE_HOME=/root/.chef

# Exec sessions start in /work so OpenCode picks up the project config.
WORKDIR /work

RUN systemctl mask systemd-resolved getty@tty1.service getty@tty2.service \
        getty@tty3.service getty@tty4.service getty@tty5.service \
        getty@tty6.service getty-static.service

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
```

- [ ] **Step 2: Write the prepare script and unit**

`workstation-prepare.sh` waits for `/shared/knife.rb` and
`/shared/pivotal.pem` (up to 300 × 2 s, exit 1 with a message), then
`install -d -m 0700 /root/.chef`, copies both files, sets `0600`, and rewrites
the key path:

```bash
sed -i 's|client_key "/shared/pivotal.pem"|client_key "/root/.chef/pivotal.pem"|' /root/.chef/knife.rb
```

It ends by printing `knife node list`, then connects the OpenCode MCP client:
`cd /work`, one `opencode mcp list` to start the background service, an
`opencode reload` (2.2.0 registers MCP servers only on a reload, not on the
first service start), then a poll of up to 30 × 2 s for a `connected` status
and a final `opencode mcp list || true`. It exits 0 either way.

`workstation-prepare.service`: `Type=oneshot`, `RemainAfterExit=yes`,
`Wants=network-online.target`, `After=network-online.target network.target`,
`ExecStart=/usr/local/bin/workstation-prepare`,
`StandardOutput=journal+console`, `StandardError=journal+console`,
`Restart=on-failure`, `RestartSec=5`, plus three
environment lines: `Environment=HOME=/root` (systemd system units have no
`HOME` and the knife plugin loader joins it), `Environment=KNIFE_HOME=/root/.chef`,
and `PassEnvironment=MW_MCP_TOKEN` (systemd services get a clean environment, and
OpenCode's `{env:MW_MCP_TOKEN}` header is substituted in the background service,
so it must be passed from the container environment).

- [ ] **Step 3: Write the OpenCode config and agent notes**

`opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "microwave-e2e": {
        "type": "remote",
        "url": "http://microwave:8000/organizations/e2e/mcp",
        "oauth": false,
        "headers": {
          "Authorization": "Bearer {env:MW_MCP_TOKEN}"
        }
      }
    }
  }
}
```

`AGENTS.md` states: knife is preconfigured for organization `e2e` (`knife …`
works without flags), the `microwave-e2e` MCP server exposes node, environment,
cookbook, and run tools, writes run as the `pivotal` superuser, and the stack is
a disposable e2e environment.

- [ ] **Step 4: Build and verify the image**

Run: `cd docker-e2e/workstation && docker build -t mw-workstation-check . && docker run --rm --entrypoint bash mw-workstation-check -c 'knife --version && opencode --version && ls -l /work/opencode.json /etc/systemd/system/multi-user.target.wants/workstation-prepare.service'`
Expected: exit 0 and both commands print versions.

- [ ] **Step 5: Validate that OpenCode reads the MCP config**

A fresh service reports an empty list until its config is reloaded, so warm it
up the same way the prepare script does.

Run: `docker run --rm --entrypoint bash mw-workstation-check -c 'cd /work && opencode mcp list >/dev/null; opencode reload; sleep 2; opencode mcp list'`
Expected: `microwave-e2e` listed, with `failed: getaddrinfo ENOTFOUND
microwave` because the Compose network is absent here.

- [ ] **Step 6: Commit**

```bash
git add docker-e2e/workstation
git commit -m "feat(e2e): add a workstation image with knife and OpenCode"
```

---

### Task 4: Compose service and server MCP configuration

**Files:**
- Modify: `docker-e2e/docker-compose.yml`
- Modify: `docker-e2e/microwave/microwave.toml`

**Interfaces:**
- Consumes: Task 3's image and unit.
- Produces: reachable `workstation` container and an MCP endpoint at
  `http://microwave:8000/organizations/e2e/mcp`.

- [ ] **Step 1: Add the `workstation` service**

After `seed`, add:

```yaml
  workstation:
    build:
      context: ./workstation
    image: ${MW_WORKSTATION_IMAGE:-microwave-workstation:e2e}
    depends_on:
      seed:
        condition: service_completed_successfully
    environment:
      MW_MCP_TOKEN: ${MW_MCP_TOKEN:-e2e-mcp-token}
      SYSTEMD_COLORS: "0"
    tty: true
    privileged: true
    cgroup: host
    stop_signal: SIGRTMIN+3
    tmpfs:
      - /run
      - /tmp
    volumes:
      - chef-shared:/shared:ro
      - workstation-opencode:/root/.local/share/opencode
      - workstation-opencode-config:/root/.config/opencode
      - /sys/fs/cgroup:/sys/fs/cgroup:rw
```

Add both volumes under `volumes:`. Update the header service list with
`workstation` and its purpose.

- [ ] **Step 2: Configure the server**

In `microwave.toml` append:

```toml
[mcp]
enabled = true
# Requests from inside the Compose network carry this Host header.
allowed_hosts = ["microwave:8000"]

[mcp.organizations.e2e]
token = "e2e-mcp-token"
principal = "pivotal"
writes = true

[data_collector]
store = "local"
```

- [ ] **Step 3: Validate the config**

`check-config` parses `server.bind` as a literal `SocketAddr`, so the Compose
DNS name always fails it; run the check on a copy with a literal bind. The real
bind is exercised when the stack starts in Task 6.

Run: `cd docker-e2e && sed 's/^bind = "microwave:8000"/bind = "127.0.0.1:8000"/' microwave/microwave.toml > /tmp/mw-check.toml && docker run --rm -v /tmp/mw-check.toml:/c.toml:ro --entrypoint microwave-ctl microwave-server:e2e --config /c.toml check-config`
Expected: exit 0 with `ok:` lines for data_dir, database, bind, search,
object_store, keygen, and the data collector. Build the server image first if
absent.

- [ ] **Step 4: Commit**

```bash
git add docker-e2e/docker-compose.yml docker-e2e/microwave/microwave.toml
git commit -m "feat(e2e): add workstation service and enable MCP"
```

---

### Task 5: Documentation

**Files:**
- Modify: `docker-e2e/README.md`

**Interfaces:**
- Consumes: names and defaults from Tasks 1–4.

- [ ] **Step 1: Add the service and workflow**

Add `workstation` to the service list. Add a section:

```sh
docker compose exec workstation bash
docker compose exec --workdir /work workstation opencode
```

Document the one-time `opencode auth login` in that shell, the MCP endpoint
with an example `curl` `tools/list` call, that the token and `pivotal` write
principal are e2e-only, that MCP is experimental, and that login state lives in
named volumes.

- [ ] **Step 2: Update tables and notes**

Add `MW_MCP_TOKEN` (`e2e-mcp-token`) and `MW_WORKSTATION_IMAGE`. Replace the
`postgres` defaults with `postgresql` / `13.0.5` and `e2e_postgresql`. Rewrite
the postgres notes: pgdg repo, PostgreSQL 18, Fedora distro fallback, new gems.
Remove the legacy `paths` shim note. Note that `--build` is required for the
workstation and that the stack has no published workstation port.

- [ ] **Step 3: Commit**

```bash
git add docker-e2e/README.md
git commit -m "docs(e2e): document the workstation and PostgreSQL 18"
```

---

### Task 6: Full-stack verification

**Files:** none.

- [ ] **Step 1: Start from a clean volume set**

Run: `cd docker-e2e && docker compose down -v && docker compose up -d --build`
Expected: build succeeds for all five services.

- [ ] **Step 2: Wait for the stack**

Run: `docker compose ps` and `curl -fsS http://127.0.0.1:18080/_status`
Expected: `seed` exited 0, all three nodes running, status OK.

- [ ] **Step 3: Verify the PostgreSQL outcome**

Run: `docker compose exec node-debian systemctl is-active postgresql && docker compose exec node-ubuntu systemctl is-active postgresql && docker compose exec node-fedora systemctl is-active postgresql && docker compose exec node-debian psql --version`
Expected: `active` three times and `psql (PostgreSQL) 18.x`.

- [ ] **Step 4: Verify knife and the report store**

Run: `docker compose exec workstation knife node list && docker compose exec workstation knife node show node-debian`
Expected: three node names and a node document.

Run against MCP: `docker compose exec workstation bash -c 'curl -sS http://microwave:8000/organizations/e2e/mcp -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Authorization: Bearer e2e-mcp-token" -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"list_runs\",\"arguments\":{}}}"'`
Expected: a JSON result with at least one run.

- [ ] **Step 5: Verify auth, writes, and OpenCode**

Run without the header and expect 401; with a wrong token and expect 401.

Run: `docker compose exec workstation opencode mcp list`
Expected: `microwave-e2e` connected.

Run: `docker compose exec workstation bash -c 'printf "{\"name\":\"scratch\"}\n" > /tmp/scratch.json && knife node from file /tmp/scratch.json'` then call `delete_node`:

```sh
docker compose exec workstation bash -c 'curl -sS http://microwave:8000/organizations/e2e/mcp -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Authorization: Bearer e2e-mcp-token" -d "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"delete_node\",\"arguments\":{\"name\":\"scratch\"}}}"'
```

Then confirm `knife node list` no longer shows `scratch`.

- [ ] **Step 6: Verify repeatability**

Run: `docker compose up -d` again, then `docker compose exec node-debian journalctl -u cinc-e2e-converge -n 5 --no-pager`
Expected: a new converge run, still successful.

- [ ] **Step 7: Tear down**

Run: `docker compose down -v`

- [ ] **Step 8: Commit any fixups**

```bash
git add -A docker-e2e
git commit -m "fix(e2e): address verification findings"
```
