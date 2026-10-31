# Load test

`docker-load/` is a Compose stack that measures how a Microwave server behaves
under a converge load: PostgreSQL, a seeded fixture, the server built from the
repository `Dockerfile`, the `microwave-load` driver and a real `chef-client`
canary. The stack's mechanics live in
[`docker-load/README.md`](../docker-load/README.md) and
[`docker-load/docker-compose.yml`](../docker-load/docker-compose.yml); this
guide says what the numbers mean.

The harness is not a CI gate. It runs by hand, takes as long as the seed and the
converges take, and writes a report for a human to read.

## What it measures, and what it does not

The driver plays `chef-client` over the signed HTTP API: it reads a node and its
environment, solves the run list, fetches every file the solution names, saves
the node and posts the run report, plus a sandbox upload with `--upload`. Each
request is timed, so the report carries a count, a status distribution, the
retry count and the solver-`503` count per endpoint, and p50/p95/p99/max
latencies in microseconds. It also counts node outcomes, polls the `/_stats`
pool gauges, and runs the canary.

A reading describes:

- **one host.** All four containers share one kernel and one bridge network.
  Traffic never leaves the machine, so latency includes bridge overhead but no
  real network round trip. A production fleet has RTT that this stack does not
  model.
- **a fixed cgroup split.** Server CPU and memory and driver CPU and memory are
  separate limits (`MW_SERVER_CPUS`/`MW_SERVER_MEM`,
  `MW_LOAD_CPUS`/`MW_LOAD_MEM`), carved out of the same machine. The numbers
  describe that split, not a machine class.
- **PostgreSQL on the same host**, with the throwaway settings the fingerprint
  records. There is no SQLite run: a report is only comparable to another
  PostgreSQL run with the same fingerprint.
- **the local object store and the embedded search backend.** No S3 gateway and
  no OpenSearch cluster sits between the server and its data, so a remote
  backend looks slower here than the stack can tell.
- **a driver that imitates `chef-client`**, at a concurrency no real fleet of
  clients would produce from one binary. Only the canary runs the real one.

The fixture is fixed so runs stay comparable: 20,000 nodes, 1,000 roles, 600
cookbook versions and 5,000 converge targets in the reference profile. A number
from a smoke profile is not comparable to a reference run.

## Prerequisites

- A Linux host with Docker Engine and Compose v2, a checkout at the commit to
  measure, and roughly 16 GB of RAM for the reference profile.
- Network access during the build for the base images and the Cinc Client
  download, plus free space for the PostgreSQL volume and the cookbook blobs.
- `run.sh` clamps the CPU limits to the host's core count and rejects a
  non-integer pool or connection knob before anything is built.

This stack always uses PostgreSQL. The seed writes rows and cookbook blobs
straight into the throwaway database and the server's data volume, and the
server config is [`docker-load/microwave/microwave.toml`](../docker-load/microwave/microwave.toml),
the measured configuration and the file a bare-metal operator copies.

### Stack knobs

| Knob | Default | Purpose |
|---|---|---|
| `MW_POOL_SIZE` | `10` | `database.pool_size` of the server. Must be an integer. |
| `MW_PG_MAX_CONNECTIONS` | `200` | PostgreSQL `max_connections`. Must be an integer. |
| `MW_PG_SHARED_BUFFERS` | `256MB` | PostgreSQL `shared_buffers`. |
| `MW_SERVER_CPUS` | `2` | CPU limit of the server container; clamped to the host's cores. |
| `MW_SERVER_MEM` | `4g` | Memory limit of the server container. |
| `MW_LOAD_CPUS` | `4` | CPU limit of the driver container; clamped to the host's cores. |
| `MW_LOAD_MEM` | `4g` | Memory limit of the driver container. |
| `MW_LOAD_PORT` | `18081` | Host port the server's `8000` is published on. |
| `MW_SEED_NODES` | `20000` | Nodes the seed creates. |
| `MW_SEED_ROLES` | `1000` | Roles the seed creates. |
| `MW_SEED_VERSIONS` | `600` | Cookbook versions the seed creates. |
| `MW_SEED_TARGETS` | `5000` | Converge targets, one keypair and node each. |
| `MW_SEED_CANARY` | `20` | Canary nodes for the real `chef-client`. |
| `RUST_LOG` | `info` | Server log level. |
| `RUST_VERSION` | `1` | Rust image tag both images build on. |
| `CINC_VERSION` | `19` | Cinc Client version installed in the load image. |

