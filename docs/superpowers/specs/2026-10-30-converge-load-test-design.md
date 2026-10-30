# Converge load test — design

**Date:** 2026-10-30
**Status:** approved; implementation plan in
`docs/superpowers/plans/2026-10-30-converge-load-test.md`

## Intent

The compatibility record proves correctness at small scale: 652 core pedant
examples and a four-distro converge. Nothing helps an operator size hardware
for the shape they actually run, and three server-side limits are known from
reading the code rather than from a measurement:

- the PostgreSQL store hard-wires `max_connections(10)`;
- `depsolver.max_concurrent` defaults to `4` and rejects the fifth concurrent
  solve instead of queueing;
- every solve rebuilds the whole cookbook world for the organization from a
  full version scan and parses each manifest.

This change makes the pool size an operator setting, moves the two cheap
inputs of a solve to write time, and adds a load test that converges 5000
nodes at once over a 20,000-node, 1,000-role dataset so an operator can
measure their own hardware instead of guessing from code.

## Goals

- `database.pool_size` / `MW_DATABASE__POOL_SIZE`, default `10`, rejected when
  `0`, resolved wherever `database.url` is, reported by `check-config`, shown
  in `/_stats` as pool gauges.
- Cookbook-version dependencies and environment constraints are derived and
  validated on write, so the solver no longer parses manifests or environment
  JSON per request.
- `microwave-load` drives 5000 concurrent converge request sequences against a
  20,000-node, 1,000-role, 600-version fixture and reports per-endpoint
  latency percentiles, status counts, retries, and solver `503`s separately.
- A canary of real `chef-client` runs alongside the driver, reported on its
  own line, so a fidelity bug in the driver is visible.
- Every run writes a report that carries the measured environment beside the
  resolved server configuration: host cores and model, RAM, kernel, filesystem,
  PostgreSQL version and its `max_connections`/`shared_buffers`, the server
  commit, plus the Compose image digests and the cgroup CPU/memory limits the
  run actually used.
- The whole run is packaged as a Compose stack — server, PostgreSQL, driver —
  so a report cannot be separated from the environment that produced it.
- `docs/load-test.md` explains how to run the harness and how to read the
  report.

## Non-goals

- **No CI/CD gate.** The harness is not part of any workflow and no threshold
  makes it red. It exists so an operator can size hardware.
- No committed reference numbers. The report is a product of a run on a
  particular machine, not a number this repository claims.
- No bare-metal parity. Bridge networking and cgroup limits are part of what
  the numbers describe, and the report says so.
- No world cache and no per-node run-list expansion memo. Both are follow-ups;
  this change measures the uncached baseline they would move.
- No change to the `depsolver.max_concurrent` default of `4`. Solver `503`s
  are expected noise and get their own counter.
- No HA, replica, OpenSearch, S3, TLS-listener, or Data Collector scenarios.
- No multi-organization fixture.

## Anti-goals

- A number that looks good because the fixture is thin: 600 cookbook versions
  are mandatory, not optional.
- `503`s folded into an error rate, or retries counted as success without
  being reported.
- A report without the host fingerprint beside the results.
- Numbers quoted without the cgroup limits they were produced under.
- Latency thresholds that fail a run. Percentiles are printed, never enforced.

## Constraints

- The driver must use the real signed request sequence, in `chef-client`'s
  order, each signed `X-Ops-Sign` 1.3: node read, environment read,
  `cookbook_versions` solve, cookbook manifest and file fetches, node save,
  run report. (`chef-client` downloads files and does not sandbox-upload on a
  converge; `--upload` adds an explicit blob PUT for bookshelf write load.)
- Each virtual node signs as its own client, with its own key and its own ACLs,
  and the fixture carries the real authz graph: per-node Update for the owning
  client, Read on the environment, cookbook container, and sandbox. A
  superuser-signed `--superuser` mode exists for cheap runs but is not the
  default.
- Seeding 21,000 objects cannot go through the API: it builds a snapshot and
  applies it through the store, the way `microwave-migrate` does.
- `chef-client` retries any 5xx five times with jittered exponential backoff.
  The driver mirrors that, or the measured failure rate is wrong.
- The reference run needs PostgreSQL. SQLite serialises every request behind
  one mutexed connection, so a SQLite run measures the mutex, not the server.
