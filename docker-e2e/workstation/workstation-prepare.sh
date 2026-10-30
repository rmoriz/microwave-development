#!/usr/bin/env bash
#
# Stage the knife config the seed wrote on the read-only hand-over volume into
# the workspace, so plain `knife …` works without -c flags, then connect the
# OpenCode MCP client.
#
# Started by the workstation entrypoint. The container stays up after this
# script returns, so the box can be used with `docker compose exec` / `./shell`.
#
set -euo pipefail

SHARED=/shared
CHEF_DIR=/root/work/.chef

echo "workstation: waiting for the seed to write $SHARED/knife.rb"
for _ in $(seq 1 300); do
    if [ -s "$SHARED/knife.rb" ] && [ -s "$SHARED/pivotal.pem" ]; then
        break
    fi
    sleep 2
done
if [ ! -s "$SHARED/knife.rb" ] || [ ! -s "$SHARED/pivotal.pem" ]; then
    echo "workstation: error: the seed did not write the knife config" >&2
    exit 1
fi

install -d -m 0700 "$CHEF_DIR"
cp "$SHARED/knife.rb" "$CHEF_DIR/knife.rb"
cp "$SHARED/pivotal.pem" "$CHEF_DIR/pivotal.pem"
chmod 0600 "$CHEF_DIR/knife.rb" "$CHEF_DIR/pivotal.pem"
# The shared volume is read-only here; point the config at the staged key.
sed -i "s|client_key \"$SHARED/pivotal.pem\"|client_key \"$CHEF_DIR/pivotal.pem\"|" \
    "$CHEF_DIR/knife.rb"

echo "workstation: knife is configured for $(knife node list | wc -l) nodes"

# OpenCode 2.2.0 only registers configured MCP servers on a configuration
# reload; the first start of its background service reports an empty list.
# Warm the service up and reload once so the endpoint is connected before the
# box is used.
echo "workstation: connecting the MCP client"
cd /root/work
opencode mcp list >/dev/null 2>&1 || true
opencode reload >/dev/null 2>&1 || true
for _ in $(seq 1 30); do
    if opencode mcp list 2>/dev/null | grep -q 'microwave-e2e.*connected'; then
        break
    fi
    sleep 2
done
opencode mcp list || true
