#!/bin/sh
#
# Boot the workstation: stage the knife config, warm the MCP client, then sleep
# so `docker compose exec` and `./shell` have a container to attach to.
#
# This image has no init system. systemd only ever ran the prepare script and
# kept the container alive, which `docker compose stop` does not need: the
# default SIGTERM ends `sleep` cleanly.
set -u

# The seed and the API can come up after this container does, so retry like the
# old unit's Restart=on-failure did. The script itself is idempotent.
until /usr/local/bin/workstation-prepare; do
    echo "workstation: prepare failed, retrying in 5s" >&2
    sleep 5
done

exec sleep infinity
