# docker-postgres-opensearch-garage backends Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run the provisioning e2e fixture against PostgreSQL 19, OpenSearch 3.9.0 and Garage 2.5.0 instead of SQLite, embedded search and the local bookshelf.

**Architecture:** The example stays a full e2e stack. Three backend services are added with healthchecks; the official Garage image is the server and a short-lived sidecar (same pinned binary in `alpine`, sharing Garage's network namespace) provisions layout, bucket and key before the seed runs. The server config points only at the backends.

**Tech Stack:** Docker Compose v2, `postgres:19beta2-alpine`, `opensearchproject/opensearch:3.9.0`, `dxflrs/garage:v2.5.0`, Microwave Server TOML config.

**Spec:** `docs/superpowers/specs/2026-11-08-docker-postgres-opensearch-garage-design.md`

## Global Constraints

- Versions are pinned exactly as requested: `postgres:19beta2-alpine`, `opensearchproject/opensearch:3.9.0`, `dxflrs/garage:v2.5.0`.
- PostgreSQL 19's image uses `PGDATA=/var/lib/postgresql/19/docker`; the volume mounts `/var/lib/postgresql`.
- The object-store endpoint is `http://garage:3900` (service name, never `host.docker.internal`).
- Garage-free fixture credentials, copied from `packaging/garage.toml` / `docker-compose.ha.yml`: access key `GK60459373BB628A8B007A69FB`, secret `72ab6d386bc67fe916f63773bf5b5d1498eb252f3829e774dc11c673f16e96e6`, bucket `chef-cookbooks`, region `garage`.
- All new paths stay under `examples/docker-postgres-opensearch-garage/` and `docs/superpowers/`; `examples/docker-e2e` and the root compose files are untouched.
- `packaging/garage.toml` is mounted, never copied.
- No backend publishes a host port; `garage-bootstrap` uses `network_mode: "service:garage"` and `restart: "no"`.
- Fixture credentials and the beta PostgreSQL are documented as such.

## Review Focus

- Presigned cookbook URLs must carry `garage:3900` and resolve from both `seed` and the nodes; a host-only endpoint breaks the node download.
- PostgreSQL 19 beta must complete migrations on first start with the volume on `/var/lib/postgresql`; a wrong path loops the init.
- `garage-bootstrap` must be idempotent on a second `up -d` (layout applied, bucket and key present) instead of failing under `set -eu`.
- OpenSearch 3.9.0 must come up with the security plugin and demo config disabled; leftover security makes the server's plain HTTP calls fail.
- Teardown must remove containers, volumes, the built bootstrap image and the three pulled images in the same step.

---

### Task 1: Backend services and server configuration

**Files:**
- Modify: `examples/docker-postgres-opensearch-garage/docker-compose.yml`
- Modify: `examples/docker-postgres-opensearch-garage/microwave/microwave.toml`
- Create: `examples/docker-postgres-opensearch-garage/garage/Dockerfile`
- Create: `examples/docker-postgres-opensearch-garage/garage/bootstrap.sh`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: services `postgres`, `opensearch`, `garage`, `garage-bootstrap`; volumes `postgres-data`, `opensearch-data`, `garage-meta`, `garage-data`. Task 3 relies on these names and on `seed` waiting for `garage-bootstrap`.

- [ ] **Step 1: Add the header service list entries**

In `docker-compose.yml`, extend the comment block's service list with:

```
#   postgres     - PostgreSQL 19 (beta) metadata store, internal only
#   opensearch   - OpenSearch 3.9.0 search backend, internal only
#   garage       - Garage 2.5.0 S3 store, official image, internal only
#   garage-bootstrap - one-shot layout/bucket/key provisioning for garage
```

and state that presigned `/bookshelf` URLs point at `garage:3900`, which resolves across the Compose network.

- [ ] **Step 2: Add the three backend services**

Insert before `microwave` (Compose has no ordering requirement; this keeps backends first):

```yaml
  postgres:
    image: ${POSTGRES_IMAGE:-postgres:19beta2-alpine}
    restart: unless-stopped
    environment:
      POSTGRES_DB: microwave
      POSTGRES_USER: microwave
      POSTGRES_PASSWORD: microwave
    volumes:
      # PostgreSQL 18+ keeps PGDATA below the major version; the volume is the
      # parent.
      - postgres-data:/var/lib/postgresql
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U microwave -d microwave"]
      interval: 3s
      timeout: 3s
      retries: 30
      start_period: 5s

  opensearch:
    image: ${OPENSEARCH_IMAGE:-opensearchproject/opensearch:3.9.0}
    restart: unless-stopped
    environment:
      discovery.type: single-node
      DISABLE_SECURITY_PLUGIN: "true"
      DISABLE_INSTALL_DEMO_CONFIG: "true"
      OPENSEARCH_JAVA_OPTS: ${OPENSEARCH_JAVA_OPTS:--Xms512m -Xmx512m}
    ulimits:
      memlock:
        soft: -1
        hard: -1
      nofile:
        soft: 65536
        hard: 65536
    volumes:
      - opensearch-data:/usr/share/opensearch/data
    healthcheck:
      test:
        [
          "CMD-SHELL",
          "curl -fsS 'http://localhost:9200/_cluster/health?wait_for_status=yellow&timeout=5s' >/dev/null || exit 1",
        ]
      interval: 10s
      timeout: 10s
      retries: 24
      start_period: 20s

  garage:
    image: ${GARAGE_IMAGE:-dxflrs/garage:v2.5.0}
    restart: unless-stopped
    command: ["/garage", "-c", "/etc/garage.toml", "server"]
    volumes:
      - garage-meta:/var/lib/garage/meta
      - garage-data:/var/lib/garage/data
      - ../../packaging/garage.toml:/etc/garage.toml:ro
    healthcheck:
      test: ["CMD", "/garage", "-c", "/etc/garage.toml", "status"]
      interval: 5s
      timeout: 5s
      retries: 12
      start_period: 5s
```

- [ ] **Step 3: Add the bootstrap sidecar**

```yaml
  # The official Garage image has no shell, so the bootstrap cannot run as a
  # command in the server container: the node id is only known at runtime.
  # This short-lived sidecar shares garage's network namespace, which keeps
  # rpc_public_addr 127.0.0.1 valid, and provisions through the local CLI.
  garage-bootstrap:
    build:
      context: ./garage
    image: ${MW_GARAGE_BOOTSTRAP_IMAGE:-microwave-garage-bootstrap:e2e}
    depends_on:
      garage:
        condition: service_healthy
    network_mode: "service:garage"
    restart: "no"
    environment:
      MW_S3_BUCKET: ${MW_S3_BUCKET:-chef-cookbooks}
      MW_S3_ACCESS_KEY: ${MW_S3_ACCESS_KEY:-GK60459373BB628A8B007A69FB}
      MW_S3_SECRET_KEY: ${MW_S3_SECRET_KEY:-72ab6d386bc67fe916f63773bf5b5d1498eb252f3829e774dc11c673f16e96e6}
    volumes:
      - ../../packaging/garage.toml:/etc/garage.toml:ro
```

- [ ] **Step 4: Wire the dependencies and volumes**

On `microwave`, add:

```yaml
    depends_on:
      postgres:
        condition: service_healthy
      opensearch:
        condition: service_healthy
      garage:
        condition: service_healthy
```

On `seed`, after its `microwave` dependency, add:

```yaml
      garage-bootstrap:
        condition: service_completed_successfully
```

In the top-level `volumes:` block add `postgres-data:`, `opensearch-data:`, `garage-meta:` and `garage-data:`.

- [ ] **Step 5: Create `garage/Dockerfile`**

```dockerfile
# Provisioning sidecar for the Garage server, which ships as a static binary
# without a shell. The server itself runs the untouched dxflrs/garage image.
FROM dxflrs/garage:v2.5.0 AS garage

FROM alpine:3.22

COPY --from=garage /garage /garage
COPY bootstrap.sh /usr/local/bin/garage-bootstrap.sh
RUN chmod 0755 /usr/local/bin/garage-bootstrap.sh

ENTRYPOINT ["/bin/sh", "/usr/local/bin/garage-bootstrap.sh"]
```

- [ ] **Step 6: Create `garage/bootstrap.sh`**

```sh
#!/bin/sh
# One-shot Garage provisioning for the docker-postgres-opensearch-garage stack.
# Runs in garage's network namespace; layout/bucket/key creation is repeated
# harmlessly on a second run.
set -eu

bucket="${MW_S3_BUCKET:?MW_S3_BUCKET is required}"
access_key="${MW_S3_ACCESS_KEY:?MW_S3_ACCESS_KEY is required}"
secret_key="${MW_S3_SECRET_KEY:?MW_S3_SECRET_KEY is required}"
capacity="${MW_GARAGE_CAPACITY:-1G}"

until /garage -c /etc/garage.toml status >/dev/null 2>&1; do
    sleep 1
done

node_id="$(/garage -c /etc/garage.toml node id -q | cut -d@ -f1)"

/garage -c /etc/garage.toml layout assign -z dc1 -c "$capacity" "$node_id" || true
/garage -c /etc/garage.toml layout apply --version 1 || true
/garage -c /etc/garage.toml bucket create "$bucket" || true
/garage -c /etc/garage.toml key import --yes "$access_key" "$secret_key" || true
/garage -c /etc/garage.toml bucket allow --read --write --owner "$bucket" --key "$access_key"

echo "garage ready: bucket $bucket, key $access_key"
```

- [ ] **Step 7: Replace the backend config**

In `microwave/microwave.toml`, replace the `[database]`, `[search]` and `[object_store]` blocks with:

```toml
[database]
# PostgreSQL 19 beta. Fixture credentials, matching the postgres service.
driver = "postgres"
url = "postgres://microwave:microwave@postgres:5432/microwave"

[search]
backend = "opensearch"
url = "http://opensearch:9200"

[object_store]
# Presigned URLs carry this endpoint, so it must resolve from the seed and the
# nodes as well: the Compose service name, not a host-side address.
backend = "garage"
endpoint = "http://garage:3900"
bucket = "chef-cookbooks"
region = "garage"
path_style = true
access_key = "GK60459373BB628A8B007A69FB"
secret_key = "72ab6d386bc67fe916f63773bf5b5d1498eb252f3829e774dc11c673f16e96e6"
```

- [ ] **Step 8: Verify the compose model renders**

Run: `cd examples/docker-postgres-opensearch-garage && docker compose config -q`
Expected: exit 0, no warnings about the mounts, the bootstrap build context or the volumes.

- [ ] **Step 9: Commit**

```bash
git add examples/docker-postgres-opensearch-garage/docker-compose.yml \
  examples/docker-postgres-opensearch-garage/microwave/microwave.toml \
  examples/docker-postgres-opensearch-garage/garage
git commit -m "feat(examples): add external backends to the postgres/opensearch/garage stack"
```

---

### Task 2: README for the external-backend stack

**Files:**
- Modify: `examples/docker-postgres-opensearch-garage/README.md`

**Interfaces:**
- Consumes: service names and defaults from Task 1.
- Produces: nothing code-facing.

- [ ] **Step 1: Rewrite the framing**

Change the title to `# docker-postgres-opensearch-garage — Microwave Server provisioning test against external backends`. In the intro list, replace item 1 so the server is described as running against PostgreSQL, OpenSearch and Garage instead of SQLite/embedded/local, and add a backends item naming the three pinned images and the one-shot bootstrap. Keep items for seed, nodes, workstation and console.

- [ ] **Step 2: Document the backends and their health**

Add a `## Backends` section with inspection commands and the presigned-URL rule:

```sh
docker compose exec postgres psql -U microwave microwave -c '\dt'
docker compose exec opensearch curl -sS 'http://localhost:9200/_cluster/health?pretty'
docker compose exec garage /garage -c /etc/garage.toml bucket list
docker compose ps
```

State that Garage is the untouched official image (no shell, hence the bootstrap sidecar), that the endpoint is `http://garage:3900`, and that no backend publishes a host port.

- [ ] **Step 3: Update the reference tables**

In the container table add rows for `postgres`, `opensearch`, `garage` and `garage-bootstrap`, all with port `—`. In the configuration table add `POSTGRES_IMAGE`, `OPENSEARCH_IMAGE`, `OPENSEARCH_JAVA_OPTS`, `GARAGE_IMAGE`, `MW_GARAGE_BOOTSTRAP_IMAGE`, `MW_GARAGE_CAPACITY`, `MW_S3_BUCKET`, `MW_S3_ACCESS_KEY` and `MW_S3_SECRET_KEY` with the Task 1 defaults.

- [ ] **Step 4: Fix the notes**

Replace the SQLite mentions with the PostgreSQL 19 beta status (matching `docs/dependency-audit.md`): beta, unvalidated with Chef tooling in this repository, volume on `/var/lib/postgresql`. Keep the existing listener, systemd, MCP and cleanup notes. Add a note that `docker compose down -v --rmi local` also removes the built bootstrap image, and that the three pulled upstream images must be removed separately.

- [ ] **Step 5: Commit**

```bash
git add examples/docker-postgres-opensearch-garage/README.md
git commit -m "docs(examples): describe the external backends in the stack README"
```

---

### Task 3: Run the stack and verify the blob and search paths

**Files:**
- No source changes unless a check fails.

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces: evidence for the Review Focus items.

- [ ] **Step 1: Start the stack**

Run: `cd examples/docker-postgres-opensearch-garage && docker compose up -d --build`
Expected: images pull/build, `garage-bootstrap` and `seed` exit 0, all services settle.

- [ ] **Step 2: Confirm the backends are healthy and the bootstrap succeeded**

Run: `docker compose ps`
Expected: `postgres`, `opensearch`, `garage` and `microwave` `healthy`; `garage-bootstrap` and `seed` exited 0.

- [ ] **Step 3: Confirm PostgreSQL and OpenSearch are the real stores**

Run: `docker compose exec postgres psql -U microwave microwave -c '\dt'`
Expected: the microwave tables are listed.
Run: `docker compose exec opensearch curl -sS http://localhost:9200/chef/_count`
Expected: a count above 0 after the node converge.

- [ ] **Step 4: Confirm blobs went through Garage**

Run: `docker compose exec garage /garage -c /etc/garage.toml bucket list`
Expected: `chef-cookbooks` is listed.
Run: `docker compose logs seed | grep -i shelf`
Expected: the upload URLs name `garage:3900`.

- [ ] **Step 5: Confirm a node converges**

Run: `docker compose logs -f node-debian`
Expected: a completed `cinc-client` run with `recipe[e2e_nginx]` and `recipe[e2e_postgresql]`, no checksum or presigned-URL failures.

- [ ] **Step 6: Confirm the bootstrap is idempotent**

Run: `docker compose up -d seed garage-bootstrap`
Expected: both exit 0 again; the seed uploads without `BucketAlreadyExists`/`KeyAlreadyExists` failures.

- [ ] **Step 7: Tear down completely**

Run: `docker compose down -v --rmi local`
Then: `docker rmi postgres:19beta2-alpine opensearchproject/opensearch:3.9.0 dxflrs/garage:v2.5.0`
Expected: no container, volume, network or image from this run remains (`docker ps -a` and `docker images` clean for these names).

- [ ] **Step 8: Commit any fixes**

```bash
git add examples/docker-postgres-opensearch-garage docs/superpowers
git commit -m "fix(examples): keep the external-backend stack reproducible"
```

If there were no fixes, skip the commit.
