# Cookbook: Microwave Server on Debian 13 with PostgreSQL and OpenSearch

## Intent

A Chef cookbook in this repository that turns fresh Debian 13 nodes into a
running Microwave Server backed by PostgreSQL 19 and OpenSearch 3.9. The
resources compose: everything on one host, or each backend on its own host
with the server pointed at it. It is the deployment shape an ops team would
actually run and it gives the repository a real package-plus-backends path
next to the Docker e2e stack: the `.deb` from the release, the packaged
systemd unit, the packaged configuration file, a real database and a real
search engine.

## Goals

- One resource-oriented cookbook, `cookbooks/microwave`, `unified_mode true`,
  `chef_version '>= 19.4.46, < 20'`, Debian 13 (`trixie`) only. It carries
  three resources:
  - `microwave_server`: package, configuration, service. Properties for
    `database_url`, `search_url`, `bind`, `public_url`, `version`.
  - `microwave_postgresql`: PGDG repository, PostgreSQL 19, application role
    and database, generated password.
  - `microwave_opensearch`: OpenSearch 3.9, single-node configuration, heap.
- Three thin recipes expose them one at a time (`server.rb`, `database.rb`,
  `search.rb`) and `recipes/default.rb` composes all three on one node with
  loopback URLs.
- `version` accepts a release version or `latest`. `latest` resolves the
  newest GitHub release through the API, then downloads the `.deb` together
  with its published `.sha256` asset (two columns, first field is the digest)
  and verifies it. The test always uses `latest`, so it installs the current
  release artifact rather than a local build.
- PostgreSQL 19 from the PGDG repository `https://apt.postgresql.org/pub/repos/apt`,
  suite `trixie-pgdg`, components `main 19`, key `ACCC4CF8`; package
  `postgresql-19`. `postgresql-19` exists only in the `19` component.
- An application role `microwave` with a generated password and a database it
  owns. The server creates its own schema on first start; the role needs no
  superuser and no CREATEDB/CREATEROLE. The all-in-one recipe keeps PostgreSQL
  on `127.0.0.1`; the split recipe opens the listener for a client network and
  keeps the `scram-sha-256` host rule.
- Both backends serve TLS from a cookbook-generated CA and service
  certificate. The server trusts that CA: PostgreSQL through the system store
  and `sslmode=require` (sqlx verifies against webpki roots, so encryption is
  guaranteed but the peer certificate is not verified), OpenSearch through the
  same store, which its platform verifier does read.
- OpenSearch 3.9.0, pinned, from `https://artifacts.opensearch.org/releases/bundle/opensearch/3.x/apt`
  (suite `stable`, key from `artifacts.opensearch.org`), pinned through an
  `apt_preference` to 3.9.0. The package is distro-agnostic
  (`opensearch-3.9.0-linux-x64.deb`, about 1.09 GB) and bundles its JDK. The
  security plugin stays disabled because Microwave's OpenSearch client sends
  no credentials; an nginx in front terminates TLS, listens on `127.0.0.1` in
  the all-in-one recipe and on the client network when one is set.
- The Microwave package from the GitHub release
  (`microwave-server_<version>_<arch>.deb`) through `remote_file` plus
  `dpkg_package`, checksum pinned to the release's published `.sha256`.
- `/etc/microwave/microwave.toml`, mode `0640`, owner `root:microwave`, from a
  template: `server.bind`, optional `server.public_url`,
  `paths.data_dir = "/var/lib/microwave"`, `object_store.backend = "local"`,
  `database.driver = "postgres"`, `database.url` with the generated password,
  `search.backend = "opensearch"`, `search.url`.
- Idempotent: a second converge reports no resource changes.
- A ChefSpec suite for the resource logic and Test Kitchen (kitchen-dokken,
  platform `debian-13`, Cinc 19.4.46) with two suites, `all-in-one` and
  `split`, that prove the deployment: `/_status` healthy, an organization and
  client created with `chef-server-ctl`, a cookbook uploaded with `knife`, and
  a `cinc-client` run on a second node against the server.
- A `.github/workflows/cookbook.yml` job matrix (`chefspec`, `kitchen`) that
  runs whenever something under `cookbooks/**` changed.

## Non-goals

