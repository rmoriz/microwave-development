#!/usr/bin/env bash
#
# End-to-end chef-client converge harness for microwave-server.
#
# What it does:
#   1. Builds microwave-server and microwave-ctl.
#   2. Starts the server on a free loopback port with a fresh temp data dir.
#   3. Creates an organization (and its validator client) plus a normal client.
#   4. Uploads a one-recipe cookbook through the sandbox/cookbook API.
#   5. Writes a chef-client config and JSON run list.
#   6. Runs a real chef-client, then verifies the marker file and the saved node.
#
# The cookbook recipe writes $ARTIFACTS/marker.txt, so a successful converge
# leaves observable local state. Nothing is stubbed.
#
# Usage:
#   scripts/run-chef-client-e2e.sh
#
# Environment:
#   MW_E2E_ARTIFACTS       artifact directory (default: mktemp -d)
#   MW_E2E_RUBY            Ruby interpreter with mixlib-authentication
#   MW_E2E_CHEF_CLIENT     explicit chef-client binary
#   MW_E2E_PREFER_VENDORED 1 to prefer the vendored chef checkout when a
#                          bundle is already installed (default: 0)
#   MW_E2E_BUNDLE_PATH     bundle path for the vendored chef checkout
#   MW_E2E_COMPAT          1 (default) to provision around known server gaps,
#                          0 for a strict stock-server run
#   MW_E2E_SYNTH_V2        1 (default) to synthesize an API v2 manifest when
#                          the server returns only the v0 shape; 0 to reproduce
#                          the stock chef-client v2 failure
#   MW_E2E_REGISTER        1 (default) to also run a validator-registration
#                          converge; 0 to skip it
#   MW_SKIP_BUILD          1 to reuse target/debug/microwave-server
#   MW_SERVER_BIN          explicit server binary (implies no build)
#
# Exit codes:
#   2  missing Ruby/mixlib-authentication or chef-client
#   3  server startup failure
#   4  bootstrap/upload failure
#   5  chef-client converge failure
#   6  marker file missing or wrong
#   7  node/client verification failure
#   8  validator-registration converge/verification failure
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOOTSTRAP="$ROOT/scripts/chef-client-e2e-bootstrap.rb"
SERVER_BIN="${MW_SERVER_BIN:-$ROOT/target/debug/microwave-server}"
ARTIFACTS="${MW_E2E_ARTIFACTS:-$(mktemp -d "${TMPDIR:-/tmp}/mw-chef-client-e2e.XXXXXX")}"
mkdir -p "$ARTIFACTS"
ARTIFACTS="$(cd "$ARTIFACTS" && pwd)"
ORG="${MW_E2E_ORG:-mw_e2e}"
NODE_NAME="${MW_E2E_NODE_NAME:-mw-e2e-node}"
COOKBOOK="${MW_E2E_COOKBOOK:-mw_e2e}"
COOKBOOK_VERSION="${MW_E2E_COOKBOOK_VERSION:-0.1.0}"
MARKER="$ARTIFACTS/marker.txt"
COMPAT="${MW_E2E_COMPAT:-1}"

