#!/usr/bin/env bash
#
# Converge this node once against microwave-server using the validator key and
# run list written by the seed container.
#
# Started by cinc-e2e-converge.service. The container keeps running after the
# converge (systemd stays PID 1), so the state can be inspected with
# `docker exec`.
#
set -euo pipefail

SHARED=/shared
# NODE_NAME comes from the compose service environment (systemd passes it via
# PassEnvironment); the kernel hostname is a fallback that needs no `hostname`
# binary (the Fedora image does not ship one).
NODE="${NODE_NAME:-$(cat /proc/sys/kernel/hostname)}"

echo "client: waiting for seed to finish"
# On a repeat `docker compose up` the seed container is restarted alongside the
# nodes and clears the previous run's completion marker as its first action.
# Wait briefly so a node that wins the startup race still sees the marker
# disappear instead of consuming the stale one.
sleep 5
for _ in $(seq 1 300); do
    if [ -f "$SHARED/seed.done" ]; then
        break
    fi
    sleep 2
done
if [ ! -f "$SHARED/seed.done" ]; then
    echo "client: error: seed did not finish" >&2
    exit 1
fi

echo "client: $(grep -E '^PRETTY_NAME=' /etc/os-release | cut -d= -f2-)"
echo "client: converging node $NODE"
echo "client: run_list $(cat "$SHARED/node.json")"

# The shared volume is a read-only handover directory. Stage the generated
# config under /etc/cinc: chef derives the Ohai plugin directory from the
# config file location, so running with -c /shared/client.rb would make the
# nginx install resource try to write into the read-only volume.
mkdir -p /etc/cinc /var/lib/cinc /var/cache/cinc /var/backups/cinc
cp "$SHARED/client.rb" /etc/cinc/client.rb
cp "$SHARED/validator.pem" /etc/cinc/validator.pem
cp "$SHARED/node.json" /etc/cinc/node.json
chmod 0600 /etc/cinc/validator.pem
sed -i "s|$SHARED/validator.pem|/etc/cinc/validator.pem|" /etc/cinc/client.rb
# Pin the node identity in the per-node config copy. Without this a plain
# `cinc-client` inside the container would use the container hostname and fail
# to authenticate.
printf '\nnode_name "%s"\n' "$NODE" >> /etc/cinc/client.rb

# The seed deletes this node's client on every run, so a key from an earlier
# seed generation is no longer valid. Re-register in that case; a plain
# container restart (same generation) keeps the existing key.
RUN_ID="$(cat "$SHARED/run-id" 2>/dev/null || echo unknown)"
if [ "$(cat /etc/cinc/seed-run 2>/dev/null || true)" != "$RUN_ID" ]; then
    rm -f /etc/cinc/client.pem
    printf '%s\n' "$RUN_ID" > /etc/cinc/seed-run
fi

exec /opt/cinc/bin/cinc-client \
    --config /etc/cinc/client.rb \
    --json-attributes /etc/cinc/node.json \
    --node-name "$NODE" \
    --log-level info \
    --no-color
