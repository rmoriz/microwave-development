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

## Tests

```sh
bundle exec rspec                          # ChefSpec, no docker needed
bundle exec rake kitchen:all_in_one        # one node, every resource
bundle exec rake kitchen:split             # server node against two backends
```

The kitchen suites need a docker daemon and a `DOCKER_HOST` that reaches it.
Both capture their console into `test/fixtures/logs/kitchen-<suite>.log`;
`.kitchen/logs` is truncated by every kitchen invocation and is not a record.
Both suites enable `enforce_idempotency` and converge twice. The bundled
verifier is Chef InSpec, which needs `CHEF_LICENSE=accept-no-persist` (the rake
tasks set it).

The split suite's `pre_converge` hook keeps a marker of the fixture and cookbook
inputs. `kitchen converge split` on an unchanged, healthy stack therefore skips
the rebuild and converges nothing; `kitchen test split` starts with a destroy,
whose `post_destroy` deletes the marker, so a test always provisions once.

The apt archives are cached in `test/fixtures/cache/apt/archives` and survive
runs, because the OpenSearch deb alone is 1.1 GB. The cache is not pruned; it
currently holds roughly 950 MB (158 archives) after runs of both suites and grows
with every package version.

