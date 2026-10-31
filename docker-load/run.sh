#!/usr/bin/env bash
#
# Run the converge load test as one Compose stack and tear it down again.
#
#   ./run.sh          # reference profile
#   ./run.sh smoke    # shrunken fixture, for a first look
#
# The stack builds both images, seeds PostgreSQL and the cookbook blobs, waits
# for the server, writes the host fingerprint, runs the driver and copies the
# report to ./out. Containers, volumes and the locally built images are
# removed on every exit, also on failure.
set -euo pipefail

cd "$(dirname "$0")"

usage() {
    echo "usage: $0 [run|smoke]" >&2
    exit 2
}

profile=${1:-run}
case "$profile" in
    run)
        nodes=20000 roles=1000 versions=600 targets=5000
        concurrency=5000 canary=20
        ;;
    smoke)
        nodes=500 roles=50 versions=60 targets=200
        concurrency=200 canary=2
        ;;
    *) usage ;;
esac
[ "$#" -le 1 ] || usage

export MW_SEED_NODES=$nodes MW_SEED_ROLES=$roles MW_SEED_VERSIONS=$versions
export MW_SEED_TARGETS=$targets MW_SEED_CANARY=$canary

# A cgroup CPU limit above the daemon's core count is rejected, so the defaults
# (2 for the server, 4 for the load side) are clamped to what the host offers.
# An explicit MW_SERVER_CPUS/MW_LOAD_CPUS is clamped as well.
available_cpus=$(docker info --format '{{.NCPU}}' 2>/dev/null || echo 1)
clamp_cpus() {
    awk -v want="$1" -v max="$available_cpus" \
        'BEGIN { v = want + 0; if (v < 1) v = 1; if (v > max) v = max; printf "%s", v }'
}
MW_SERVER_CPUS=$(clamp_cpus "${MW_SERVER_CPUS:-2}")
MW_LOAD_CPUS=$(clamp_cpus "${MW_LOAD_CPUS:-4}")
export MW_SERVER_CPUS MW_LOAD_CPUS

# The fingerprint carries these as JSON numbers, so reject anything else before
# the stack is built instead of writing a fingerprint the driver cannot read.
require_int() {
    case "$2" in
        ''|*[!0-9]*) echo "$1 must be a positive integer, got '$2'" >&2; exit 2 ;;
    esac
}
pg_max_connections=${MW_PG_MAX_CONNECTIONS:-200}
pool_size=${MW_POOL_SIZE:-10}
require_int MW_PG_MAX_CONNECTIONS "$pg_max_connections"
require_int MW_POOL_SIZE "$pool_size"

compose() { docker compose --profile "$profile" "$@"; }

# ./keys and ./out are bind-mounted at /keys and /out, so `--out /out` writes
# the report into this directory and the printed host path is the same file.
# A previous run's manifest is replaced by the seed; stale reports only pile up.
rm -rf keys out
mkdir -p keys out

fingerprint=out/host-fingerprint.json

cleanup() {
    local status=$?
    if [ "$status" -ne 0 ]; then
        echo "--- seed logs ---" >&2
        compose logs --no-color --tail 40 seed >&2 || true
        echo "--- microwave logs ---" >&2
        compose logs --no-color --tail 40 microwave >&2 || true
    fi
    # The bind mounts were written as root; hand them back to the caller so
    # the next run can replace them without sudo.
    compose run --rm --no-deps --entrypoint sh load -c \
        "chown -R $(id -u):$(id -g) /keys /out" >/dev/null 2>&1 || true
    compose down -v --rmi local || true
    exit "$status"
}
trap cleanup EXIT

compose build

# All three in one operation: `seed` is a one-shot and the server is gated on
# it having exited successfully. A second `up` would run the seed again and the
# fixed fixture ids would collide with the populated database.
compose up -d postgres seed microwave

