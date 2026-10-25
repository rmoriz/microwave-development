#!/usr/bin/env bash
#
# Run the vendored oc-chef-pedant suite against a locally started
# microwave-server.
#
# Each requested tag runs against its own fresh server, data directory, and
# seeded actors.  Pedant deletes the requestor clients during its after-suite
# hook even when they were pre-existing, so sharing one instance between tags
# produces bogus 401s after the first tag.
#
# Usage:
#   scripts/run-pedant.sh [TAG ...]
#
# Default tags: headers users organizations clients nodes
#
# Tag sets:
#   core                 headers users organizations clients nodes (the default)
#   extended             keys acls data_bags cookbooks sandboxes search policies
#                        oauth license depsolver versioned_behaviors
#   all                  core + extended
#
# Individual tags are passed straight to Pedant's --focus.  Friendly aliases
# are accepted for a few suites whose RSpec metadata differs:
#   oauth -> oc_id, acls -> acl, versioned_behaviors -> api_v1
#
# Environment:
#   PEDANT_TAGS          space- or comma-separated tags (overridden by argv)
#   PEDANT_ARTIFACTS     directory for logs, generated keys, and config
#   PEDANT_RUBY          Ruby interpreter to use (default: asdf ruby 3.4.12)
#   PEDANT_BUNDLE_PATH   persistent bundle install path
#   MW_SKIP_BUILD=1      reuse an existing target/debug/microwave-server
#   MW_SERVER_BIN        explicit server binary (implies no build)
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PEDANT_DIR="$ROOT/chef-server/oc-chef-pedant"
SERVER_BIN="${MW_SERVER_BIN:-$ROOT/target/debug/microwave-server}"

ARTIFACTS="${PEDANT_ARTIFACTS:-$(mktemp -d "${TMPDIR:-/tmp}/mw-pedant.XXXXXX")}"
mkdir -p "$ARTIFACTS"
ARTIFACTS="$(cd "$ARTIFACTS" && pwd)"

# ---- Ruby / bundler ---------------------------------------------------------
if [[ -n "${PEDANT_RUBY:-}" ]]; then
  RUBY_BIN="$PEDANT_RUBY"
elif command -v asdf >/dev/null 2>&1 && asdf where ruby 3.4.12 >/dev/null 2>&1; then
  RUBY_BIN="$(asdf where ruby 3.4.12)/bin/ruby"
else
  RUBY_BIN="$(command -v ruby)"
fi
RUBY_DIR="$(dirname "$RUBY_BIN")"
export PATH="$RUBY_DIR:$PATH"
BUNDLE_BIN="$RUBY_DIR/bundle"
[[ -x "$BUNDLE_BIN" ]] || BUNDLE_BIN="$(command -v bundle)"

echo "==> ruby:       $("$RUBY_BIN" -v)"
echo "==> bundler:    $("$BUNDLE_BIN" -v)"
echo "==> artifacts:  $ARTIFACTS"

# ---- Bundler install --------------------------------------------------------
export BUNDLE_GEMFILE="$PEDANT_DIR/Gemfile"
export BUNDLE_PATH="${PEDANT_BUNDLE_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/mw-pedant-bundle}"
export BUNDLE_SILENCE_ROOT_WARNING=1

# Bundler rewrites the vendored lockfile for this host (extra platform, and the
# rack floor disappears because the omnibus safe_versions.rb is not present).
# Preserve the checkout's lockfile; restore it on exit.
PEDANT_LOCK="$PEDANT_DIR/Gemfile.lock"
LOCK_BACKUP="$ARTIFACTS/Gemfile.lock.original"
cp "$PEDANT_LOCK" "$LOCK_BACKUP"
restore_lock() {
  if [[ -f "$LOCK_BACKUP" ]] && ! cmp -s "$PEDANT_LOCK" "$LOCK_BACKUP"; then
    cp "$LOCK_BACKUP" "$PEDANT_LOCK"
  fi
}
trap restore_lock EXIT

if ! (cd "$PEDANT_DIR" && "$BUNDLE_BIN" check >/dev/null 2>&1); then
  echo "==> installing pedant bundle into $BUNDLE_PATH"
  (cd "$PEDANT_DIR" && "$BUNDLE_BIN" install)
fi

# ---- Build ------------------------------------------------------------------
if [[ -z "${MW_SERVER_BIN:-}" && "${MW_SKIP_BUILD:-0}" != "1" ]]; then
  echo "==> building microwave-server and microwave-ctl"
  (cd "$ROOT" && cargo build --locked --bin microwave-server --bin microwave-ctl)
fi
if [[ ! -x "$SERVER_BIN" ]]; then
  echo "error: server binary not found at $SERVER_BIN" >&2
  exit 2
