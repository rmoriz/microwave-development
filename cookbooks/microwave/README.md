# microwave

Installs and configures
[Microwave Server](https://github.com/rmoriz/microwave-development), its
PostgreSQL database and its OpenSearch backend from packages on Debian 13
(`trixie`).

The cookbook is resource-oriented: `microwave_server`, `microwave_postgresql`
and `microwave_opensearch` compose on one node or spread over several, and the
recipes only wrap them.

Microwave's MCP endpoint is experimental. This cookbook renders no `[mcp]`
block, so MCP stays off until you add one; once it is on, its writes are on by
default and only switch off with `writes = false`. See
[`docs/mcp.md`](../../docs/mcp.md).

## Supported platform

Debian 13 only. The cookbook needs Chef Infra Client 19.4.46 or newer, below
20. The backend services are systemd units; on a node without
`/run/systemd/system` the OpenSearch resource starts the daemon directly
instead.

## Resources

All three resources carry one `:install` action; there is no remove action.

| Resource | Installs | Manages |
|---|---|---|
| `microwave_server` | The `microwave-server` `.deb` of a GitHub release, checksum-pinned to the release's published `.sha256`; `logrotate` | `/etc/microwave/microwave.toml` (mode `0640`, `root:microwave`), the systemd ordering drop-in, the `microwave-server` service, the backend CAs in the system trust store, and the demo organization when asked |
| `microwave_postgresql` | PostgreSQL 19 from the PGDG repository (pinned), the `microwave` system group without login | A cookbook-generated CA and the `db` service certificate, `/etc/microwave/db.password`, `conf.d/microwave.conf` (TLS on), `pg_hba.conf`, the application role and its database |
| `microwave_opensearch` | OpenSearch 3.9.0 (pinned) and nginx | `/etc/opensearch/opensearch.yml` on loopback port 9210, the JVM heap, the `vm.max_map_count` sysctl, and the nginx TLS proxy on port 9200 |

The server package comes from `rmoriz/microwave-development` releases.
`version: 'latest'` asks the GitHub API for the newest release and requires its
tag to carry a `.deb` for the node's machine (`amd64`/`arm64`) plus a matching
`.sha256`; a draft release is rejected, so pin a published version instead. A
`GITHUB_TOKEN` in the environment lifts the anonymous API rate limit.

`latest` asks the `releases/latest` endpoint on every converge, so a rate limit
or an API outage fails the run. Pinning `version` is the escape hatch: it asks
the release's tag endpoint directly, so a `latest` that appears broken cannot
hold it back.

### `microwave_server`

| Property | Type | Default | Details |
|---|---|---|---|
| `database_url` | String | — | Required. Rendered into `[database]`, as is. |
| `search_url` | String | — | Required. Rendered into `[search]`, as is. |
| `bind` | String | `node['microwave']['bind']` | Rendered into `[server] bind`, with port. The recipe resolves a name to an address first. |
| `public_url` | String | unset | `server.public_url`; rendered only when given. |
| `version` | String | `'latest'` | `latest` or `x.y.z`, nothing else. |
| `ca_certs` | Array | `[]` | PEM CAs written to `/usr/local/share/ca-certificates` and installed with `update-ca-certificates`. |

A `bind` name is resolved before it is written. When the name resolves to a
loopback address — stock Debian maps the hostname to `127.0.1.1` in
`/etc/hosts` — the node's primary address is used instead, so the default does
not leave the listener loopback-only. A host that is reachable from elsewhere
still wants an explicit name that resolves to a reachable address, or a
`public_url` when the public base differs from the bind.

The resource also renders
`/etc/systemd/system/microwave-server.service.d/ordering.conf` (`After=` and
`Wants=` both backends) and enables and starts the service. The server unit
keeps `LimitMEMLOCK=infinity`; `microwave_opensearch` adds the same limit to the
OpenSearch unit, which is the process that maps locked memory.

### `microwave_postgresql`

| Property | Type | Default | Details |
|---|---|---|---|
| `hostname` | String | node FQDN, else hostname | Certificate name and SAN, and the name the server dials. |
| `port` | Integer | `node['microwave']['database_port']` | Listener port. |
| `database` | String | `node['microwave']['database_name']` | Database the role owns. |
| `user` | String | `node['microwave']['database_user']` | Login role. |
| `client_network` | String | `node['microwave']['client_network']` | CIDR for the `pg_hba` `hostssl` rule. Without it the listener stays on loopback and no remote client can connect. |
| `ca_certs` | Array | `[]` | PEM bundle written to `tls/client-ca.crt` and used as `ssl_ca_file`; the generated CA is the fallback. |

The role password is generated once into `/etc/microwave/db.password` (mode
`0640`, `root:microwave`) and re-applied to the role if the file is recreated.
`pg_hba.conf` always keeps `hostssl` rules for loopback, so every connection,
including the all-in-one one, is encrypted.

### `microwave_opensearch`

| Property | Type | Default | Details |
|---|---|---|---|
| `hostname` | String | node FQDN, else hostname | Certificate name and SAN, nginx `server_name`. |
| `heap` | String | `node['microwave']['opensearch_heap']` | `-Xms`/`-Xmx`; the same value for both. |
| `client_network` | String | `node['microwave']['client_network']` | CIDR the nginx proxy allows in addition to loopback. |

OpenSearch itself only listens on `127.0.0.1:9210`. The security plugin is
removed — Microwave's client sends no credentials — so the nginx proxy on
`9200` is the only way in and the only access control: on loopback without
`client_network`, on the node's name with it, and then behind an allow list.

## Recipes

| Recipe | Contents |
|---|---|
| `microwave::default` | All three resources on one node. Backend hosts default to `127.0.0.1` unless a wrapper set them. |
| `microwave::server` | The server only, pointed at `database_host` and `search_host`. |
| `microwave::database` | PostgreSQL only. |
| `microwave::search` | OpenSearch only. |

## Attributes

The server recipe reads `bind`, `database_host`, `search_host`, the password
settings, `ca_certs`/`ca_cert_file`, `public_url` and `create_organization`; the
rest are defaults the resources pick up.

| Attribute | Default | Purpose |
|---|---|---|
| `node['microwave']['bind']` | node FQDN, else hostname | Address or name the server binds; the recipe resolves it, prefers the primary address over a loopback result, and appends `:8000`. |
| `node['microwave']['public_url']` | `nil` | `server.public_url` when set; the explicit way to publish a reachable base without changing the bind. |
| `node['microwave']['database_name']` | `'microwave'` | Database and the name in the connection URL. |
| `node['microwave']['database_user']` | `'microwave'` | Role and the name in the connection URL. |
| `node['microwave']['database_host']` | `nil` | Database node; `microwave::default` defaults it to `127.0.0.1`. |
| `node['microwave']['database_port']` | `5432` | Database port in the URL and for the backend. |
| `node['microwave']['database_password']` | `nil` | Literal password; wins over the file. Prefer the file on a shared attributes source. |
| `node['microwave']['database_password_file']` | `/etc/microwave/db.password` | Read for the password when `database_password` is unset or empty. |
| `node['microwave']['ca_certs']` | `[]` | PEM CAs handed to the server resource and installed into the trust store. |
| `node['microwave']['ca_cert_file']` | `nil` | File to read one CA from when `ca_certs` is empty; the local generated CA is the last fallback. |
| `node['microwave']['search_host']` | `nil` | Search node; the URL is `https://<host>:9200`. |
| `node['microwave']['opensearch_heap']` | `'1g'` | OpenSearch heap. |
| `node['microwave']['client_network']` | `nil` | CIDR that opens both backends. |
| `node['microwave']['create_organization']` | `false` | Creates the `pivotal` admin, the organization `demo` and its membership, and waits for `/_status` first. |

## Topologies

### All-in-one

The whole stack on one node. Only the bind is worth setting:

```ruby
node.default['microwave']['bind'] = 'chef.example.com'
node.default['microwave']['create_organization'] = true
include_recipe 'microwave::default'
```

Backends only listen on loopback in this layout; the server dials
`127.0.0.1`.

### Split

A database node, a search node and a server node. Both backends get the client
network; the database node also publishes its password and CA, and the server
node reads them:

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
# server node
node.default['microwave']['bind'] = '10.0.0.10'
node.default['microwave']['database_host'] = 'db.example.com'
node.default['microwave']['search_host'] = 'search.example.com'
node.default['microwave']['database_password_file'] = '/etc/chef/db.password'
node.default['microwave']['ca_cert_file'] = '/etc/chef/ca.crt'
include_recipe 'microwave::server'
```

`microwave::server` raises without a database host and a search host, and
without a password it can read from `database_password` or the password file.
A server-only wrapper must therefore carry the backend password and CA itself.

## TLS and the `sslmode=require` limit

Each node issues its own CA (`/etc/microwave/tls/ca.crt`, ten years) and the
service certificate it signs (825 days, node name and `127.0.0.1` as SANs). Only
the CA key signs; the service keys are `0600`/`0640` and owned by the service
user, while the CA key is `0640 root:root` because both backends share it.

Two gaps remain by design:

- **The database connection is not peer-verified.** The URL the server recipe
  renders ends in `sslmode=require`: the traffic is encrypted, but the driver
  does not check the certificate against the CA. A host answering on the
  database port with any certificate passes. `verify-full` in a hand-written
  `database_url` closes that.
- **The server's own listener is plain HTTP.** The cookbook renders no
  `[server.tls]` block, so clients need a TLS-terminating proxy in front. The
  CA the cookbook installs is for the backend connections, not for clients.

Installing the CA into the system trust store makes the OpenSearch HTTPS
connection verify. That connection is the one the CA has to cover.

## Tests

```sh
bundle install                             # once
bundle exec rspec                          # ChefSpec, no docker needed
bundle exec cookstyle                      # lint
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

CI runs `bundle exec rspec` and `bundle exec cookstyle` plus both suites in
`.github/workflows/cookbook.yml`, for pull requests and `main` pushes that
touch `cookbooks/**`.

## Package cache

The apt archives are cached in `test/fixtures/cache/apt/archives` and survive
runs, because the OpenSearch deb alone is 1.1 GB. The cache is not pruned; it
currently holds roughly 950 MB (158 archives) after runs of both suites and
grows with every package version. Deleting the directory is always safe; CI
keys an `actions/cache` entry on the fixture files.

`chefignore` keeps the cache (and the logs) out of the cookbook, so ChefSpec and
Test Kitchen do not checksum 950 MB per example; without it every ChefSpec
example takes about two seconds longer.
