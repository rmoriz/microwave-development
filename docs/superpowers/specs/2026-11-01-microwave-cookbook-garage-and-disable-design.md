# Cookbook: Garage backend and switchable services

Date: 2026-11-01
Status: draft for approval
Extends: `docs/superpowers/specs/2026-10-31-microwave-cookbook-design.md`

## Intent

The cookbook installs Microwave Server with PostgreSQL 19 and OpenSearch 3.9,
all-in-one or split, with TLS on every backend hop. Two gaps remain:

1. **No object store off the server host.** The template hardcodes
   `[object_store] backend = "local"`, and local `/bookshelf` URLs are built
   from `server.bind` (`docs/deployment.md`, "The local object store caveat"),
   so a split deployment serves presigned URLs nobody outside the node can use;
   the cookbook's Garage endpoint therefore follows the certificate name, not
   `server.bind`.
   Garage is already a first-class backend in the server
   (`ObjectStoreBackend::Garage`, `microwave-config/src/config.rs:317`) and
   `packaging/` carries an evaluation-only compose profile for it. The cookbook
   needs a real resource: single node, `replication_factor = 1`, TLS, and the
   bucket/key provisioning the server cannot do itself.
2. **Services cannot be switched off.** Removing a service today means editing
   the run list and hoping nothing is left running. Operators want a supported
   way to stop all four services and keep them stopped — for a migration, a
   decommission, or maintenance — with packages, configuration and data intact.

## Goals

- `microwave_garage` with the same shape as `microwave_postgresql` and
  `microwave_opensearch`: system user, data directories, unit, configuration,
  TLS material from the cookbook CA, nginx terminating TLS on 3900 with the
  engine on `127.0.0.1:3910`, and `s3_api.public_endpoint` pointing at the TLS
  URL so presigned URLs are valid off-host.
- One-time, guarded provisioning of the layout (zone `dc1`, capacity from an
  attribute), the bucket and the S3 key; the key material lands in
  `/etc/microwave/garage.access_key` and `/etc/microwave/garage.secret_key`
  (`0640 root:microwave`), the same shape as `db.password`.
- `microwave_server` gains the object-store properties and renders
  `[object_store]` from them (`backend`, `bucket`, `endpoint`, `region`,
  `path_style`, `access_key`, `secret_key`); `secret` stays local-only.
- `action :disable` and `action :enable` on all four resources. `:disable`
  stops the unit, disables it and drops a per-unit marker; nothing else — no
  package removal, no data deletion, no `mask`. `:enable` removes the marker,
  enables and starts.
- **A disable sticks.** While the marker ` /etc/microwave/disabled/<unit>`
  exists, `action :install` skips its `enable`/`start` (and its delayed-restart
  notifications do not start the unit), so a later converge of
  `microwave::default` leaves the service down. Only `:enable` or
  `microwave::enable` brings it back. This is what makes the action usable for
  a decommission or a migration instead of for one run.
- `microwave::garage` for a backend-only node, `microwave::disable` for all
  four services in reverse dependency order (server, search, database, garage)
  plus nginx, `microwave::enable` as the inverse, and `recipes/default.rb`
  includes garage only when `node['microwave']['object_store_backend'] ==
  'garage'`.
- Both topologies proven: the `split` suite runs garage as a third backend and
  the knife upload goes through the server into Garage; a new `disable` suite
  converges `microwave::disable`, asserts every unit `inactive`/`disabled`, the
  data directories and database rows intact, and then converges
  `microwave::default` again to prove they stay down.

## Non-goals

- Multi-node Garage clusters: no `bootstrap_peers`, no multi-zone layout, no
  replication factor above 1, no gateway nodes. A real cluster stays
  out-of-band; the docs say so.
- The `[s3_web]` website endpoint beyond a loopback bind.
- Migrating existing `/bookshelf` objects into Garage. The object keys are
  identical, but the copy itself is an operator step (documented).
- Anything that deletes: no `:remove`/`:purge` action, no automatic pruning of
  old binaries, buckets or backups.

## Anti-goals

