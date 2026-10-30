#!/usr/bin/env bash
#
# Seed microwave-server for the docker-e2e stack:
#   1. wait for the server and its bootstrap superuser key
#   2. create the organization (which also creates <org>-validator)
#   3. reset the node/client fixtures so a repeated run can bootstrap again
#   4. resolve nginx + postgres from Supermarket with Berkshelf
#   5. upload the cookbooks with knife
#   6. create a role whose run_list points at both cookbooks
#   7. write the Cinc Client config and node run_list for the test nodes
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

# ---- 3. Reset node fixtures ------------------------------------------------
# A validator may create clients but not update existing ones. chef-client
# turns the resulting 409 into a PUT, which the validator is not authorized
# for. Delete the test node and client leftovers with the superuser key so a
# second `docker compose up` bootstraps cleanly instead of failing with 403.
for node in $NODE_NAMES; do
    knife node delete "$node" --yes --config "$SHARED/knife.rb" >/dev/null 2>&1 || true
    knife client delete "$node" --yes --config "$SHARED/knife.rb" >/dev/null 2>&1 || true
done

# ---- 4. Resolve cookbooks from Supermarket ---------------------------------
cat >"$SHARED/Berksfile" <<EOF
source "https://supermarket.chef.io"

cookbook "$NGINX_COOKBOOK", "$NGINX_VERSION"
cookbook "$POSTGRES_COOKBOOK", "$POSTGRES_VERSION"
EOF

log "vendoring $NGINX_COOKBOOK $NGINX_VERSION and $POSTGRES_COOKBOOK $POSTGRES_VERSION"
cd "$SHARED"
rm -rf "$COOKBOOKS"
berks vendor "$COOKBOOKS"

# ---- 5. Upload cookbooks ---------------------------------------------------
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

# ---- 6. Role ---------------------------------------------------------------
cat >"$SHARED/role.rb" <<EOF
name "$ROLE_NAME"
description "nginx + PostgreSQL 18 from Chef Supermarket"
run_list "recipe[$NGINX_RECIPE]", "recipe[$POSTGRES_RECIPE]"
default_attributes({})
override_attributes({})
EOF

log "creating role $ROLE_NAME"
knife role from file "$SHARED/role.rb" --config "$SHARED/knife.rb"

# ---- 7. Client config and node run_list ------------------------------------
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
validator: $ORG-validator
role: $ROLE_NAME
run_list: recipe[$NGINX_RECIPE], recipe[$POSTGRES_RECIPE]
cookbooks: $NGINX_COOKBOOK $NGINX_VERSION, $POSTGRES_COOKBOOK $POSTGRES_VERSION
EOF

log "done"
cat "$SHARED/seed-summary.txt" >&2
touch "$SHARED/seed.done"