`run.sh` sets the five `MW_SEED_*` values from the chosen profile and exports
them, so an inherited value loses against the profile. Use them only when
starting the services through Compose yourself. `MW_CONFIG`,
`MW_HEALTHCHECK_HOST`, `MW_HTTP_PORT` and the `MW_STATS__*` pair are fixed in
the compose file and must match the mounted configuration.

The resolved pool size, the PostgreSQL settings, the Compose limits and the
image digests are written into the fingerprint, so the report shows the values
the run actually used rather than the ones it was meant to use.

## Running it

```sh
docker-load/run.sh          # reference profile
docker-load/run.sh smoke    # shrunken fixture, for a first look
```

`run.sh` builds both images, seeds, waits for the server health check, writes
the fingerprint, runs the driver, which writes the report into
`docker-load/out/`, and tears the stack down on every exit, also on failure. A
failed run prints the seed and server logs first. The report is
`docker-load/out/load-report-<UTC>.md` with a sibling `.json`; the timestamped
name sorts in time order.

Exit status is diagnostic: `0` for a finished run, also when nodes failed, `2`
for an unreachable server or a broken fixture, `3` for a fingerprint or report
failure.

### Driver flags

`run.sh` passes a fixed set. The flags below matter when starting the services
through Compose yourself or calling the binary directly; `run` is the driver,
`seed` the fixture writer.

| Flag | Command | Default | Meaning |
|---|---|---|---|
| `--server-url` | `seed`, `run` | required | Base URL of the server. |
| `--keys` | `seed`, `run` | required | Manifest written by the seed, read by the run. |
| `--data-dir` | `seed` | required | Data directory, only used without `--postgres-url`. |
| `--postgres-url` | `seed`, `run` | unset | Seed into PostgreSQL; on `run`, measure the PostgreSQL facts. |
| `--nodes` | `seed` | `20000` | Nodes to create. |
| `--roles` | `seed` | `1000` | Roles to create. |
| `--versions` | `seed` | `600` | Cookbook versions to create. |
| `--targets` | `seed` | `5000` | Converge targets to create. |
| `--shared-key` | `seed` | off | One keypair for every actor instead of one each. |
| `--canary` | `seed`, `run` | `20` | Canary nodes to create; `0` runs none. |
| `--out` | `run` | `.` | Report directory. |
| `--concurrency` | `run` | `5000` | Concurrent converge sequences. |
| `--ramp` | `run` | `0` | Seconds over which node starts are spread. |
| `--chef-client` | `run` | `chef-client` | Binary the canary executes. |
| `--upload` | `run` | off | Upload one blob per sequence through a presigned URL. |
| `--stats-user`, `--stats-pass` | `run` | unset | Credentials for the `/_stats` pool poller. |
| `--superuser` | `run` | off | Sign every request as the manifest's superuser. |
| `--fingerprint` | `run` | unset | Wrapper fingerprint JSON to merge under the measured facts. |
| `--commit` | `run` | unset | Server commit printed into the report. |

## Reading the fingerprint

The report opens with the fingerprint and the resolved configuration, before
the first table. A latency without the machine that produced it is not
comparable to anything, so the order is deliberate.

Measured fields are `cpu_model`, `cores`, `memory_total_bytes`, `kernel`,
`out_filesystem`, `out_free_bytes`, `cgroup_cpu_max`, `cgroup_memory_max`, the
PostgreSQL version with `max_connections` and `shared_buffers`, `server_commit`
and `pool_size`. `pool_size` is the last maximum the `/_stats` poller saw — the
pool the server really ran with, not the one the configuration asked for.

The wrapper adds what the driver cannot see from inside its container:
`host_cores`, `host_cpu_model`, `host_memory_total_bytes`, `host_kernel`,
`compose_limits` and the `image_digests`. `cores` is the container's cgroup
view, so read `host_cores` for the machine and `cgroup_cpu_max` /
`cgroup_memory_max` for the share it was allowed.