- A service that stops but starts again on the next converge: `:disable` must
  survive a later converge of another resource on the same node.
- A garage resource that pretends to be production-ready: the docs must not
  suggest `replication_factor = 1` or a single node is durable.
- Losing the existing local default. An existing all-in-one installation must
  keep working unchanged after this change.

## Constraints

- Debian 13 only; Cinc 19.4.46; no Supermarket dependencies.
- **The server cannot verify a private CA on the S3 hop today.** The object
  store builds its client with ureq and ureq's bundled Mozilla roots
  (`crates/microwave-objectstore/src/s3.rs:101`); no system trust store, no
  `SSL_CERT_FILE`. The search hop only works because reqwest verifies through
  `rustls-platform-verifier`. The workspace therefore enables ureq's
  `platform-verifier` feature (`Cargo.toml:67`), which makes the S3 client use
  the OS store and thereby the cookbook CA that `update-ca-certificates`
  installs, same as the database hop. A test covers a server certificate from a
  throwaway CA trusted only through `SSL_CERT_FILE`.
- Garage publishes no Debian package — only static musl binaries per
  architecture (`https://garagehq.deuxfleurs.fr/_releases/v2.5.0/<arch>/garage`)
  and no checksum next to them. The cookbook pins the version and carries the
  two digests it recorded itself:
  `x86_64` `2ff667286217cd73ebcb1c03af993822365000441dfe31a80cb64c5189a9dea8`,
  `aarch64` `69b6040cef428b993500c17dd0d0c1240acf2f3f667b9b05ceba4b9660db4e8c`.
  `remote_file` verifies with `checksum`, and the README says where they came
  from and how to re-record them on a bump.
- Garage needs no TLS of its own; nginx terminates, following the OpenSearch
  pattern (engine on a loopback port, proxy on the public one).
- Secrets: `rpc_secret_file` and `admin_token_file` keep both out of the TOML,
  so it does not need `sensitive`.
- Every hop stays idempotent; a second converge reports 0 changed resources in
  all three suites.
- CI: the workflow matrix grows by the `disable` suite, `cookbooks/**` stays
  the only trigger, `workflow_dispatch` gains the suite choice.

## Approaches

### Garage installation

| Way | Trade-off |
| --- | --- |
| **Binary from the release URL, version + recorded digest (picked)** | Matches the server resource; no build toolchain on the node; costs two digests we must record and bump by hand, because upstream publishes none |
| Build from source | Reproducible from a tag, but a Rust build per run; minutes of extra time per suite |
| Container | Needs Docker as a second runtime on a package-based node, and a compose/naming story for the binder |

### TLS to the S3 endpoint

Garage speaks plain HTTP on `api_bind_addr` and documents a reverse proxy for
TLS, so the OpenSearch shape is the only one that keeps the split promise:
engine on `127.0.0.1:3910`, nginx `listen <hostname>:3900 ssl`, certificate from
the cookbook CA. Serving plain 3900 would be cheaper but break the rule that
every backend hop is TLS.

### Proving the disable actions

| Way | Trade-off |
| --- | --- |
| **Own `disable` suite** (picked) | Proves `inactive`/`disabled` per unit plus intact data on a real systemd; costs ~3 minutes and one more matrix entry |
| ChefSpec only | Fast, but ChefSpec asserts the resource collection, not the unit state; the "kept stopped" claim would be untested |

## Design

### `microwave_garage`

Properties: `hostname` (as in the other resources), `port` (default 3900),
`capacity` (layout, default `1G`), `bucket` (default `chef-cookbooks`),
`region` (default `garage`), `client_network`.

`action :install`:

1. `group 'microwave'` (system) — the key files are `0640 root:microwave`,
   and the group otherwise only exists once the database resource ran, which
   a garage-only node does not have.
2. `group`/`user` `garage` (system, no home, `/usr/sbin/nologin`).
3. Directories `/var/lib/garage/meta`, `/var/lib/garage/data` (`0700`,
   `garage:garage`), `/etc/microwave` (`0755`).
4. `remote_file /usr/local/bin/garage` with the arch URL and digest, `0755`,
   notifies the unit restart.
