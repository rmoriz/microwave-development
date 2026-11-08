# docker-postgres-opensearch-garage example

## Intent

`examples/docker-postgres-opensearch-garage` is currently an untracked,
byte-identical copy of `examples/docker-e2e`: SQLite, in-process search, local
`/bookshelf`. Turn it into the external-backend variant of that stack so the
provisioning fixtures run against a server whose metadata, search and blob
stores are separate containers.

The motivating case is the topology from
[docs/deployment.md](../../deployment.md#9-ha-layout) that has never been
exercised by a full `cinc-client` run: PostgreSQL metadata plus remote search
plus an S3-compatible object store, with real cookbook uploads and downloads.

## Goals

- Add `postgres`, `opensearch` and `garage` services to the example's
  `docker-compose.yml`, pinned as requested: `postgres:19beta2-alpine`,
  `opensearchproject/opensearch:3.9.0`, `dxflrs/garage:v2.5.0`.
- Configure `microwave/microwave.toml` for `postgres` / `opensearch` / `garage`
  so the server starts only against the backends, with no embedded fallback in
  use.
- Prove the full path: the seed's `knife cookbook upload` writes blobs through
  Garage's presigned URLs, a node downloads and converges them, and search
  documents land in OpenSearch.
- Keep the rest of the stack intact: seed, console, workstation, four nodes.

## Non-goals

- No `website/docs` page and no site wiring; the example keeps a README.
- No change to `examples/docker-e2e` or the root `docker-compose*.yml`.
- No TLS, no replication factor above one, no backup/restore fixtures.
- Not a production or multi-replica deployment.

## Anti-goals

- A reachability trick that only works from the host (`host.docker.internal`):
  presigned URLs must resolve from the seed and the nodes.
- SQLite, `embedded` search or the `local` object store still active.
- Backend services that start without a healthcheck other services can wait on.

## Constraints

- The object-store endpoint is used for server-side S3 calls *and* for
  presigned URLs handed to clients, so it is the Compose service name
  `http://garage:3900`; `extra_hosts` is not needed.
- Credentials and the Garage `rpc_secret` stay the committed fixture values
  from `packaging/garage.toml` and the HA override.
- `postgres:19beta2-alpine` is a beta. PostgreSQL 18 has no stable 19 tag yet
  (`postgres:19` is unpresent, `19beta1`/`19beta2` are present) and its official
  image changed `PGDATA` to `/var/lib/postgresql/19/docker` with the volume at
  `/var/lib/postgresql`; the volume mount must follow that, not the old
  `/var/lib/postgresql/data`.
- The Garage server is the official `dxflrs/garage:v2.5.0` image, not a derived
  one. The image is a static `/garage` without a shell, so the one-shot
  bootstrap (layout needs the runtime node id) cannot run as a fixed `command`
  in the server container. It runs in a short-lived sidecar that copies the
  same pinned binary into `alpine` and shares the server's network namespace,
  which keeps `rpc_public_addr = "127.0.0.1:3901"` from
  `packaging/garage.toml` valid without forking the config.
- `packaging/garage.toml` is mounted from the repository, not copied. Only the
  provisioning steps (without the server start/wait) are example-local.

## Approaches

1. **Self-contained services in this example (chosen).** Direct, one-command,
   and the nodes keep exercising the real blob path. Cost: the PostgreSQL and
   OpenSearch blocks duplicate `docker-compose.ha.yml`, and Garage needs the
   bootstrap sidecar the static image cannot inline.
2. **Thin overlay that `include`s `../docker-e2e/docker-compose.yml`.** Less
   copying, but subtle build contexts and a shared project namespace; every e2e
   edit leaks in. Rejected.
3. **Only reconfigure the server against host-run backends.** No containers
   added, which contradicts the request. Rejected.

## Implementation outline

- `docker-compose.yml`:
  - `postgres` with `POSTGRES_DB/USER/PASSWORD` fixtures, a `postgres-data`
    volume on `/var/lib/postgresql`, `pg_isready` healthcheck, no host port.
  - `opensearch` as in `docker-compose.ha.yml` (single node, security plugin
    and demo config disabled, memlock/nofile ulimits, `opensearch-data`
    volume, cluster-health check that waits for yellow).
  - `garage` from the official `dxflrs/garage:v2.5.0`, default `server` command
    with `-c /etc/garage.toml`, the `garage-meta`/`garage-data` volumes, the
    read-only `../../packaging/garage.toml`, `/garage … status` healthcheck, no
    host port.
  - `garage-bootstrap` one-shot: `../garage/Dockerfile` copies `/garage` from
    the same pinned image into `alpine`, `network_mode: "service:garage"`, the
    same config mount read-only, bucket/key fixtures, `restart: "no"`; it waits
    for the server, assigns the layout, creates the bucket, imports the key and
    grants it. `seed` waits for it to complete.
  - `microwave` waits for the three backends with `condition: service_healthy`.
- `microwave/microwave.toml`: `database.driver = "postgres"` + URL to the
  `postgres` service, `search.backend = "opensearch"` + `http://opensearch:9200`,
  `object_store.backend = "garage"` + `http://garage:3900`, bucket
  `chef-cookbooks`, region `garage`, `path_style = true`, fixture key pair.
  `data_dir`, MCP, console key and the local data collector stay.
- `README.md`: retitle/repoint from `docker-e2e`, replace the SQLite/embedded/
  local claims, name the three pinned images and the beta status, add the
  backend ports/health and inspection commands (`psql`, `_cluster/health`,
  `garage status`/`bucket list`), and add the backend variables to the table.

Left to the builder: exact healthcheck intervals/retries, whether the image
tags get `*_IMAGE` overrides, and the README prose.

## Verification

1. `docker compose config -q`.
2. `docker compose up -d`, wait for `garage-bootstrap` and `seed` to complete;
   `docker compose ps` shows all three backends healthy.
3. `node-debian` converges `role[e2e_role]` successfully; its cookbook
   files arrive from Garage.
4. `docker compose exec postgres psql -U microwave -c '\dt'` lists the schema;
   OpenSearch `_count` for the `chef` index is non-zero;
   `docker compose exec garage /garage -c /etc/garage.toml bucket list` contains
   `chef-cookbooks`; the API serves.
5. `docker compose down -v --rmi local` (removes the built bootstrap image),
   then remove the three pulled upstream images in the same step; only that
   example's containers, volumes and images go.

If the store itself fails on `19beta2` for a code reason, report it and fall
back to `postgres:18-alpine` only with approval.