- The stack is built and torn down through its own Compose file, never an
  ad-hoc `docker build`, and every run removes its volumes, images, and
  containers in the same step, as the repository's docker rules require.

## Approaches considered

### Pool size

1. Fixed constants raised to 32 — one-line, but no operator control and no way
   to match a smaller PostgreSQL `max_connections`. Rejected.
2. `database.pool_size` with the existing config plumbing. **Chosen.**
3. Derive the pool from core count automatically — convenient, but surprises
   operators whose PostgreSQL is shared, and it cannot be reasoned about from
   the config file.

### Harness

1. 5000 real `chef-client` containers. Highest fidelity, roughly a terabyte of
   RAM, unusable outside a dedicated host.
2. A Rust signing driver. One host, but it is a model of a client and a wrong
   assumption stays invisible.
3. Driver plus a canary of real clients. **Chosen:** the canary anchors
   fidelity at ~2% of option 1's cost, and the driver scales.
4. k6/vegeta with pre-signed requests. Dropped: v1.3 signing is RSA-SHA256
   over a per-request canonical string with per-node keys, so it means JS
   crypto over 5000 keys and a second copy of canonicalisation to keep
   correct.

### Where the solve gets cheaper

1. Per-organization `World` cache, invalidated on cookbook writes. Removes the
   query but adds invalidation, replica divergence, and a miss storm after
   every upload.
2. Write-time derivation of dependencies and environment constraints.
   **Chosen** for cookbooks and environments: both are immutable or
   near-immutable properties of the object, so there is nothing to invalidate,
   and upstream already stores `cookbook_versions.meta_deps` this way.
3. Both. Deferred — the test measures the baseline first.

Role-level pre-expansion was considered and dropped: expansion depends on the
environment and on nested roles, so it needs a per-environment key and a
child-role fan-out. Node-level pre-expansion was dropped because the client
requests the solve *before* it saves its node, so the write arrives too late.

## How it works

### Pool size

`DatabaseConfig` gains `pool_size: usize` (serde default `10`), env override
`MW_DATABASE__POOL_SIZE`, rejected when `0` by `validate()` beside the other
zero checks. `PostgresStore` gains
`connect_with_compression_and_pool(database_url, compression, pool_size)`;
`connect_with_compression` forwards the default so existing callers and tests
are unchanged. The server and `microwave-ctl` pass the configured value; the
migrator takes no config and keeps the default. `check-config` prints the
resolved size. The settings table,
the example configuration, and the deployment guide get the row, including the
per-process-multiplies-against-`max_connections` warning.

`/_stats` gains `microwave_server_db_pool_connections{state="idle"|"in_use"}`
and `_pool_connections_max`, sampled from `sqlx::PgPool`. That needs a
`MetadataStore::pool_stats()` returning `Option<PoolStats>` (default `None`,
PostgreSQL override), because the server only holds the trait object. Without
it, a pool-bound run is indistinguishable from a slow store.

### Derivations on write

A `meta_deps` value on `cookbook_versions` (the column already exists in both
backends and in snapshots, hard-coded to `'{}'`) and a new
`cookbook_constraints` column on environments, both JSON. Both write paths
already validate their constraint maps, so the derivation only re-serializes
them. Environment constraints are written on create and update; an unparsable
constraint still fails the upload with the usual `400` instead of failing 5000
converges. Both are carried in the snapshot rows (`meta_deps` already is), and
`seed` populates them through the same shared helpers rather than leaving them
empty — otherwise the reference run would measure the old parse-per-request
behaviour. Reads fall back to parsing when the column is empty, `'{}'` or
`NULL`, so existing databases and older snapshots keep working.
`cookbook_world` and `environment_constraints` read the stored form and never
parse the object when one is present.

### The stack

`docker-load/docker-compose.yml`, separate from `docker-e2e` so the systemd
node stack stays untouched:

- `postgres` — pinned image, named volume, `max_connections` and
  `shared_buffers` set from the environment and recorded.
- `microwave` — built from the repository `Dockerfile`, bound to the service
  name so presigned `/bookshelf` URLs resolve for the driver, with
  `data_collector.store = local` and the `database.pool_size` under test.
