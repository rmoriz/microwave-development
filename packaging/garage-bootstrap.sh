#!/bin/sh
# Garage bootstrap for the docker-compose.ha.yml profile.
#
# Starts the Garage server in the same container, waits for it to answer, then
# creates the layout, bucket, and S3 key that the Microwave Server config
# points at. Running the CLI in-container means it talks to the local node
# (rpc_public_addr 127.0.0.1:3901 in packaging/garage.toml) and shares its
# metadata directory.
#
# Evaluation only. A production Garage cluster is provisioned out of band and
# needs multiple nodes, a replication factor > 1, and a TLS reverse proxy.

set -eu

bucket="${MW_S3_BUCKET:-chef-cookbooks}"
access_key="${MW_S3_ACCESS_KEY:?MW_S3_ACCESS_KEY is required}"
secret_key="${MW_S3_SECRET_KEY:?MW_S3_SECRET_KEY is required}"

/garage -c /etc/garage.toml server &
garage_pid=$!
trap 'kill -TERM "$garage_pid" 2>/dev/null || true' TERM INT

until /garage -c /etc/garage.toml status >/dev/null 2>&1; do
    sleep 1
done

node_id="$(/garage -c /etc/garage.toml node id -q | cut -d@ -f1)"

# Assign the single node to a zone and apply layout version 1. Garage v1.0
# takes the capacity in bytes and accepts suffixes, so `1G` is one gibibyte.
# Both commands fail harmlessly when the layout is already configured.
/garage -c /etc/garage.toml layout assign -z dc1 -c "${MW_GARAGE_CAPACITY:-1G}" "$node_id" || true
/garage -c /etc/garage.toml layout apply --version 1 || true

/garage -c /etc/garage.toml bucket create "$bucket" || true
/garage -c /etc/garage.toml key import --yes "$access_key" "$secret_key" || true
/garage -c /etc/garage.toml bucket allow --read --write --owner "$bucket" --key "$access_key"

echo "garage ready: bucket $bucket, key $access_key"

wait "$garage_pid"
