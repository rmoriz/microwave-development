#!/bin/sh
# One-shot Garage provisioning for the docker-postgres-opensearch-garage stack.
# Runs in garage's network namespace and only acts on state that is missing, so
# a repeated `docker compose up -d` logs no expected-error noise.
#
# The layout is assigned once; a later MW_GARAGE_CAPACITY change needs a manual
# `layout assign` + `layout apply --version <n>`.
set -eu

bucket="${MW_S3_BUCKET:?MW_S3_BUCKET is required}"
access_key="${MW_S3_ACCESS_KEY:?MW_S3_ACCESS_KEY is required}"
secret_key="${MW_S3_SECRET_KEY:?MW_S3_SECRET_KEY is required}"
capacity="${MW_GARAGE_CAPACITY:-1G}"

until /garage -c /etc/garage.toml status >/dev/null 2>&1; do
    sleep 1
done

if /garage -c /etc/garage.toml status | grep -q 'NO ROLE ASSIGNED'; then
    node_id="$(/garage -c /etc/garage.toml node id -q | cut -d@ -f1)"
    /garage -c /etc/garage.toml layout assign -z dc1 -c "$capacity" "$node_id"
    /garage -c /etc/garage.toml layout apply --version 1
fi

if ! /garage -c /etc/garage.toml bucket info "$bucket" >/dev/null 2>&1; then
    /garage -c /etc/garage.toml bucket create "$bucket"
fi

if ! /garage -c /etc/garage.toml key list 2>/dev/null | grep -q "$access_key"; then
    /garage -c /etc/garage.toml key import --yes "$access_key" "$secret_key"
fi

/garage -c /etc/garage.toml bucket allow --read --write --owner "$bucket" --key "$access_key"

echo "garage ready: bucket $bucket, key $access_key"