HA or multiple replicas, S3/Garage object storage, client-facing TLS and a
reverse proxy, mail, MCP, platform migration, Ubuntu/Fedora, Supermarket
publishing, pedant runs, backup automation.

## Anti-goals

- A cookbook that only works because a human did a step first.
- Diverging from the packaged layout: the unit `microwave-server.service`, the
  data directory `/var/lib/microwave` and the service account stay as shipped.
- Pulling Supermarket cookbooks whose distro and version assumptions we then
  have to debug.

## Constraints

- `microwave.toml` is a dpkg conffile. Deploy it with
  `--force-confold` so an upgrade cannot stop at a prompt.
- The package `postinst` creates the account, the directories and starts the
  unit. Order matters: backends first, then the package, the configuration,
  then `service` enable and start.
- The generated database password is persisted to
  `/etc/microwave/db.password` (`0640 root:microwave`) and read lazily, so
  later converges keep the same role password.
- OpenSearch needs a heap bound (`/etc/opensearch/jvm.options.d/heap.options`)
  and either a systemd `LimitMEMLOCK` override or `bootstrap.memory_lock`
  off; both are cookbook decisions, not operator chores.
- PostgreSQL serves TLS via `ssl = on` plus `ssl_cert_file`/`ssl_key_file` in
  `conf.d`; the OpenSearch nginx needs the same certificate and key.
- No Chef Server in the test: `cinc-client -z` with chef-zero, Cinc Client
  pinned to 19.4.46 from `packages.cinc.sh` with its published SHA256.
- Kitchen runs on kitchen-dokken with `product_name: cinc`,
  `product_version: 19.4.46`, so the CI runner needs Docker; the OpenSearch
  package makes a suite take several minutes.
- The certificate material lives under `/etc/microwave/tls/` on the server and
  is handed to the backends by the split suite through attributes, so both
  suites converge the same resources.
- Docker stacks follow `AGENTS.md`: owned by one compose file, torn down with
  `down -v --rmi local`.

## Approaches

- **A, self-contained (picked).** Own resources for both backends, the
  package, the template and the service. More code, no foreign assumptions,
  full control over the 3.9 details and the PGDG pin.
- **B, wrapper over Supermarket `postgresql` and `opensearch`.** Less code,
  but `postgresql` 13 is resource-only and carries pgdg-repo and version
  assumptions, and the OpenSearch cookbook is untested on trixie with 3.9.
- **C, tarballs instead of packages.** Avoids package repositories, but costs
  hand-written units, directories and sysctls for OpenSearch.

## How

- Attributes for the bind hostname, public URL, database name and user,
  OpenSearch heap, client network, and whether the test creates an
  organization. Architecture is derived from `node['kernel']['machine']` and
  validated against `amd64`/`arm64`.
- The thin recipes (`server.rb`, `database.rb`, `search.rb`, `default.rb`) set
  the attributes and declare the matching resource; the resources hold the
  logic.
- The `microwave_server` resource installs with `remote_file` + `dpkg_package`
  (`--force-confold`), renders the configuration, installs a trusted CA into
  the system store and enables the service; the two backend resources own
  their packages, certificates, configuration and services, and open their
  listener for the client network when one is set.
- A systemd drop-in orders the unit after `postgresql.service` and
  `opensearch.service` and raises `LimitMEMLOCK`; a `sysctl` resource sets
  `vm.max_map_count`.
- The database password is generated once and persisted to
  `/etc/microwave/db.password` (`0640 root:microwave`). The file wins; if the
  role exists but the file is gone, the role password is rotated to the newly
  generated one.
- The ChefSpec suite covers the resource logic without converging; the Kitchen
  suites converge the resources and run InSpec verifiers. The `split` suite
  starts its database and search containers from a compose fixture in a
  `pre_converge` hook and tears them down in `post_destroy`.
- CI gets `.github/workflows/cookbook.yml` with a `paths` filter on
  `cookbooks/**`, a `chefspec` job (Ruby, bundler, `rspec`) and a `kitchen`
  matrix job per suite, pinned to Cinc 19.4.46.

## Decisions on the open points

- A split deployment opens the backends for a client network and serves TLS;
  it is not limited to handing the server foreign URLs.
- The test covers both shapes, all-in-one and split.

## Left to the builder

Resource and recipe file layout, attribute names, default heap size, how the
verifier reports failures, and the exact knife and `cinc-client` invocations.
