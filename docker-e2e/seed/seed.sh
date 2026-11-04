#!/usr/bin/env bash
#
# Seed microwave-server for the docker-e2e stack:
#   1. wait for the server and its bootstrap superuser key
#   2. create the organization (which also creates <org>-validator)
#   3. create the demo console user and add it to admins
#   4. reset the node/client fixtures so a repeated run can bootstrap again
#   5. resolve nginx + postgres from Supermarket with Berkshelf
#   6. upload the cookbooks with knife
#   7. create a role whose run_list points at both cookbooks
#   8. write the Cinc Client config and node run_list for the test nodes
#
set -euo pipefail

SERVER="${MW_SERVER_URL:-http://microwave:8000}"
ORG="${MW_ORG:-e2e}"
ORG_FULL_NAME="${MW_ORG_FULL_NAME:-Microwave E2E}"
ROLE_NAME="${ROLE_NAME:-e2e_role}"
NGINX_COOKBOOK="${NGINX_COOKBOOK:-nginx}"
NGINX_VERSION="${NGINX_VERSION:-12.3.4}"
POSTGRES_COOKBOOK="${POSTGRES_COOKBOOK:-postgresql}"
POSTGRES_VERSION="${POSTGRES_VERSION:-13.0.5}"
NGINX_RECIPE="${NGINX_RECIPE:-e2e_nginx}"
POSTGRES_RECIPE="${POSTGRES_RECIPE:-e2e_postgresql}"
# Client names the node containers register with. Included so the seed can
# remove leftovers from a previous, partially failed run.
NODE_NAMES="${NODE_NAMES:-node-debian node-ubuntu node-fedora node-amazon}"

# Demo console login. Fixture credentials, not production.
CONSOLE_USER="${MW_CONSOLE_USER:-e2e-console}"
CONSOLE_PASSWORD="${MW_CONSOLE_PASSWORD:-e2e-console-password}"
CONSOLE_EMAIL="${MW_CONSOLE_EMAIL:-${CONSOLE_USER}@example.com}"

SHARED=/shared
DATA=/mw-data
COOKBOOKS="$SHARED/cookbooks"

# Chef tooling in this image asks for license acceptance on first run.
export CHEF_LICENSE=accept-no-persist
export BERKSHELF_PATH=/var/lib/berkshelf

log() { printf 'seed: %s\n' "$*" >&2; }

mkdir -p "$SHARED"
# Remove the previous run's completion marker first. `docker compose up` starts
# the nodes alongside a restarted seed container, and a stale marker would let
# them register before the seed has reset their clients.
rm -f "$SHARED/seed.done"
# Generation marker: nodes only re-register when the seed ran again (see
# client/converge.sh).
RUN_ID="$(date +%s)-$$"
printf '%s\n' "$RUN_ID" >"$SHARED/run-id"

# ---- 1. Wait for microwave -------------------------------------------------
log "waiting for $SERVER/_status and $DATA/pivotal.pem"
for _ in $(seq 1 150); do
    if curl -fsS "$SERVER/_status" >/dev/null 2>&1 && [ -s "$DATA/pivotal.pem" ]; then
        break
    fi
    sleep 2
done
if ! curl -fsS "$SERVER/_status" >/dev/null 2>&1; then
    log "error: microwave did not become ready"
    exit 1
fi
if [ ! -s "$DATA/pivotal.pem" ]; then
    log "error: $DATA/pivotal.pem is missing"
    exit 1
fi

cp "$DATA/pivotal.pem" "$SHARED/pivotal.pem"
chmod 0600 "$SHARED/pivotal.pem"

# ---- 2. Organization -------------------------------------------------------
# Server-level config: no organization in the URL.
cat >"$SHARED/knife-server.rb" <<EOF
current_dir = File.dirname(__FILE__)
log_level :info
log_location STDOUT
node_name "pivotal"
client_key "$SHARED/pivotal.pem"
chef_server_url "$SERVER"
ssl_verify_mode :verify_none
EOF

log "creating organization $ORG"
if knife org show "$ORG" --config "$SHARED/knife-server.rb" >/dev/null 2>&1; then
    log "organization $ORG already exists"
    if [ ! -s "$SHARED/validator.pem" ]; then
        log "error: $ORG exists but $SHARED/validator.pem is missing;"
        log "       remove the volumes (docker compose down -v) and start over"
        exit 1
    fi
else
    knife org create "$ORG" "$ORG_FULL_NAME" \
        --filename "$SHARED/validator.pem" \
        --config "$SHARED/knife-server.rb"
fi
chmod 0600 "$SHARED/validator.pem"

# Organization-level config for the remaining knife commands.
cat >"$SHARED/knife.rb" <<EOF
current_dir = File.dirname(__FILE__)
log_level :info
log_location STDOUT
node_name "pivotal"
client_key "$SHARED/pivotal.pem"
chef_server_url "$SERVER/organizations/$ORG"
ssl_verify_mode :verify_none
EOF

# ---- 3. Console demo user --------------------------------------------------
# Recreate on every run so the documented password stays true. The delete
# disassociates org membership by default; -R additionally bypasses knife's
# "only admin" guard should the explicit group removal have failed. Absence
# on the first run is not an error.
remove_console_user() {
    knife group remove user "$1" admins --config "$SHARED/knife.rb" >/dev/null 2>&1 || true
    knife user delete "$1" --remove-from-admin-groups -y \
        --config "$SHARED/knife-server.rb" >/dev/null 2>&1 || true
}