5. `/etc/microwave/garage.rpc_secret` and `/etc/microwave/garage.admin_token`
   from `SecureRandom.hex(32)`, `0600 garage:garage`, written only while
   missing. Garage rejects any secret file with a group or other bit
   (`mode & 0o077 != 0` → "world-readable, expected 0600"), so `0640` cannot be
   used and the owner has to be the engine user. The S3 key pair stays
   `0640 root:microwave` because the server reads it.
6. `/etc/garage.toml` (`0640 root:garage`) from a template: `replication_factor
   = 1`, `db_engine = "sqlite"`, meta/data dirs, `rpc_bind_addr [::]:3901`,
   `rpc_public_addr <hostname>:3901`, `rpc_secret_file`, `[s3_api]`
   `api_bind_addr 127.0.0.1:3910`, `s3_region`, `public_endpoint
   https://<hostname>:3900`, `public_endpoint_path_style = true`,
   `[admin] api_bind_addr 127.0.0.1:3903`, `admin_token_file`.
7. `/etc/systemd/system/garage.service` from a template, the one unit the
   cookbook writes itself: `User`/`Group` `garage`, `ExecStart
   /usr/local/bin/garage -c /etc/garage.toml server`, `Restart=on-failure`,
   `TimeoutStopSec`, `GARAGE_LOG_TO_JOURNALD=1`, `StateDirectory` omitted in
   favour of the directories above, no `ProtectSystem`/`ReadWritePaths` that
   would fight them. On a node without an init the service resource starts the
   daemon directly through `libraries/direct_service.rb`, which OpenSearch
   shares.
8. The TLS block as in `postgresql.rb`/`opensearch.rb`: CA key/cert, key
   `Microwave::TLS.key('garage')`, CSR, SAN `DNS:<hostname>,IP:127.0.0.1`,
   certificate, `backend-cert.cnf`. Key `0600` `garage:garage`, cert `0640`.
9. nginx site `microwave-garage` on `<hostname>:3900 ssl` and `127.0.0.1:3900`,
   proxy to `127.0.0.1:3910` with `client_max_body_size 0`, `proxy_request_
   buffering off`, long read timeouts (cookbook blobs can be large).
10. `service 'garage'` `%i(enable start)`, guarded by the disable marker.
11. Guarded provisioning, all as user `garage`, all `not_if` on the CLI state:
   `layout assign -z dc1 -c <capacity> "$(node id -q | cut -d@ -f1)"`,
   `layout apply --version 1`, `bucket create <bucket>`,
   `key import --yes <access> <secret>`, `bucket allow --read --write --owner`.
   The assign records the capacity in `/var/lib/garage/capacity` and guards on
   that exact string, because the layout table renders capacities humanised and
   rounded (`2G` shows as `1.9 GiB`).
   The key pair is generated by us (`SecureRandom.hex(20)`/`hex(32)`) into
   `/etc/microwave/garage.access_key`/`.secret_key`, `0640 root:microwave`.
12. `execute 'systemctl daemon-reload garage'` (guarded like the other two).

`action :disable` and `action :enable` only touch the unit and its marker.

### Disable

Each resource gets:

```ruby
action :disable do
  file "/etc/microwave/disabled/<unit>"
  service '<unit>' do
    action %i(stop disable)
  end
end

action :enable do
  file "/etc/microwave/disabled/<unit>" do
    action :delete
  end
  service '<unit>' do
    action %i(enable start)
  end
end
```

The marker directory is created by the resources (`0755 root:root`), the unit is
guarded with the same `only_if` the resources already use for an absent unit, so
`microwave::disable` on a server-only node does not fail. `action :install`'s
`enable`/`start` and the two `service` notification targets skip a marked unit,
so neither a re-converge nor a delayed restart wakes it up again.

`recipes/disable.rb` disables server, search, database and garage in that order
and nginx last: nginx is shared by both proxies and would otherwise keep
listening on 9200 and 3900 and answer 502. `recipes/enable.rb` is the inverse;
its nginx line runs first. A re-enabled backend does not implicitly start
nginx, so `microwave::enable` belongs to the same node's run list.