- `load` — the driver image, gated on the server healthcheck, running `seed`
  then `run`, writing the report into a mounted output directory. It needs
  `chef-client` in the image only when the canary is enabled.

`deploy.resources.limits.cpus` and `mem_limit` are pinned per service, so the
split between client and server is an explicit, recorded decision instead of
whatever the scheduler left over. `profiles: smoke` shrinks the fixture for
laptops.

Bridge networking and a shared kernel mean the numbers describe this
containerised pair on this host — not bare metal, and not two physical
machines. Linux as the host is recommended because a Docker Desktop VM adds a
layer the report cannot see into.

The driver binary stays runnable outside Compose with the same flags, for
anyone who wants to point it at a real server and PostgreSQL.

### Harness

`crates/microwave-load` plus `bin/microwave-load`:

- `seed` builds a `StoreSnapshot` — 20,000 nodes, 1,000 roles, 600 cookbook
  versions across 200 cookbooks, one organization, three environments (the
  default plus two with `cookbook_versions` constraints), 5,000 registered
  target nodes each owned by a client with a pre-generated key — applies it
  through the store, writes the cookbook blobs into the object store, and
  writes a `0600` key manifest that `run` and the canary read with `--keys`.
  Ids and names are deterministic, so a second run reuses the fixture. RSA key
  generation is parallel and bounded; `--shared-key` trades realism for speed
  in smoke runs.
- Topology: every target node carries three roles drawn from the pool, roles
  nest at most two deep and resolve five cookbook packages, so a solve
  produces a real graph instead of one leaf.
- `run` executes the sequence per node with `--concurrency` and `--ramp`
  (seconds over which node starts are spread linearly; `0` releases all at
  once), retrying 5xx exactly like `chef-client`, and records per-endpoint
  `p50/p95/p99/max`, status counts, retry counts, solver `503`s, and sampled
  pool gauges. The first read of a target is `200` and the save is a `PUT`,
  because targets are pre-registered — this is the re-converge steady state,
  not a registration run.
- The harness writes the server configuration it measures: `data_collector.store = local`
  so run reports are persisted, embedded search, the chosen `database.pool_size`,
  and the `/_stats` credentials. Without `local` the run report step is a
  no-op.
- `--canary N` runs N real `chef-client` processes, named `canary-<n>` so they
  cannot collide with driver targets, over the same fixture, and prints their
  results on a separate line; `--canary 0` disables it. The client binary
  comes from `PATH` or `--chef-client`.
- The report is written to `load-report-<UTC timestamp>.md` plus a sibling
  JSON, both under `--out` (default `.`). The Markdown carries the host
  fingerprint and resolved configuration above the tables; the JSON is there
  for diffing two runs later.
- Exit status is informational, never a gate: `0` when the run completed and
  every converge finished, `2` on a server or fixture failure, `3` on a
  diagnostic program failure. A run whose retries were exhausted on real
  errors still exits `0` and says so in the report.
- Percentiles are printed per endpoint; no threshold is evaluated.

## Decisions taken here

- The harness is a standalone tool, not a CI gate; exit status is diagnostic
  only, and no latency threshold is enforced.
- The run is packaged as a Compose stack under `docker-load/`, with pinned
  cgroup limits, rather than a bare-metal procedure.
- Every report carries the host fingerprint, the cgroup limits, and the image
  digests, because a number without its environment is useless.
- Pool default stays `10`; the documented procedure compares default against
  raised on the same fixture by hand.
- Solver `503`s are expected noise with their own counter.
- Canary defaults to `20` and signs as its own real clients; a canary failure
  is reported prominently but does not fail the run.
- Fixture sizes are fixed defaults (20,000 nodes, 1,000 roles, 600 versions);
  the flags may only reduce them for smoke runs.
- Seed is idempotent and deterministic so the repeat comparison runs on one
  fixture twice.
- One organization, three environments, three roles per node, nest depth two,
  five packages per solve.

## Left to the builder

- Exact percentile algorithm and report layout.
- How many canary clients are sensible on a given host, and how the tool
  degrades when `chef-client` is absent.
- Whether the pool gauges need a sample interval flag or the existing
  `/_stats` poll cadence is enough.
- The wording of `docs/load-test.md`, including what a reader should compare
  between two runs.
