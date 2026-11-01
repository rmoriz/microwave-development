#!/bin/sh
# Tears the backend stack down and removes everything the suite created: the
# containers and their volumes, the locally built node image, the rendered
# secrets and the network. The apt cache survives.
#
# A failing `compose down` must not leave the secrets or the network behind, so
# only its status is carried out; the cleanup below always runs.
set -eu

cd "$(dirname "$0")/.."

status=0
docker compose -f fixtures/docker-compose.yml down -v --rmi local || status=$?

rm -rf fixtures/shared
docker network rm microwave-test >/dev/null 2>&1 || true

exit "$status"
