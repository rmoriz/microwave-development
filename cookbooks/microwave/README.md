# microwave

Installs and configures
[Microwave Server](https://github.com/rmoriz/microwave-development), its
PostgreSQL database and its OpenSearch backend from packages on Debian 13
(`trixie`).

The cookbook is resource-oriented: `microwave_server`, `microwave_postgresql`
and `microwave_opensearch` compose on one node or spread over several, and the
recipes only wrap them.

## Supported platform

Debian 13 only. The cookbook needs Chef Infra Client 19.4.46 or newer, below
20.

## Properties

`microwave_postgresql` and `microwave_server` take `ca_certs`, a list of
PEM-encoded CA certificates. The server installs them into the system trust
store. The database renders them into the `ssl_ca_file` bundle it uses to
verify client certificates and falls back to the generated CA when the list is
empty.
