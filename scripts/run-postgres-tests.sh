#!/usr/bin/env bash
#
# Run the microwave-store-postgres integration tests against a disposable
# PostgreSQL container.
#
# Usage:
#   scripts/run-postgres-tests.sh [extra cargo test args...]
#
# Environment:
#   MICROWAVE_POSTGRES_IMAGE   Image to use (default: postgres:18-alpine)
#   MICROWAVE_KEEP_POSTGRES    Set to 1 to keep the container after the run
#   MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS
#                              Set to 1 to explicitly skip CLI backup/restore/
#                              psql coverage when host pg_dump/pg_restore/psql
#                              are unavailable. Without it, missing tools fail.
#   MICROWAVE_REQUIRE_POSTGRES Set to 1 to turn the Docker/client-tool
#                              SKIP paths into hard failures. The script also
#                              exports this for the test processes.
#
# The script exits 0 with a SKIP message when Docker or its daemon is not
# available, so CI without Docker stays green unless MICROWAVE_REQUIRE_POSTGRES=1
# is set, in which case the same conditions fail. The tests themselves also
# skip cleanly when MICROWAVE_TEST_POSTGRES_URL is unset.

set -euo pipefail

IMAGE="${MICROWAVE_POSTGRES_IMAGE:-postgres:18-alpine}"
CONTAINER="microwave-postgres-test-$$"
KEEP="${MICROWAVE_KEEP_POSTGRES:-0}"

skip() {
    if [[ "${MICROWAVE_REQUIRE_POSTGRES:-0}" == "1" ]]; then
        echo "ERROR: $* (MICROWAVE_REQUIRE_POSTGRES=1 requires PostgreSQL)" >&2
        exit 1
    fi
    echo "SKIP: $*" >&2
    exit 0
}

cleanup() {
    if [[ "$KEEP" == "1" ]]; then
        echo "KEEP: leaving container $CONTAINER running" >&2
        return
    fi
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
}
trap cleanup EXIT

command -v docker >/dev/null 2>&1 || skip "docker is not installed"
docker info >/dev/null 2>&1 || skip "the Docker daemon is not reachable"

missing_client_tools=()
for tool in pg_dump pg_restore psql; do
    command -v "$tool" >/dev/null 2>&1 || missing_client_tools+=("$tool")
done
if [[ ${#missing_client_tools[@]} -gt 0 ]]; then
    if [[ "${MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS:-0}" == "1" ]]; then
        echo "WARNING: missing PostgreSQL client tools (${missing_client_tools[*]});" \
             "backup/restore/psql CLI coverage will be skipped because" \
             "MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS=1 was set explicitly." >&2
    else
        echo "ERROR: missing PostgreSQL client tools: ${missing_client_tools[*]}" >&2
        echo "Install pg_dump/pg_restore/psql, or set" \
             "MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS=1 to skip that CLI coverage explicitly." >&2
        exit 1
    fi
fi

echo "Starting disposable PostgreSQL ($IMAGE)..." >&2
docker run --detach \
    --name "$CONTAINER" \
    --env POSTGRES_USER=microwave \
    --env POSTGRES_PASSWORD=microwave \
    --env POSTGRES_DB=microwave_test \
    --publish 127.0.0.1::5432 \
    "$IMAGE" >/dev/null

PORT=""
for _ in $(seq 1 60); do
    if docker exec "$CONTAINER" pg_isready -U microwave -d microwave_test >/dev/null 2>&1; then
        PORT="$(docker port "$CONTAINER" 5432/tcp | head -n1 | awk -F: '{print $NF}')"
        [[ -n "$PORT" ]] && break
    fi
    sleep 1
done

if [[ -z "$PORT" ]]; then
    echo "ERROR: PostgreSQL did not become ready within 60 seconds" >&2
    docker logs "$CONTAINER" >&2 || true
    exit 1
fi

export MICROWAVE_TEST_POSTGRES_URL="postgres://microwave:microwave@127.0.0.1:${PORT}/microwave_test"
# CI gate: the parity matrix must not silently fall back to SQLite-only when
# this script has already provisioned PostgreSQL.
export MICROWAVE_REQUIRE_POSTGRES=1
echo "PostgreSQL ready at 127.0.0.1:${PORT}; running tests..." >&2

cargo test -p microwave-store-postgres "$@"
# The same contract suite runs against SQLite (always) and PostgreSQL (via the
# environment above), so backend parity is part of the PostgreSQL gate.
cargo test -p microwave-store --test dual_backend "$@"
# The API runtime tests connect through AppState and the real HTTP router, so
# they must run with the same disposable database.
cargo test -p microwave-api --test postgres_runtime_test "$@"
cargo test -p microwave-api --test postgres_http_coverage_test "$@"
# The HTTP parity matrix re-runs the same signed scenarios against SQLite and
# PostgreSQL and compares status, headers and normalized bodies.
cargo test -p microwave-api --test http_parity_test "$@"
# Operational CLI and snapshot tests run after the store suite because the
# maintenance-flag assertions are global to the shared database.
cargo test -p microwave-ctl-bin --test cli_test "$@"
cargo test -p microwave-migrate-bin --test cli_test "$@"