# A previous run may have used another name. Remove it so an override replaces
# the account instead of leaving a second admin with the default password. The
# summary fallback covers a stack seeded before the state file existed.
prev_console_user=""
if [ -s "$SHARED/console-user" ]; then
    prev_console_user="$(head -1 "$SHARED/console-user")"
elif [ -f "$SHARED/seed-summary.txt" ]; then
    prev_console_user="$(sed -n 's/^console_user: //p' "$SHARED/seed-summary.txt" | tail -1)"
fi
if [ -n "$prev_console_user" ] && [ "$prev_console_user" != "$CONSOLE_USER" ]; then
    log "removing previous console user $prev_console_user"
    remove_console_user "$prev_console_user"
    # `user list` distinguishes a surviving user from a transient `show` error:
    # an assignment failure aborts under `set -e`, absence is explicit below.
    remaining_users="$(knife user list --config "$SHARED/knife-server.rb" -F json)"
    case "$remaining_users" in
    *\"$prev_console_user\"*)
        log "error: previous console user $prev_console_user still exists"
        exit 1
        ;;
    esac
fi

log "resetting console user $CONSOLE_USER"
remove_console_user "$CONSOLE_USER"
knife user create "$CONSOLE_USER" \
    --email "$CONSOLE_EMAIL" \
    --password "$CONSOLE_PASSWORD" \
    --config "$SHARED/knife-server.rb"
printf '%s\n' "$CONSOLE_USER" >"$SHARED/console-user"
knife org user add "$ORG" "$CONSOLE_USER" --config "$SHARED/knife.rb"
knife group add user "$CONSOLE_USER" admins --config "$SHARED/knife.rb"

# ---- 4. Reset node fixtures ------------------------------------------------
# A validator may create clients but not update existing ones. chef-client
# turns the resulting 409 into a PUT, which the validator is not authorized
# for. Delete the test node and client leftovers with the superuser key so a
# second `docker compose up` bootstraps cleanly instead of failing with 403.
for node in $NODE_NAMES; do
    knife node delete "$node" --yes --config "$SHARED/knife.rb" >/dev/null 2>&1 || true
    knife client delete "$node" --yes --config "$SHARED/knife.rb" >/dev/null 2>&1 || true
done

# ---- 5. Resolve cookbooks from Supermarket ---------------------------------
cat >"$SHARED/Berksfile" <<EOF
source "https://supermarket.chef.io"

cookbook "$NGINX_COOKBOOK", "$NGINX_VERSION"
cookbook "$POSTGRES_COOKBOOK", "$POSTGRES_VERSION"
EOF

log "vendoring $NGINX_COOKBOOK $NGINX_VERSION and $POSTGRES_COOKBOOK $POSTGRES_VERSION"
cd "$SHARED"
rm -rf "$COOKBOOKS"
berks vendor "$COOKBOOKS"

# ---- 6. Upload cookbooks ---------------------------------------------------
log "uploading cookbooks"
knife cookbook upload --all \
    --cookbook-path "$COOKBOOKS" \
    --config "$SHARED/knife.rb"

# Both Supermarket cookbooks are resource-only, so the role points at the local
# wrapper recipes. Upload them after the vendored cookbooks so their
# dependencies are already on the server.
for wrapper in e2e_nginx e2e_postgresql; do
    log "uploading wrapper cookbook $wrapper"
    knife cookbook upload "$wrapper" \
        --cookbook-path /seed/site-cookbooks \
        --config "$SHARED/knife.rb"
done

# ---- 7. Role ---------------------------------------------------------------
cat >"$SHARED/role.rb" <<EOF
name "$ROLE_NAME"
description "nginx + PostgreSQL 18 from Chef Supermarket"
run_list "recipe[$NGINX_RECIPE]", "recipe[$POSTGRES_RECIPE]"
default_attributes({})
override_attributes({})
EOF

log "creating role $ROLE_NAME"
knife role from file "$SHARED/role.rb" --config "$SHARED/knife.rb"

# ---- 8. Client config and node run_list ------------------------------------
cat >"$SHARED/client.rb" <<EOF
chef_server_url "$SERVER/organizations/$ORG"
validation_client_name "$ORG-validator"
validation_key "$SHARED/validator.pem"
ssl_verify_mode :verify_none
# The shared volume is read-only in the node containers, so keep the machine
# identifier and caches on the container filesystem.
chef_guid_path "/var/lib/cinc/cinc_guid"
file_cache_path "/var/cache/cinc"
file_backup_path "/var/backups/cinc"
checksum_path "/var/cache/cinc/checksums"
log_level :info
EOF

cat >"$SHARED/node.json" <<EOF
{
  "run_list": ["role[$ROLE_NAME]"]
}
EOF

cat >"$SHARED/seed-summary.txt" <<EOF
server: $SERVER
organization: $ORG
console_user: $CONSOLE_USER
console_password: $CONSOLE_PASSWORD
validator: $ORG-validator
role: $ROLE_NAME
run_list: recipe[$NGINX_RECIPE], recipe[$POSTGRES_RECIPE]
cookbooks: $NGINX_COOKBOOK $NGINX_VERSION, $POSTGRES_COOKBOOK $POSTGRES_VERSION
EOF

log "done"
cat "$SHARED/seed-summary.txt" >&2
touch "$SHARED/seed.done"
