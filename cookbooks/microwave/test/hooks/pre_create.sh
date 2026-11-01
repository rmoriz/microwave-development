#!/bin/sh
# Runs before the kitchen node exists, so everything the driver bind-mounts is
# already there: the shared secret directory and, for the split suite, the
# network the node joins.
set -eu

cd "$(dirname "$0")/.."

mkdir -p fixtures/shared fixtures/logs
# The apt cache is persistent and shared by every node; the archives directory
# must be there before the container bind-mounts it and `_apt` must be able to
# write into it, hence 0777 (the sandbox user is not the owner).
mkdir -p fixtures/cache/apt/archives/partial
chmod 0777 fixtures/cache/apt/archives fixtures/cache/apt/archives/partial

if ! docker network inspect microwave-test >/dev/null 2>&1; then
  docker network create --subnet 172.28.0.0/16 microwave-test >/dev/null
fi
