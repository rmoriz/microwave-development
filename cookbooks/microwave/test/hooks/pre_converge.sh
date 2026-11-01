#!/bin/sh
# Builds and starts the backend stack and waits, bounded, until all three nodes
# published their password and CA. The server node mounts the shared directory
# read-only, so it has to be complete before the converge starts.
#
# A healthy stack is left alone: rebuilding it rotates the password and all
# three CAs, which would change the node on every run. The marker records the
# fixture inputs the running stack was built from, so the hook can tell "same
# stack, running" from "needs a build" and stays a no-op in the common case. A
# failed skip falls back to a full rebuild.
set -eu

cd "$(dirname "$0")/.."

compose="docker compose -f fixtures/docker-compose.yml"
marker=fixtures/shared/.provisioned
cache=/var/cache/apt/archives

# Everything the backends are built from: the compose file, the node image and
# the cookbook including the key files it drops into /etc/apt/keyrings (the
# backend nodes converge it, so any of these may change them). The hooks run
# from the `test` directory, so the cookbook parts are one up.
digest() {
  find fixtures/docker-compose.yml fixtures/attrs.json fixtures/node \
    fixtures/cookbooks ../metadata.rb ../files ../recipes ../resources \
    ../templates ../attributes ../libraries -type f |
    sort | xargs cat | shasum -a 256 | cut -d' ' -f1
}

both_running() {
  services=$($compose ps --status running --services 2>/dev/null || true)
  printf '%s\n' "$services" | grep -qx db &&
    printf '%s\n' "$services" | grep -qx search &&
    printf '%s\n' "$services" | grep -qx garage
}

# The password has to be there and all three backends have to answer over TLS.
# Returns non-zero on a timeout or as soon as a backend container has exited.
ready() {
  deadline=$(( $(date +%s) + $1 ))
  while :; do
    if [ -s fixtures/shared/db.password ] &&
      [ -s fixtures/shared/db-ca.crt ] &&
      [ -s fixtures/shared/search-ca.crt ] &&
      [ -s fixtures/shared/garage.s3 ] &&
      $compose exec -T db pg_isready -q >/dev/null 2>&1 &&
      $compose exec -T search curl -fsS --cacert /mnt/shared/search-ca.crt \
        https://search.microwave.test:9200/_cluster/health 2>/dev/null |
        grep -Eq '"status":"(green|yellow)"' &&
      # An unsigned GET on the store root answers 403, so no `-f`: the hop is
      # what counts here.
      $compose exec -T garage curl -sS --cacert /mnt/shared/garage-ca.crt \
        https://garage.microwave.test:3900/ >/dev/null 2>&1; then
      return 0
    fi
    [ "$(date +%s)" -lt "$deadline" ] || return 1
    [ -z "$($compose ps --status exited -q)" ] || return 1
    sleep 3
  done
}

give_up() {
  echo "pre_converge: $1; giving up after the bounded wait" >&2
  $compose ps >&2 || true
  $compose logs --tail 40 >&2 || true
  exit 1
}

provision=1
if [ -f "$marker" ] && [ "$(cat "$marker")" = "$(digest)" ] && both_running; then
  if ready 60; then
    provision=0
    # stderr, like the give-up path: local hooks' stdout is swallowed.
    echo "pre_converge: stack unchanged and healthy, keeping it" >&2
  fi
fi

if [ "$provision" -eq 1 ]; then
  # Only the contents are wiped: the node already mounts this directory, so
  # deleting it would leave the mount pointing at a removed inode. The apt cache
  # is not touched at all; it has to survive runs.
  mkdir -p fixtures/shared
  find fixtures/shared -mindepth 1 -delete

  $compose build
  # The shared directory is wiped above, so the nodes have to converge again and
  # republish their secrets; a running container would not run its client.
  $compose up -d --force-recreate
  # Three sequential converges now, so the bound covers a cold apt cache on top
  # of the OpenSearch and Garage installs.
  ready 1800 || give_up 'the backends are not ready'
fi

# The cache directory is a host mount and the apt sandbox user has to be able to
# write it. The host mode can shift with the uid mapping, so set it from inside
# the containers: a fresh container is only guaranteed the host mode from
# pre_create. All three backends mount the same directory the kitchen node gets.
for service in db search garage; do
  $compose exec -T "$service" sh -c \
    "chmod 0777 $cache && install -d -m 0777 $cache/partial"
done

# The CAs are all distinct; the server trusts the bundle.
cat fixtures/shared/db-ca.crt fixtures/shared/search-ca.crt \
  fixtures/shared/garage-ca.crt > fixtures/shared/ca.crt

# The verifier runs on the server node only, so the backends' rendered vhosts are
# published next to the secrets for it to read.
$compose exec -T search cat /etc/nginx/sites-available/microwave-opensearch \
  > fixtures/shared/search-vhost.conf

$compose exec -T garage cat /etc/nginx/sites-available/microwave-garage \
  > fixtures/shared/garage-vhost.conf

# The shared copies are plain host files, so they are readable here; the mode
# the package set is published separately for the verifier to assert.
$compose exec -T db stat -c '%a' /etc/microwave/db.password \
  > fixtures/shared/db-password.mode

# The shared files are written by the containers, which run as root, so the
# runner itself cannot chmod them; the mode stays wide enough for the node to
# read what the probes publish.
$compose exec -T db sh -c 'chmod 0644 /mnt/shared/*'

# apt leaves its lock file behind as root:root 0640. Kitchen copies the cookbook
# tree for the sandbox and cannot read it, and the runner cannot chmod a file it
# does not own, but it can unlink it in the 0777 directory.
rm -f fixtures/cache/apt/archives/lock

if [ "$provision" -eq 1 ]; then
  digest > "$marker"
  chmod 0644 "$marker"
fi