SERVER_PID=""
cleanup() {
  local status=$?
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

# ---- Ruby / chef-client -----------------------------------------------------
if [[ -n "${MW_E2E_RUBY:-}" ]]; then
  RUBY_BIN="$MW_E2E_RUBY"
else
  RUBY_BIN="$(command -v ruby || true)"
fi
if [[ -z "$RUBY_BIN" ]]; then
  echo "error: no Ruby interpreter found; set MW_E2E_RUBY" >&2
  exit 2
fi
if ! "$RUBY_BIN" -e 'require "mixlib/authentication/signedheaderauth"' >/dev/null 2>&1; then
  echo "error: $RUBY_BIN cannot load mixlib-authentication." >&2
  echo "       Install chef-client or point MW_E2E_RUBY at a Ruby that has it." >&2
  exit 2
fi

vendored_bundle_ready() {
  [[ -f "$ROOT/chef/Gemfile" ]] || return 1
  local bundle_path="${MW_E2E_BUNDLE_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/mw-chef-bundle}"
  (cd "$ROOT/chef" && BUNDLE_GEMFILE="$ROOT/chef/Gemfile" BUNDLE_PATH="$bundle_path" bundle check >/dev/null 2>&1)
}

CHEF_KIND=""
CHEF_BIN=""
if [[ -n "${MW_E2E_CHEF_CLIENT:-}" ]]; then
  CHEF_KIND="installed"
  CHEF_BIN="$MW_E2E_CHEF_CLIENT"
elif [[ "${MW_E2E_PREFER_VENDORED:-0}" == "1" ]] && vendored_bundle_ready; then
  CHEF_KIND="vendored"
elif command -v chef-client >/dev/null 2>&1; then
  CHEF_KIND="installed"
  CHEF_BIN="$(command -v chef-client)"
elif vendored_bundle_ready; then
  CHEF_KIND="vendored"
else
  echo "error: no chef-client available." >&2
  echo "       Install chef-client, set MW_E2E_CHEF_CLIENT, or run bundle install in $ROOT/chef" >&2
  echo "       and set MW_E2E_PREFER_VENDORED=1." >&2
  exit 2
fi

if [[ "$CHEF_KIND" == "installed" ]]; then
  CHEF_VERSION="$("$CHEF_BIN" --version 2>&1 | tail -1 || true)"
else
  CHEF_VERSION="vendored $(cd "$ROOT/chef" && BUNDLE_GEMFILE="$ROOT/chef/Gemfile" BUNDLE_PATH="${MW_E2E_BUNDLE_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/mw-chef-bundle}" bundle exec ruby chef-bin/bin/chef-client --version 2>&1 | tail -1 || true)"
fi

echo "==> ruby:        $("$RUBY_BIN" -v)"
echo "==> chef-client: $CHEF_VERSION ($CHEF_KIND)"
echo "==> artifacts:   $ARTIFACTS"

# ---- Build ------------------------------------------------------------------
if [[ -z "${MW_SERVER_BIN:-}" && "${MW_SKIP_BUILD:-0}" != "1" ]]; then
  echo "==> building microwave-server and microwave-ctl"
  (cd "$ROOT" && cargo build --locked --bin microwave-server --bin microwave-ctl)
fi
if [[ ! -x "$SERVER_BIN" ]]; then
  echo "error: server binary not found at $SERVER_BIN" >&2
  exit 3
fi

# ---- Server lifecycle -------------------------------------------------------
DATA_DIR="$ARTIFACTS/data"
DB="$DATA_DIR/microwave.db"
CONFIG="$ARTIFACTS/microwave.toml"
mkdir -p "$DATA_DIR"
PORT="$("$RUBY_BIN" -rsocket -e 's=TCPServer.new("127.0.0.1",0); puts s.addr[1]; s.close')"
SERVER_URL="http://127.0.0.1:$PORT"

cat >"$CONFIG" <<EOF
[server]
bind = "127.0.0.1:$PORT"

[paths]
data_dir = "$DATA_DIR"

[database]
driver = "sqlite"
url = "sqlite://$DB"
EOF

echo "==> starting microwave-server on $SERVER_URL"
RUST_LOG="${RUST_LOG:-info}" "$SERVER_BIN" serve --config "$CONFIG" >"$ARTIFACTS/server.log" 2>&1 &
SERVER_PID=$!

ready=0
for _ in $(seq 1 600); do
  if ! kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    echo "error: microwave-server exited during startup; see $ARTIFACTS/server.log" >&2
    tail -30 "$ARTIFACTS/server.log" >&2 || true
    exit 3
  fi
  if curl --silent --fail --max-time 1 "$SERVER_URL/_status" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.1
done
if [[ "$ready" != "1" ]]; then
  echo "error: server did not become ready at $SERVER_URL/_status" >&2
  tail -30 "$ARTIFACTS/server.log" >&2 || true
  exit 3
fi
if [[ ! -f "$DATA_DIR/pivotal.pem" ]]; then
  echo "error: server did not create $DATA_DIR/pivotal.pem" >&2
  exit 3
fi

# ---- Bootstrap --------------------------------------------------------------
export MW_E2E_SERVER_URL="$SERVER_URL"
export MW_E2E_ARTIFACTS="$ARTIFACTS"
export MW_E2E_SUPERUSER_NAME="pivotal"
export MW_E2E_SUPERUSER_KEY="$DATA_DIR/pivotal.pem"
export MW_E2E_ORG="$ORG"
export MW_E2E_NODE_NAME="$NODE_NAME"
export MW_E2E_COOKBOOK="$COOKBOOK"
export MW_E2E_COOKBOOK_VERSION="$COOKBOOK_VERSION"
export MW_E2E_MARKER="$MARKER"
export MW_E2E_COMPAT="$COMPAT"
export MW_E2E_SYNTH_V2="${MW_E2E_SYNTH_V2:-1}"

echo "==> bootstrapping organization, clients, and cookbook"
if ! "$RUBY_BIN" "$BOOTSTRAP" 2>&1 | tee "$ARTIFACTS/bootstrap.log"; then
  echo "error: bootstrap failed; see $ARTIFACTS/bootstrap.log" >&2
  exit 4
fi

# ---- Run chef-client --------------------------------------------------------
run_chef_client() {
  local config="$1"
  local log="$2"
  local -a args=(-c "$config" -j "$ARTIFACTS/node.json" --no-color)

  set +e
  if [[ "$CHEF_KIND" == "installed" ]]; then
    CHEF_LICENSE=accept-no-persist "$CHEF_BIN" "${args[@]}" 2>&1 | tee "$log"
  else
    (
      cd "$ROOT/chef"
      BUNDLE_GEMFILE="$ROOT/chef/Gemfile" \
        BUNDLE_PATH="${MW_E2E_BUNDLE_PATH:-${XDG_CACHE_HOME:-$HOME/.cache}/mw-chef-bundle}" \
        CHEF_LICENSE=accept-no-persist \
        bundle exec ruby chef-bin/bin/chef-client "${args[@]}"
    ) 2>&1 | tee "$log"
  fi
  local status=${PIPESTATUS[0]}
  set -e
  return "$status"
}

rm -f "$MARKER"
echo "==> running chef-client"
if ! run_chef_client "$ARTIFACTS/client.rb" "$ARTIFACTS/chef-client.log"; then
  echo "error: chef-client converge failed" >&2
  echo "       log: $ARTIFACTS/chef-client.log" >&2
  tail -40 "$ARTIFACTS/chef-client.log" >&2 || true
  exit 5
fi

# ---- Verify -----------------------------------------------------------------
if [[ ! -f "$MARKER" ]] || ! grep -q 'converged by chef-client e2e' "$MARKER"; then
  echo "error: converge completed but marker file was not written: $MARKER" >&2
  exit 6
fi
echo "==> marker verified: $MARKER"

if ! "$RUBY_BIN" "$BOOTSTRAP" verify 2>&1 | tee "$ARTIFACTS/verify.log"; then
  echo "error: node/client verification failed; see $ARTIFACTS/verify.log" >&2
  exit 7
fi

# ---- Validator registration converge ---------------------------------------
if [[ "${MW_E2E_REGISTER:-1}" == "1" ]]; then
  rm -f "$ARTIFACTS/keys/registered-client.pem"
  echo "==> running chef-client with validator registration"
  if ! run_chef_client "$ARTIFACTS/client-validator.rb" "$ARTIFACTS/chef-client-validator.log"; then
    echo "error: validator registration converge failed" >&2
    echo "       log: $ARTIFACTS/chef-client-validator.log" >&2
    tail -40 "$ARTIFACTS/chef-client-validator.log" >&2 || true
    exit 8
  fi
  if [[ ! -s "$ARTIFACTS/keys/registered-client.pem" ]]; then
    echo "error: validator registration did not write $ARTIFACTS/keys/registered-client.pem" >&2
    exit 8
  fi
  if ! env MW_E2E_NODE_NAME="${NODE_NAME}-validator" \
    "$RUBY_BIN" "$BOOTSTRAP" verify 2>&1 | tee "$ARTIFACTS/verify-validator.log"; then
    echo "error: registered node/client verification failed; see $ARTIFACTS/verify-validator.log" >&2
    exit 8
  fi
  echo "==> validator registration verified: ${NODE_NAME}-validator"
fi

# ---- Summary ----------------------------------------------------------------
{
  echo "ruby: $("$RUBY_BIN" -v)"
  echo "chef-client: $CHEF_VERSION"
  echo "chef-client kind: $CHEF_KIND"
  echo "server: $SERVER_URL"
  echo "org: $ORG"
  echo "node: $NODE_NAME"
  echo "marker: $MARKER"
  echo "compat: $COMPAT"
  echo "v2 synthesis: ${MW_E2E_SYNTH_V2:-1}"
  echo "validator registration: ${MW_E2E_REGISTER:-1}"
  echo "capabilities: $(tr -d '\n' < "$ARTIFACTS/capabilities.json")"
} >"$ARTIFACTS/summary.txt"

echo
echo "chef-client e2e: PASS"
echo "artifacts: $ARTIFACTS"
cat "$ARTIFACTS/summary.txt"