fi

# ---- Tag selection ----------------------------------------------------------
CORE_TAGS=(headers users organizations clients nodes)
EXTENDED_TAGS=(keys acls data_bags cookbooks sandboxes search policies oauth
               license depsolver versioned_behaviors)

expand_tag_sets() {
  local tag
  for tag in "$@"; do
    case "$tag" in
      core) printf '%s\n' "${CORE_TAGS[@]}" ;;
      extended) printf '%s\n' "${EXTENDED_TAGS[@]}" ;;
      all) printf '%s\n' "${CORE_TAGS[@]}" "${EXTENDED_TAGS[@]}" ;;
      *) printf '%s\n' "$tag" ;;
    esac
  done
}

if [[ $# -gt 0 ]]; then
  TAGS=($(expand_tag_sets "$@"))
elif [[ -n "${PEDANT_TAGS:-}" ]]; then
  IFS=', ' read -r -a REQUESTED_TAGS <<<"${PEDANT_TAGS//,/ }"
  TAGS=($(expand_tag_sets "${REQUESTED_TAGS[@]}"))
else
  TAGS=("${CORE_TAGS[@]}")
fi

# ---- Tag aliases ------------------------------------------------------------
# Pedant's own tag names do not always match the suite names people use.  Map
# the friendly names accepted on the command line (and in PEDANT_TAGS) to the
# RSpec metadata tags in spec/api.
pedant_tag() {
  case "$1" in
    oauth|oc-id|oc_id) printf '%s' "oc_id" ;;
    acls|acl) printf '%s' "acl" ;;
    versioned_behaviors|versioned-behaviors|api_v1|api-v1) printf '%s' "api_v1" ;;
    *) printf '%s' "$1" ;;
  esac
}

# ---- Server lifecycle -------------------------------------------------------
SERVER_PID=""
SEARCH_STUB_PID=""
TAG_DIR=""
cleanup() {
  local status=$?
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  stop_search_stub
  restore_lock
  exit "$status"
}

start_search_stub() {
  local server_url="$1" superuser_key="$2" port
  port="$("$RUBY_BIN" -rsocket -e 's=TCPServer.new("127.0.0.1",0); puts s.addr[1]; s.close')"
  PEDANT_SEARCH_SERVER="http://127.0.0.1:$port"
  "$BUNDLE_BIN" exec "$RUBY_BIN" "$ROOT/scripts/pedant-solr-stub.rb" \
    "$port" "$server_url" "$superuser_key" "${PEDANT_ORG:-pedant_testorg}" \
    >"$TAG_DIR/solr-stub.log" 2>&1 &
  SEARCH_STUB_PID=$!
  export PEDANT_SEARCH_SERVER

  local ready=0
  for _ in $(seq 1 100); do
    if "$RUBY_BIN" -rsocket -e "TCPSocket.new('127.0.0.1', $port).close" 2>/dev/null; then
      ready=1
      break
    fi
    sleep 0.05
  done
  if [[ "$ready" != "1" ]]; then
    echo "error: solr commit stub did not start on port $port" >&2
    cat "$TAG_DIR/solr-stub.log" >&2 || true
    exit 3
  fi
}

stop_search_stub() {
  if [[ -n "$SEARCH_STUB_PID" ]] && kill -0 "$SEARCH_STUB_PID" >/dev/null 2>&1; then
    kill "$SEARCH_STUB_PID" >/dev/null 2>&1 || true
    wait "$SEARCH_STUB_PID" 2>/dev/null || true
  fi
  SEARCH_STUB_PID=""
}
trap cleanup EXIT INT TERM

