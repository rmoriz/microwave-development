# Chef cookbook

[`cookbooks/microwave`](../cookbooks/microwave/README.md) installs and
configures Microwave Server, PostgreSQL 19, OpenSearch 3.9 and, optionally,
Garage behind nginx on Debian 13 nodes. The cookbook is resource-oriented: the
four resources `microwave_server`, `microwave_postgresql`,
`microwave_opensearch` and `microwave_garage` compose on one node or spread over
several, and the recipes only wrap them. The
[README](../cookbooks/microwave/README.md) is the reference for every resource,
property and attribute; this page shows the two topologies.

It needs Chef Infra Client 19.4.46 or newer, below 20. The MCP endpoint stays
off: the cookbook renders no `[mcp]` block, and it stays `experimental` when you
add one — see [MCP endpoint](mcp.md).

## Prerequisites

The run list needs a Chef Infra Server, or a Microwave Server that answers the
Chef API. The server package is a `.deb` from a GitHub release.

`version: 'latest'` asks the GitHub API on every converge, so an anonymous rate
limit or an API outage fails the run. A `GITHUB_TOKEN` in the environment lifts
the anonymous limit; pinning a published version asks that release's tag
endpoint directly and is the escape hatch when `latest` stays broken.

## Minimal embedded node

The whole stack on one node: PostgreSQL and OpenSearch on loopback, the local
object store, Garage only when asked for.

```ruby
node.default['microwave']['bind'] = 'chef.example.com'
node.default['microwave']['create_organization'] = true
include_recipe 'microwave::default'
```

These two are all that is worth setting. The bind has to be set because the
default is the node name, which stock Debian maps to `127.0.1.1` in
`/etc/hosts`; the recipe prefers the node's primary address over that loopback
result, but a name that resolves to the reachable address is clearer.
`create_organization` creates the `pivotal` admin, the organization `demo` and
its membership, waits for `/_status`, and leaves the superuser key in
`<data_dir>/pivotal.pem` — back it up with the database.

Garage is composed only with `object_store_backend = 'garage'`; the default
`local` store writes the cookbook blobs under the data directory. That local
Garage endpoint defaults to `https://127.0.0.1:3900`, so set
`object_store_host` to the name clients reach the node at — or
`object_store_endpoint` directly — as soon as `chef-client` hosts elsewhere
fetch cookbooks: the endpoint lands in the presigned URLs. The listener is
plain HTTP, so client TLS belongs in a proxy in front of the node.

## Everything external

One node per role, every backend opened to the client network. The blocks are
independent: a mixed layout with a single backend somewhere else is fine too,
and only that backend's block changes.

```ruby
# database node
node.default['microwave']['client_network'] = '10.0.0.0/24'
include_recipe 'microwave::database'
```

```ruby
# search node
node.default['microwave']['client_network'] = '10.0.0.0/24'
include_recipe 'microwave::search'
```

```ruby
# store node
node.default['microwave']['client_network'] = '10.0.0.0/24'
include_recipe 'microwave::garage'
```

```ruby
# server node
node.default['microwave']['bind'] = '10.0.0.10'
node.default['microwave']['database_host'] = 'db.example.com'
node.default['microwave']['search_host'] = 'search.example.com'
node.default['microwave']['database_password_file'] = '/etc/chef/db.password'
node.default['microwave']['ca_cert_file'] = '/etc/chef/ca.crt'
node.default['microwave']['object_store_backend'] = 'garage'
node.default['microwave']['object_store_endpoint'] = 'https://store.example.com:3900'
node.default['microwave']['object_store_access_key_file'] = '/etc/chef/garage.access_key'
node.default['microwave']['object_store_secret_key_file'] = '/etc/chef/garage.secret_key'
include_recipe 'microwave::server'
```

Without `client_network` PostgreSQL keeps only `hostssl` loopback rules in
`pg_hba.conf` and the OpenSearch proxy stays on loopback, so no remote client
reaches either. The Garage proxy always listens on the store's name;
`client_network` is what adds its IP allow list, and without it the store
answers on the name unfenced.

`microwave::server` raises without a database host and a search host, and
without a password it can read from `database_password` or the password file.
A server-only wrapper therefore carries the database password and the CA
material itself, in files the run can read. The password is
`/etc/microwave/db.password` on the database node and the key pair is
`/etc/microwave/garage.access_key` and `/etc/microwave/garage.secret_key` on
the store node; copy all three together with the CA files before the server
converges. `ca_cert_file` has to hold all three CAs concatenated — every node
issues its own CA, and the server verifies the OpenSearch and S3 hops against
the system trust store.

`object_store_endpoint` lands in presigned URLs, so it must be the address the
clients reach the store at, and the store certificate has to cover that host.
The backends' CA files are `/etc/microwave/tls/ca.crt` on each backend node;
Garage's certificate is a separate pair from its own CA.

## The service switches

`microwave::disable` is `stop` plus `disable` and a marker file under
`/etc/microwave/disabled/<unit>`: no `mask`, no package removal, no data
deletion. The unit names are `microwave-server`, `postgresql`, `opensearch`,
`garage` and `nginx`.

The marker is what makes the switch stick. While it exists, every later
`microwave::default` converge skips the enable and start of its unit, so the
service stays down instead of waking on the next run. Only `:enable` or
`microwave::enable` removes it.

Disable goes server first and nginx last, so the node stops answering before
the backends go; enable is the inverse and starts nginx first. A single
resource's `:enable` never starts nginx, because both proxies share it, so
`microwave::enable` belongs on the same node.

## TLS limits

Each backend serves TLS from a cookbook-generated CA, and the server's own
connections to OpenSearch and the store verify against the system trust store.
Two gaps stay by design:

- **The database connection is not peer-verified.** The rendered URL ends in
  `sslmode=require`: the traffic is encrypted, but the driver does not check
  the certificate against the CA. `verify-full` in a hand-written
  `database_url` closes that.
- **The server's listener is plain HTTP.** The cookbook renders no
  `[server.tls]` block, and the installed CA covers the backend connections,
  not the clients. Terminate client TLS in a proxy in front of the node.

## Tests

```sh
bundle install                             # once
bundle exec rspec                          # ChefSpec, no docker needed
bundle exec cookstyle                      # lint
bundle exec rake kitchen:all_in_one        # one node, every resource
bundle exec rake kitchen:split             # server node against three backends
bundle exec rake kitchen:disable           # switch off, data survives, switch on
```

All three Kitchen suites converge twice with `enforce_idempotency`. They need a
docker daemon and `CHEF_LICENSE=accept-no-persist`. CI runs the specs, the lint
and the suites from `.github/workflows/cookbook.yml` for changes under
`cookbooks/**`.

## Reference

The [cookbook README](../cookbooks/microwave/README.md) documents every
resource, its properties, the attributes, the Garage provisioning and what the
certificates protect. Start there before changing an attribute this page does
not name.