### `microwave_server`

New properties: `object_store_backend` (`local`/`s3`/`garage`, default from the
attribute), `object_store_bucket`, `object_store_endpoint`,
`object_store_region`, `object_store_path_style`, `object_store_access_key_file`,
`object_store_secret_key_file`. The template renders `[object_store]` for a
non-local backend and reads the two key files lazily, like `db.password`.
`database_url`/`search_url` lose `required: true` and are validated at the top
of `action :install`, so `:disable` works on a node whose attributes did not
carry them.

### Recipes and attributes

Attributes: `object_store_backend` (`local`), `object_store_bucket`
(`chef-cookbooks`), `object_store_region` (`garage`), `object_store_endpoint`,
`object_store_access_key_file`, `object_store_secret_key_file`,
`garage_version` (`2.5.0`), `garage_capacity` (`1G`), plus the per-arch digest
map. `recipes/default.rb` sets the object-store endpoint to the local garage
address and includes `microwave::garage` when the backend is `garage`; the
server recipe passes the properties through.

### Test Kitchen

- `split`: third service `garage` (`garage.microwave.test`, same node image),
  recipe `microwave-test::garage_node` → `include_recipe 'microwave::garage'`,
  publishes `garage.access_key`, `garage.secret_key` and `garage-ca.crt` to
  `/mnt/shared`. `search` gains a `search.converged` marker and a healthcheck;
  `garage` waits on it, the server waits on `garage.s3`. The suite sets
  `object_store_backend: garage` and the CA/key files under `/mnt/backend`.
- `all-in-one`: sets `object_store_backend: garage` as well, so the default
  composition is proven with a real backend instead of the local store.
- `disable`: run list `microwave::default` with the same attributes as
  `all-in-one`; the verifier converges `microwave::disable`, asserts
  `systemctl is-active` = `inactive` and `is-enabled` = `disabled` for all four
  units and that `systemctl show -p UnitFileState` says `disabled`, asserts the
  data survived (`psql` row still there, object still in the bucket), and then
  converges plain `microwave::default` again and re-asserts `inactive`/
  `disabled` for every unit — that is the stickiness proof. A last control
  converges `microwave::enable` and asserts all four back at `active`.
- Verifier additions in `split`: `/_status` reports the `bookshelf` upstream as
  `pong`; the garage node fixture writes `garage bucket info <bucket>` to
  `/mnt/shared/garage-bucket.txt` (the admin API stays on loopback, so the
  server node cannot ask it) and one control asserts the key is listed there;
  the knife upload path still succeeds end to end and one control downloads the
  uploaded cookbook blob through the server, so the bytes really come from
  Garage.
- Rakefile task `kitchen:disable`; CI matrix gains `disable`.

### Docs

README: Garage in the resource table, the object-store attributes, `:disable`/
`:enable`, the digest provenance, and that a single node with `rp = 1` is for
evaluation and small installations. `docs/deployment.md`: replace the local
caveat with the Garage option and its limits. `AGENTS.md` row unchanged.

## Verification

- `bundle exec rspec` (new examples: every property, the `[object_store]`
  block for both non-local backends, the guards, `:disable`/`:enable` per
  resource, the marker guard on `:install`, the `microwave::disable` order),
  `bundle exec cookstyle --fail-level refactor`.
- `cargo test -p microwave-objectstore` with the new private-CA TLS test, plus
  `cargo test --workspace`.
- `bundle exec rake kitchen:all_in_one`, `kitchen:split`, `kitchen:disable`,
  each with a second converge at 0 changes.
- `actionlint`, `python3 scripts/check-claims.py`.

## Left to the builder

- Exact capacity unit and whether the layout command should pre-check
  `garage status` instead of trusting `|| true`.
- Whether the nginx buffering/timeouts need per-location tuning beyond the
  defaults named above.
- The precise ChefSpec idiom for the guarded CLI executes (stub the `not_if`
  commands, as the password spec does).
- Log capture and teardown details for the third backend follow the existing
  hooks.