start_server() {
  local tag="$1"
  TAG_DIR="$ARTIFACTS/$tag"
  local data_dir="$TAG_DIR/data"
  local database="$data_dir/microwave.db"
  local config="$TAG_DIR/microwave.toml"
  mkdir -p "$data_dir"

  local port
  port="$("$RUBY_BIN" -rsocket -e 's=TCPServer.new("127.0.0.1",0); puts s.addr[1]; s.close')"
  SERVER_URL="http://127.0.0.1:$port"

  cat >"$config" <<EOF
[server]
bind = "127.0.0.1:$port"

[paths]
data_dir = "$data_dir"

[database]
driver = "sqlite"
url = "sqlite://$database"

[license]
node_license = 25
upgrade_url = "https://www.chef.io/pricing"
EOF

  echo
  echo "==> [$tag] starting microwave-server on $SERVER_URL"
  RUST_LOG="${RUST_LOG:-info}" "$SERVER_BIN" serve --config "$config" >"$TAG_DIR/server.log" 2>&1 &
  SERVER_PID=$!

  local ready=0
  # A fresh SQLite database runs the full migration set on first boot; on a
  # cold page cache (and with several tags running back to back) that can take
  # well over ten seconds, so allow up to 60 seconds.
  for _ in $(seq 1 600); do
    if ! kill -0 "$SERVER_PID" >/dev/null 2>&1; then
      echo "error: [$tag] microwave-server exited during startup; see $TAG_DIR/server.log" >&2
      tail -30 "$TAG_DIR/server.log" >&2 || true
      exit 3
    fi
    if curl --silent --fail --max-time 1 "$SERVER_URL/_status" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 0.1
  done
  if [[ "$ready" != "1" ]]; then
    echo "error: [$tag] server did not become ready at $SERVER_URL/_status" >&2
    tail -30 "$TAG_DIR/server.log" >&2 || true
    exit 3
  fi
  if [[ ! -f "$data_dir/pivotal.pem" ]]; then
    echo "error: [$tag] server did not create $data_dir/pivotal.pem" >&2
    exit 3
  fi

  # Pedant verifies index deletion by querying the configured search server
  # directly.  Start the stub now that the API is reachable, so it can relay
  # those narrow queries to the live search API.
  start_search_stub "$SERVER_URL" "$data_dir/pivotal.pem"

  export PEDANT_SERVER_URL="$SERVER_URL"
  export PEDANT_SUPERUSER_KEY="$data_dir/pivotal.pem"
  export PEDANT_DATABASE="$database"
  export PEDANT_ARTIFACTS="$TAG_DIR"
  export PEDANT_CONFIG="$TAG_DIR/pedant_config.rb"
  # The ACL suite creates an auxiliary organization in a before(:all) hook
  # only when Pedant is allowed to create the main organization. This harness
  # bootstraps the main organization itself, so opt into auxiliary org
  # management for that suite only (other suites use the flag to pick a
  # dedicated org instead of the seeded one).
  if [[ "$(pedant_tag "$tag")" == "acl" ]]; then
    export PEDANT_AUX_ORG=1
  else
    unset PEDANT_AUX_ORG || true
  fi
  if ! (cd "$PEDANT_DIR" && "$BUNDLE_BIN" exec "$RUBY_BIN" "$ROOT/scripts/pedant-bootstrap.rb"); then
    echo "error: [$tag] pedant bootstrap failed (see output above)" >&2
    exit 4
  fi
}

stop_server() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
  stop_search_stub
}

# ---- Run --------------------------------------------------------------------
SUMMARY="$ARTIFACTS/summary.tsv"
: >"$SUMMARY"
FAILED_TAGS=()
PEDANT_STATUS=0

for tag in "${TAGS[@]}"; do
  [[ -n "$tag" ]] || continue
  start_server "$tag"
  focus="$(pedant_tag "$tag")"

  echo
  echo "==> [$tag] running pedant (focus: $focus)"
  LOG="$TAG_DIR/pedant.log"
  set +e
  (cd "$PEDANT_DIR" && \
    SUPERUSER_KEY="$PEDANT_SUPERUSER_KEY" \
    WEBUI_KEY="$PEDANT_SUPERUSER_KEY" \
    "$BUNDLE_BIN" exec ./bin/oc-chef-pedant \
      -c "$PEDANT_CONFIG" \
      -L "$TAG_DIR/http-traffic.log" \
      --focus "$focus" 2>&1) | tee "$LOG"
  status=${PIPESTATUS[0]}
  set -e

  line="$(grep -E '[0-9]+ examples?,' "$LOG" | tail -1 || true)"
  line="$(printf '%s' "$line" | sed -E $'s/\x1b\\[[0-9;]*m//g')"
  if [[ -z "$line" ]]; then
    line="NO RSPEC SUMMARY (suite aborted)"
  fi
  printf '%s\t%s\t%s\n' "$tag" "$status" "$line" >>"$SUMMARY"

  if [[ "$status" -ne 0 ]]; then
    FAILED_TAGS+=("$tag")
    PEDANT_STATUS="$status"
  fi

  stop_server
done

# ---- Summary ----------------------------------------------------------------
echo
echo "==> summary ($ARTIFACTS)"
if command -v column >/dev/null 2>&1; then
  column -t -s $'\t' "$SUMMARY"
else
  cat "$SUMMARY"
fi
echo
if [[ ${#FAILED_TAGS[@]} -eq 0 ]]; then
  echo "pedant: all selected tags passed"
else
  echo "pedant: failing tags: ${FAILED_TAGS[*]}" >&2
fi
echo "artifacts: $ARTIFACTS"
echo "pedant status: $PEDANT_STATUS"
exit "$PEDANT_STATUS"