`unknown` or a missing field means the probe was unavailable, not that the fact
was zero. A malformed `--fingerprint` file stops the run with exit `3` instead
of leaving a thinner report behind.

## Comparing two runs

Compare only runs with the same fingerprint: same CPU model and core count,
same memory, same kernel, same cgroup limits, same PostgreSQL settings and
version, same pool size, same image digests and same server commit. Diff the
JSON siblings for that; the Markdown is for reading.

Change one variable per pair and keep `--concurrency`, `--ramp` and the fixture
identical. A second variable in the same step makes the difference
unattributable. Compare the same endpoint's p50, p95 and p99, the failure
counts and the `solver 503s` column, not a single aggregate.

Do not compare across `--shared-key` modes: the key material and the signing
cost differ.

## Testing another pool size

The defaults are `MW_POOL_SIZE=10`, `MW_PG_MAX_CONNECTIONS=200` and
`MW_PG_SHARED_BUFFERS=256MB`. Raising the pool without raising PostgreSQL's
connection budget hands the extra connections a refusal.

1. Run the default profile and keep its report as the baseline.
2. Raise `MW_POOL_SIZE` and `MW_PG_MAX_CONNECTIONS` in the same step, with
   `max_connections` above the pool size plus the other clients (the
   fingerprint connection and the `/_stats` poller). Keep the pool below
   `max_connections`.
3. Run again and compare the pool occupancy with the baseline. A `max` that
   grew and an `in_use` spread that stays at that maximum means the pool was
   the constraint; a latency that does not move means the bottleneck sits
   elsewhere and the pool change bought nothing.
4. Keep both values in the report and compare the pair only against runs with
   the same pair. The fingerprint records both.

`shared_buffers` is a separate variable: change it only together with the
memory allotted to PostgreSQL and never in the same step as the pool size.

## The canary

The seed creates canary nodes next to the targets. After the driver, the load
container converges them with the real `chef-client`, at most four at a time
and with a ten-minute timeout each. The report's canary section carries one
`canary-<n>: ok|failed` line per node, with the stderr tail under a failure. A
run without a `chef-client` binary or without canary entries says so instead.

A canary failure is reported but never fails the run. That is the point: the
driver is an imitation, and a green driver run next to a failing real converge
means the driver drifted from `chef-client`, not that the server is broken.
Treat the canary as a fidelity check on the harness before trusting a
throughput comparison.

## Signing as the superuser

`--superuser` signs every driver request with the manifest's superuser entry
instead of each node's client. The driver then exercises no per-node
authorization: ACL checks only happen in the canary. The report states this
under the node outcomes. The manifest must carry a superuser entry, or the run
stops as a fixture error.

Use it to separate authorization cost from throughput, not as an ACL
benchmark, and compare it only against another superuser run.

## Solver 503s

The stack's configuration sets `depsolver.max_concurrent = 4`, the default
described in the [deployment guide](deployment.md#search-and-dependency-solver).
A solve beyond the four permits answers `503 Dependency solver overloaded`. The
driver retries a `503` with the `chef-client` budget — five retries, exponential
backoff with jitter — so the node usually still completes, with the retry and
the `solver 503` counted in its endpoint row and slower than a first-try answer.

A non-zero `solver 503s` column is therefore normal at high concurrency and not a
defect, as long as the retries recover it. A run that exhausts the budget stops
that node at the solve and lands it in the failures. To reduce the noise, edit
`docker-load/microwave/microwave.toml` or set
`MW_DEPSOLVER__MAX_CONCURRENT` in the same step, and compare only against runs
with the same value.

## Where the evidence is

- [`docker-load/README.md`](../docker-load/README.md) — the stack in brief.
- [`docker-load/docker-compose.yml`](../docker-load/docker-compose.yml) — the
  services, limits and mounts.
- [`crates/microwave-load/src/cli.rs`](../crates/microwave-load/src/cli.rs) —
  every flag and its default.
- [`crates/microwave-load/src/fingerprint.rs`](../crates/microwave-load/src/fingerprint.rs)
  and [`crates/microwave-load/src/report.rs`](../crates/microwave-load/src/report.rs)
  — the fingerprint keys and the report layout.
