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
#
# The script exits 0 with a SKIP message when Docker or its daemon is not
# available, so CI without Docker stays green. The tests themselves also skip
# cleanly when MICROWAVE_TEST_POSTGRES_URL is unset.

set -euo pipefail

IMAGE="${MICROWAVE_POSTGRES_IMAGE:-postgres:18-alpine}"
CONTAINER="microwave-postgres-test-$$"
KEEP="${MICROWAVE_KEEP_POSTGRES:-0}"

skip() {
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
echo "PostgreSQL ready at 127.0.0.1:${PORT}; running tests..." >&2

cargo test -p microwave-store-postgres "$@"
# The API runtime test connects through AppState and the real HTTP router, so
# it must run with the same disposable database.
cargo test -p microwave-api --test postgres_runtime_test "$@"