# Wait until `service` is ready. `--health` requires its probe to report
# `healthy` (postgres, microwave); without it a running container is enough, and
# for the one-shot seed a clean exit. `limit` bounds the wait in seconds.
wait_for() {
    local health=0
    if [ "$1" = "--health" ]; then
        health=1
        shift
    fi
    local service=$1 limit=$2 id status
    for _ in $(seq 1 "$limit"); do
        # `ps` alone only lists running containers; the completed one-shot seed
        # is already exited here.
        id=$(compose ps -aq "$service")
        if [ -n "$id" ]; then
            status=$(docker inspect --format '{{.State.Status}}' "$id")
            if [ "$status" = "exited" ]; then
                local code
                code=$(docker inspect --format '{{.State.ExitCode}}' "$id")
                echo "$service exited with $code" >&2
                return "$code"
            fi
            if [ "$status" = "running" ]; then
                [ "$health" -eq 0 ] && return 0
                status=$(docker inspect --format '{{.State.Health.Status}}' "$id" 2>/dev/null || true)
                [ "$status" = "healthy" ] && return 0
            fi
        fi
        sleep 1
    done
    echo "$service did not become ready in ${limit}s" >&2
    return 1
}

# The reference seed generates one keypair per target, so give it an hour.
wait_for --health postgres 120
wait_for seed 3600
wait_for --health microwave 120

# The facts the driver cannot measure from inside its container: the host, the
# image digests and the resolved Compose limits.
cores=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 0)

# Linux first; the sysctl fallbacks cover a macOS Docker host.
cpu_model() {
    local model
    model=$(awk -F': ' '/^model name/{print $2; exit}' /proc/cpuinfo 2>/dev/null || true)
    [ -n "$model" ] || model=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)
    printf '%s' "${model:-unknown}"
}
host_memory() {
    local bytes
    bytes=$(awk '/^MemTotal:/{printf "%d", $2 * 1024}' /proc/meminfo 2>/dev/null || true)
    [ -n "$bytes" ] || bytes=$(sysctl -n hw.memsize 2>/dev/null || true)
    printf '%s' "${bytes:-0}"
}
model=$(cpu_model)
memory=$(host_memory)
kernel=$(uname -a)

digests=""
for image in $(compose config --images | sort -u); do
    digest=$(docker image inspect --format \
        '{{if .RepoDigests}}{{index .RepoDigests 0}}{{else}}{{.Id}}{{end}}' "$image")
    digests="$digests$digest"$'\n'
done

{
    printf '{\n'
    printf '  "host_cores": %s,\n' "$cores"
    printf '  "host_cpu_model": "%s",\n' "$(printf '%s' "$model" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
    printf '  "host_memory_total_bytes": %s,\n' "$memory"
    printf '  "host_kernel": "%s",\n' "$(printf '%s' "$kernel" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
    printf '  "postgres_max_connections": %s,\n' "$pg_max_connections"
    printf '  "postgres_shared_buffers": "%s",\n' "${MW_PG_SHARED_BUFFERS:-256MB}"
    printf '  "pool_size": %s,\n' "$pool_size"
    printf '  "compose_limits": {\n'
    printf '    "server_cpus": "%s",\n' "${MW_SERVER_CPUS:-2}"
    printf '    "server_mem": "%s",\n' "${MW_SERVER_MEM:-4g}"
    printf '    "load_cpus": "%s",\n' "${MW_LOAD_CPUS:-4}"
    printf '    "load_mem": "%s"\n' "${MW_LOAD_MEM:-4g}"
    printf '  },\n'
    printf '  "image_digests": [\n'
    printf '%s' "$digests" | sed -e 's/.*/    "&",/' -e '$ s/,$//'
    printf '  ]\n'
    printf '}\n'
} > "$fingerprint"

# --no-deps: the dependencies already ran above and `run` would restart the
# one-shot seed, which then collides with its own fixture.
compose run --rm --no-deps load run \
    --server-url http://microwave:8000 \
    --keys /keys/load.keys \
    --out /out \
    --postgres-url postgres://load:load@postgres:5432/load \
    --fingerprint /out/host-fingerprint.json \
    --commit "$(git rev-parse --short HEAD)" \
    --concurrency "$concurrency" \
    --canary "$canary" \
    --stats-user load \
    --stats-pass load-stats

# The report name starts with its UTC timestamp, so glob order is time order.
reports=(out/load-report-*.md)
if [ -e "${reports[0]}" ]; then
    echo "report: $PWD/${reports[${#reports[@]} - 1]}"
fi
