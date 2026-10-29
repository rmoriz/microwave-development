#!/usr/bin/env bash
#
# Run the microwave-store-postgres integration tests against a disposable
# PostgreSQL container.
#
# Usage:
#   scripts/run-postgres-tests.sh [--check-url] [extra cargo test args...]
#
#   --check-url   print the resolved URL (password redacted) and exit 0 without
#                 connecting.
#
# Environment:
#   MICROWAVE_TEST_POSTGRES_URL
#                              Use this database instead of starting a
#                              container. The variable is passed to the tests
#                              unchanged.
#   MICROWAVE_POSTGRES_IMAGE   Image to use (default: postgres:18-alpine)
#   MICROWAVE_KEEP_POSTGRES    Set to 1 to keep the container after the run
#   MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS
#                              Set to 1 to explicitly skip CLI backup/restore/
#                              psql coverage when host pg_dump/pg_restore/psql
#                              are unavailable. Without it, missing tools fail.
#   MICROWAVE_REQUIRE_POSTGRES Set to 1 or true to turn the Docker SKIP paths
#                              into hard failures. The script also exports this
#                              for the test processes. An explicit
#                              MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS=1 remains an
#                              intentional opt-out of only the CLI-tool check.
#
# The script exits 0 with a SKIP message when Docker or its daemon is not
# available, so CI without Docker stays green unless MICROWAVE_REQUIRE_POSTGRES
# is 1/true, in which case the same conditions fail. The tests themselves also
# skip cleanly when MICROWAVE_TEST_POSTGRES_URL is unset.

set -euo pipefail

IMAGE="${MICROWAVE_POSTGRES_IMAGE:-postgres:18-alpine}"
CONTAINER="microwave-postgres-test-$$"
KEEP="${MICROWAVE_KEEP_POSTGRES:-0}"

CHECK_URL=0
ARGS=()
for arg in "$@"; do
    case "$arg" in
        --check-url) CHECK_URL=1 ;;
        *) ARGS+=("$arg") ;;
    esac
done

redact_url() {
    printf '%s\n' "$1" | sed -E 's#^([^:]+://[^:/@]+):[^@]*@#\1:***@#'
}

# A supplied URL means the caller (CI's service container, a remote box) owns
# the lifecycle; only the self-provisioned path may start or remove a container.
SUPPLIED_URL="${MICROWAVE_TEST_POSTGRES_URL:-}"

if [[ "$CHECK_URL" == "1" ]]; then
    if [[ -z "$SUPPLIED_URL" ]]; then
        echo "MICROWAVE_TEST_POSTGRES_URL is unset; the script would provision a container" >&2
        exit 1
    fi
    redact_url "$SUPPLIED_URL"
    exit 0
fi

skip() {
    case "${MICROWAVE_REQUIRE_POSTGRES:-0}" in
        1|[Tt][Rr][Uu][Ee])
            echo "ERROR: $* (MICROWAVE_REQUIRE_POSTGRES requires PostgreSQL)" >&2
            exit 1
            ;;
    esac
    echo "SKIP: $*" >&2
    exit 0
}

cleanup() {
    [[ "${OWN_CONTAINER:-0}" == "1" ]] || return 0
    if [[ "$KEEP" == "1" ]]; then
        echo "KEEP: leaving container $CONTAINER running" >&2
        return
    fi
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
}
trap cleanup EXIT

OWN_CONTAINER=0
if [[ -n "$SUPPLIED_URL" ]]; then
    echo "using supplied MICROWAVE_TEST_POSTGRES_URL" >&2
    export MICROWAVE_TEST_POSTGRES_URL="$SUPPLIED_URL"
fi

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

if [[ -z "$SUPPLIED_URL" ]]; then
    command -v docker >/dev/null 2>&1 || skip "docker is not installed"
    docker info >/dev/null 2>&1 || skip "the Docker daemon is not reachable"
    OWN_CONTAINER=1

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
fi

# CI gate: the parity matrix must not silently fall back to SQLite-only when a
# PostgreSQL URL was supplied or provisioned above.
export MICROWAVE_REQUIRE_POSTGRES=1

cargo test -p microwave-store-postgres ${ARGS[@]+"${ARGS[@]}"}
# The same contract suite runs against SQLite (always) and PostgreSQL (via the
# environment above), so backend parity is part of the PostgreSQL gate.
cargo test -p microwave-store --test dual_backend ${ARGS[@]+"${ARGS[@]}"}
# The API runtime tests connect through AppState and the real HTTP router, so
# they must run with the same disposable database.
cargo test -p microwave-api --test postgres_runtime_test ${ARGS[@]+"${ARGS[@]}"}
cargo test -p microwave-api --test postgres_http_coverage_test ${ARGS[@]+"${ARGS[@]}"}
# The HTTP parity matrix re-runs the same signed scenarios against SQLite and
# PostgreSQL and compares status, headers and normalized bodies.
cargo test -p microwave-api --test http_parity_test ${ARGS[@]+"${ARGS[@]}"}
# The knife backup importer has a PostgreSQL-gated half in its library test;
# run it explicitly because the crate is not part of the store matrix above.
cargo test -p microwave-migrate --test knife_backup_test ${ARGS[@]+"${ARGS[@]}"}
# Operational CLI and snapshot tests run after the store suite because the
# maintenance-flag assertions are global to the shared database.
cargo test -p microwave-ctl-bin --test cli_test ${ARGS[@]+"${ARGS[@]}"}
cargo test -p microwave-migrate-bin --test cli_test ${ARGS[@]+"${ARGS[@]}"}
