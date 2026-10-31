# Microwave Cookbook Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Debian 13 Chef cookbook that installs Microwave Server against PostgreSQL 19 and OpenSearch 3.9, on one node or split across three, with ChefSpec and Test Kitchen proving it.

**Architecture:** Three custom resources carry the work (`microwave_server`, `microwave_postgresql`, `microwave_opensearch`); thin recipes set attributes and declare them, and `default` is the all-in-one composition. Backends serve TLS from cookbook-generated material, the server trusts the CA through the system store, and the package comes from the newest GitHub release with its published digest verified.

**Tech Stack:** Chef Infra/Cinc 19.4.46, Debian 13, PostgreSQL 19 (PGDG), OpenSearch 3.9.0, kitchen-dokken, ChefSpec, InSpec, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-10-31-microwave-cookbook-design.md`

## Global Constraints

- Debian 13 (`trixie`) only; `metadata.rb` carries `chef_version '>= 19.4.46', '< 20'` (two arguments; one comma string is not a valid `Gem::Requirement`) and `unified_mode true` in every resource.
- PostgreSQL 19 from `https://apt.postgresql.org/pub/repos/apt`, suite `trixie-pgdg`, components `main 19`, key `ACCC4CF8`; package `postgresql-19`.
- OpenSearch **3.9.0** from `https://artifacts.opensearch.org/releases/bundle/opensearch/3.x/apt`, suite `stable`; package `opensearch`, pinned with `apt_preference`.
- Microwave package from `https://github.com/rmoriz/microwave-development/releases`; asset `microwave-server_<version>_<arch>.deb`, digest from the sibling `.sha256` (two columns, first field).
- `version` defaults to `latest`; `arch` from `node['kernel']['machine']`, `amd64`/`arm64` only.
- Configuration at `/etc/microwave/microwave.toml`, mode `0640`, owner `root:microwave`; data directory `/var/lib/microwave`; unit `microwave-server.service`; package installed with `--force-confold`.
- Database password file `/etc/microwave/db.password`, mode `0640`, owner `root:microwave`.
- No Supermarket cookbook dependencies.
- All text in files and the cookbook README in English.
- Docker stacks are owned by one compose file and torn down with `down -v --rmi local`.

## Review Focus

- `bind` left at `0.0.0.0` makes every presigned `/bookshelf` URL unusable; the default must be the node's hostname and the template must carry it.
- `latest` resolution meets API rate limits, draft or prerelease releases and missing assets; it must fail with a message naming what was missing, and honour `ENV['GITHUB_TOKEN']`.
- A template that forgets `paths.data_dir` sends the server to `./data` under `ProtectSystem=strict` and the service fails to start.
- A re-converge where the password file is unreadable must not lock the role out; the file is authoritative and only a missing file rotates.
- Certificates without a SAN for the hostname the server dials break the split topology; the cert helper must always add `DNS:<hostname>`.

---

### Task 1: Cookbook skeleton and spec harness

**Files:**
- Create: `cookbooks/microwave/metadata.rb`
- Create: `cookbooks/microwave/README.md`
- Create: `cookbooks/microwave/attributes/default.rb`
- Create: `cookbooks/microwave/Gemfile`, `cookbooks/microwave/Rakefile`
- Create: `cookbooks/microwave/spec/spec_helper.rb`, `cookbooks/microwave/.rspec`
- Create: `cookbooks/microwave/.gitignore`
- Test: `cookbooks/microwave/spec/metadata_spec.rb`

**Interfaces:**
- Produces: the cookbook `microwave`, `chef_version '>= 19.4.46, < 20'`, `supports 'debian'`; `node['microwave']` with `bind`, `public_url`, `database_name`, `database_user`, `database_host`, `database_port`, `search_host`, `opensearch_heap`, `client_network`, `create_organization`; a working `bundle exec rspec`.

- [ ] **Step 1: Write metadata, attributes, Gemfile and harness**

`metadata.rb`: `name 'microwave'`, `maintainer 'Roland Moriz'`, `license 'AGPL-3.0'`, `chef_version '>= 19.4.46', '< 20'`, `supports 'debian', '>= 13.0'`, `recipe 'microwave::default', 'Server and backends on one node'`, `recipe 'microwave::server'`, `recipe 'microwave::database'`, `recipe 'microwave::search'`.

`attributes/default.rb`:

```ruby
default['microwave']['bind'] = node['fqdn'].to_s.empty? ? node['hostname'] : node['fqdn']
default['microwave']['public_url'] = nil
default['microwave']['database_name'] = 'microwave'
default['microwave']['database_user'] = 'microwave'
default['microwave']['database_host'] = nil
default['microwave']['database_port'] = 5432
default['microwave']['search_host'] = nil
default['microwave']['opensearch_heap'] = '1g'
default['microwave']['client_network'] = nil
default['microwave']['create_organization'] = false
```

`Gemfile`: `source 'https://rubygems.org'`, `gem 'chefspec', '~> 9.3'`, `gem 'rake'`.

`spec/spec_helper.rb`: require `chefspec`, set `RSpec.configure { |c| c.cookbook_path = File.expand_path('..', __dir__); c.platform = 'debian'; c.version = '13' }`.

`Rakefile`: default task `:spec`, `require 'rspec/core/rake_task'`.

`.gitignore`: `.kitchen/`, `Berksfile.lock`, `spec/examples.txt`, `Gemfile.lock`.

**Note on Gemfile.lock:** the design commits neither lockfile; keep it ignored.

- [ ] **Step 2: Write the failing spec**

```ruby
require 'spec_helper'

describe 'microwave metadata' do
  it 'supports debian 13 and the client floor' do
    meta = Chef::Cookbook::Metadata.new
    expect(File.read('metadata.rb')).to match(/chef_version '>= 19\.4\.46'/)
    expect(meta).to be_truthy
  end
end
```

- [ ] **Step 3: Run it**

Run: `cd cookbooks/microwave && bundle install && bundle exec rspec spec/metadata_spec.rb`
Expected: PASS (the skeleton from Step 1 satisfies it).

- [ ] **Step 4: Commit**

```bash
git add cookbooks/microwave
git commit -m "feat(cookbook): add the cookbook skeleton and spec harness"
```

---

### Task 2: Backend TLS helper

**Files:**
- Create: `cookbooks/microwave/libraries/backend_tls.rb`
- Test: `cookbooks/microwave/spec/backend_tls_spec.rb`

**Interfaces:**
- Produces: `Microwave::TLS.ca_cert`, `Microwave::TLS.ca_key`, `Microwave::TLS.cert(hostname)`, `Microwave::TLS.key(hostname)` — paths under `/etc/microwave/tls/`; `Microwave::TLS.ca_dir` = `/etc/microwave/tls`; a resource body calls `openssl` with `-subj "/CN=<hostname>"` and `-addext "subjectAltName=DNS:<hostname>,IP:127.0.0.1"`.

- [ ] **Step 1: Write the failing spec**

```ruby
require 'spec_helper'

describe 'Microwave::TLS' do
  it 'names the CA and per-host files under /etc/microwave/tls' do
    expect(Microwave::TLS.ca_dir).to eq('/etc/microwave/tls')
    expect(Microwave::TLS.ca_cert).to eq('/etc/microwave/tls/ca.crt')
    expect(Microwave::TLS.cert('db')).to eq('/etc/microwave/tls/db.crt')
    expect(Microwave::TLS.key('db')).to eq('/etc/microwave/tls/db.key')
  end
end
```

- [ ] **Step 2: Run it**

Run: `bundle exec rspec spec/backend_tls_spec.rb`
Expected: FAIL with `uninitialized constant Microwave`.

- [ ] **Step 3: Implement the module**

```ruby
module Microwave
  module TLS
    DIR = '/etc/microwave/tls'.freeze

    def self.ca_dir
      DIR
    end

    def self.ca_cert
      File.join(DIR, 'ca.crt')
    end

    def self.ca_key
      File.join(DIR, 'ca.key')
    end

    def self.cert(hostname)
      File.join(DIR, "#{hostname}.crt")
    end

    def self.key(hostname)
      File.join(DIR, "#{hostname}.key")
    end
  end
end
```

- [ ] **Step 4: Run it** — expect PASS.

- [ ] **Step 5: Commit**

```bash
git add cookbooks/microwave/libraries/backend_tls.rb cookbooks/microwave/spec/backend_tls_spec.rb
git commit -m "feat(cookbook): add the backend TLS path helper"
```

---

### Task 3: `microwave_server` resource

**Files:**
- Create: `cookbooks/microwave/libraries/release.rb`
- Create: `cookbooks/microwave/resources/server.rb`
- Create: `cookbooks/microwave/templates/microwave.toml.erb`
- Create: `cookbooks/microwave/templates/microwave-server.service.d/ordering.conf.erb`
- Test: `cookbooks/microwave/spec/server_resource_spec.rb`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: resource `microwave_server` with properties `database_url` (String, required), `search_url` (String, required), `bind` (String, default `node['microwave']['bind']`), `public_url` (String), `version` (String, default `'latest'`, regex `\A(latest|\d+\.\d+\.\d+)\z`), `ca_certs` (Array, default `[]`); `Microwave::Release.asset(version:, arch:)` returning `{ url:, sha256: }`, which raises `Microwave::Release::Missing` with the release tag and the missing asset name.

- [ ] **Step 1: Write the failing specs** in `spec/server_resource_spec.rb`

```ruby
require 'spec_helper'

describe 'microwave_server' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
      node.nodename('chef.example.com')
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::server')
  end

  it 'installs the resolved latest package with the published digest' do
    expect(chef_run).to create_remote_file('/var/cache/microwave/microwave-server_0.1.0_amd64.deb')
    expect(chef_run).to install_dpkg_package('/var/cache/microwave/microwave-server_0.1.0_amd64.deb').with(options: '--force-confold')
  end

  it 'renders the data directory, the URLs and the bind hostname' do
    expect(chef_run).to render_file('/etc/microwave/microwave.toml')
      .with_content(%r{^data_dir = "/var/lib/microwave"$})
      .with_content(%r{^url = "postgres://microwave:.*@db\.example\.com:5432/microwave\?sslmode=require"$})
      .with_content(%r{^url = "https://search\.example\.com:9200"$})
      .with_content(%r{^bind = "chef\.example\.com:8000"$})
  end

  it 'writes the configuration 0640 root:microwave' do
    expect(chef_run).to create_template('/etc/microwave/microwave.toml')
      .with(owner: 'root', group: 'microwave', mode: '0640')
  end

  it 'trusts the CA and refreshes the system store' do
    expect(chef_run).to create_file('/usr/local/share/ca-certificates/microwave-backend.crt')
    expect(chef_run).to run_execute('update-ca-certificates')
  end

  it 'enables and starts the service' do
    expect(chef_run).to enable_service('microwave-server')
    expect(chef_run).to start_service('microwave-server')
  end
end
```

Add `spec/fixtures/cookbooks/microwave-test/metadata.rb` (`name 'microwave-test'`, `depends 'microwave'`) and `recipes/server.rb` declaring the resource with the URLs above, `ca_certs ['CA-PEM']`.

- [ ] **Step 2: Run**

Run: `bundle exec rspec spec/server_resource_spec.rb`
Expected: FAIL `.converge` raises "could not find recipe".

- [ ] **Step 3: Implement `Microwave::Release`**

Behavior: for `version == 'latest'`, GET `https://api.github.com/repos/rmoriz/microwave-development/releases/latest` with headers `Accept: application/vnd.github+json` and `Authorization: Bearer <ENV['GITHUB_TOKEN']>` when set (use `Chef::HTTP::Simple`); take `tag_name`; for a concrete version use `v<version>`. Fetch `https://api.github.com/repos/rmoriz/microwave-development/releases/tags/<tag>` and read `assets`. Pick `microwave-server_<version>_<arch>.deb`, then the digest from `<asset>.sha256` by downloading it and taking the first whitespace-separated field. Raise `Microwave::Release::Missing` naming the tag and asset when either is absent, and let `Chef::HTTP::Simple` errors through unchanged.

- [ ] **Step 4: Implement `resources/server.rb`**

`unified_mode true`; properties as in Interfaces; `action :install`:

1. `directory '/var/cache/microwave'` (root, 0755).
2. `remote_file` at `/var/cache/microwave/<asset>` with `source url`, `checksum sha256`, `mode '0644'`, `notifies :install, "dpkg_package[#{asset}]", :immediately`.
3. `dpkg_package` of the local path with `options '--force-confold'`, `notifies :restart, 'service[microwave-server]', :delayed`.
4. `apt_package 'nginx'` is not needed here.
5. `directory Microwave::TLS.ca_dir` (root, 0755).
6. For each entry of `ca_certs`, a `file` `/usr/local/share/ca-certificates/microwave-backend-<index>.crt` with content, `notifies :run, 'execute[update-ca-certificates]', :delayed`.
7. `execute 'update-ca-certificates'` with `action :nothing`.
8. `template '/etc/microwave/microwave.toml'` (owner root, group microwave, mode 0640) with the variables, `notifies :restart, 'service[microwave-server]', :delayed`.
9. `directory '/etc/systemd/system/microwave-server.service.d'` (root, 0755) and `template '/etc/systemd/system/microwave-server.service.d/ordering.conf'`, notifying `execute[systemctl daemon-reload]`.
10. `execute 'systemctl daemon-reload'` with `action :nothing`.
11. `service 'microwave-server'` with `action [:enable, :start]`.

The drop-in:

```
[Unit]
After=postgresql.service opensearch.service
Wants=postgresql.service opensearch.service

[Service]
LimitMEMLOCK=infinity
```

`templates/microwave.toml.erb` renders exactly: `[server]` with `bind` and an optional `public_url`, `[paths]` `data_dir = "/var/lib/microwave"`, `[object_store]` `backend = "local"`, `[database]` `driver = "postgres"` and the URL, `[search]` `backend = "opensearch"` and the URL — no commented example blocks.

- [ ] **Step 5: Run the specs and fix until green**

Run: `bundle exec rspec spec/server_resource_spec.rb`. Expected: PASS. Add two specs: a concrete `version '0.1.0'` resolves through the tags endpoint (stub both endpoints), and a release without the `.deb` asset raises `Microwave::Release::Missing` naming the tag and the asset.

- [ ] **Step 6: Commit**

```bash
git add cookbooks/microwave/resources/server.rb cookbooks/microwave/libraries/release.rb cookbooks/microwave/templates cookbooks/microwave/spec
git commit -m "feat(cookbook): add the microwave_server resource"
```

---

### Task 4: `microwave_postgresql` resource

**Files:**
- Create: `cookbooks/microwave/resources/postgresql.rb`
- Create: `cookbooks/microwave/templates/postgresql-microwave.conf.erb`
- Create: `cookbooks/microwave/templates/pg_hba-microwave.conf.erb`
- Create: `cookbooks/microwave/templates/backend-cert.cnf.erb` (shared with Task 5)
- Test: `cookbooks/microwave/spec/postgresql_resource_spec.rb`
- Modify: `cookbooks/microwave/spec/fixtures/cookbooks/microwave-test/recipes/database.rb`

**Interfaces:**
- Produces: resource `microwave_postgresql` with properties `hostname` (String, default `node['fqdn']`), `port` (Integer, default 5432), `database` (default `node['microwave']['database_name']`), `user` (default `node['microwave']['database_user']`), `client_network` (String, default `node['microwave']['client_network']`), `ca_certs` (Array, default `[]`); writes the password to `/etc/microwave/db.password` and exposes `/etc/microwave/tls/db.crt` to the operator.

- [ ] **Step 1: Write the failing specs**

Cover: the PGDG source line and key, the `apt_preference` pin at `19`, the password file created 0640 with 32 hex characters on first run, no rewrite on a second run when the file exists, role and database creation guarded by `psql --list`/`\du`, `ssl = on` with the cert paths, an `hostssl` `scram-sha-256` line only when `client_network` is set, and `/etc/postgresql/19/main/conf.d/microwave.conf` plus `postgresql.service` enabled and started.

- [ ] **Step 2: Run** — expect FAIL.

- [ ] **Step 3: Implement the resource**

Steps: `apt_repository 'pgdg'` (uri `https://apt.postgresql.org/pub/repos/apt`, distribution `trixie-pgdg`, components `['main', '19']`, key `https://www.postgresql.org/media/keys/ACCC4CF8.asc`), `apt_preference 'postgresql-19'` (`pin 'version 19*'`, `pin_priority '600'`), `apt_package 'postgresql-19'`, directory `Microwave::TLS.ca_dir`, `openssl` commands for the CA (`/etc/microwave/tls/ca.key`, `ca.crt`, `-days 3650`, `-subj "/CN=microwave-ca"`) and the server cert (`db.key`, `db.crt`, `-addext "subjectAltName=DNS:#{hostname},IP:127.0.0.1"`), each with `not_if { ::File.exist?(...) }`, then the password file:

```ruby
file '/etc/microwave/db.password' do
  content "#{SecureRandom.hex(16)}\n"
  owner 'root'
  group 'microwave'
  mode '0640'
  not_if { ::File.exist?('/etc/microwave/db.password') }
end
```

Then the `conf.d` template (`ssl = on`, `ssl_cert_file`, `ssl_key_file`, `listen_addresses` = `'127.0.0.1'` or `'*'` when a client network is set), the `pg_hba` template appending `hostssl <db> <user> <client_network> scram-sha-256` when set, guarded `execute` blocks for `CREATE ROLE`/`ALTER ROLE ... PASSWORD` (rotation when the file was just created) and `CREATE DATABASE ... OWNER`, a `service 'postgresql'` (`[:enable, :start]`), and notifications restarting `postgresql` on config or cert change.

- [ ] **Step 4: Run the specs** — expect PASS. Add a spec that with `client_network '10.0.0.0/8'` the rendered `pg_hba` has the `hostssl` line and `listen_addresses = '*'`.

- [ ] **Step 5: Commit**

```bash
git add cookbooks/microwave/resources/postgresql.rb cookbooks/microwave/templates cookbooks/microwave/spec
git commit -m "feat(cookbook): add the microwave_postgresql resource"
```

---

### Task 5: `microwave_opensearch` resource

**Files:**
- Create: `cookbooks/microwave/resources/opensearch.rb`
- Create: `cookbooks/microwave/templates/opensearch.yml.erb`
- Create: `cookbooks/microwave/templates/heap.options.erb`
- Create: `cookbooks/microwave/templates/opensearch-proxy.nginx.erb`
- Test: `cookbooks/microwave/spec/opensearch_resource_spec.rb`
- Modify: `cookbooks/microwave/spec/fixtures/cookbooks/microwave-test/recipes/search.rb`

**Interfaces:**
- Produces: resource `microwave_opensearch` with `hostname` (default `node['fqdn']`), `heap` (default `node['microwave']['opensearch_heap']`), `client_network` (default `node['microwave']['client_network']`); writes `/etc/microwave/tls/search.crt` and the CA, and serves TLS on port 9200 through nginx.

- [ ] **Step 1: Write the failing specs**

Cover: `apt_repository 'opensearch'` with the 3.x apt URL and the artifacts key, `apt_preference` pin `3.9.0`, `apt_package 'opensearch'`, `opensearch.yml` carrying `plugins.security.disabled: true`, `network.host: 127.0.0.1`, `http.port: 9200`, `discovery.type: single-node`, the heap file content, the sysctl `vm.max_map_count` at 262144, nginx installed with a TLS server block proxying to `127.0.0.1:9200`, `opensearch` and `nginx` enabled and started.

- [ ] **Step 2: Run** — expect FAIL.

- [ ] **Step 3: Implement**

Mirror Task 4 for the repo, pin, package, CA/cert (`search.key`, `search.crt`) and service; add `sysctl 'vm.max_map_count'` with the value and `notifies :restart, 'service[opensearch]', :delayed`; the nginx vhost listens on `127.0.0.1:9200` when no client network is set and on `<hostname>:9200` otherwise, `ssl_certificate` the search cert, `proxy_pass http://127.0.0.1:9200`; set an `satisfy`-free `execute` guard that the security plugin is off.

- [ ] **Step 4: Run the specs** — expect PASS.

- [ ] **Step 5: Commit**

```bash
git add cookbooks/microwave/resources/opensearch.rb cookbooks/microwave/templates cookbooks/microwave/spec
git commit -m "feat(cookbook): add the microwave_opensearch resource"
```

---

### Task 6: Recipes and defaults

**Files:**
- Create: `cookbooks/microwave/recipes/default.rb`, `server.rb`, `database.rb`, `search.rb`
- Test: `cookbooks/microwave/spec/recipes_spec.rb`

**Interfaces:**
- Consumes: the three resources.
- Produces: `microwave::default` converges all three with loopback URLs (`http://127.0.0.1:9200`, `postgres://...@127.0.0.1`); `microwave::server` needs `node['microwave']['database_host']` and `search_host` set by the wrapper; `microwave::database` and `microwave::search` converge one backend.

- [ ] **Step 1: Write the failing specs**

`default` declares `microwave_postgresql`, `microwave_opensearch` and `microwave_server` in that order with `bind` `#{node['fqdn']}:8000` and a `search_url` of `http://127.0.0.1:9200`; `server` raises when `database_host` is unset; `database`/`search` declare only their resource; `create_organization` adds a `microwave_organization` execute when true.

- [ ] **Step 2: Run** — expect FAIL.

- [ ] **Step 3: Implement the four recipes**

`default.rb` converges `database`, `search`, then `server` with the loopback URLs and `sslmode=require`. `server.rb` validates the host attributes and declares `microwave_server` with `postgres://#{user}:#{password}@#{database_host}:#{port}/#{db}?sslmode=require` read from the password file, `search_url` `https://#{search_host}:9200`, and `ca_certs [::File.read(Microwave::TLS.ca_cert)]` when the CA file exists. `database.rb` and `search.rb` declare their resource from attributes.

- [ ] **Step 4: Run all specs** — `bundle exec rspec` expects PASS.

- [ ] **Step 5: Commit**

```bash
git add cookbooks/microwave/recipes cookbooks/microwave/spec
git commit -m "feat(cookbook): add the composition recipes"
```

---

### Task 7: Kitchen suites and verifiers

**Files:**
- Create: `cookbooks/microwave/.kitchen.yml`
- Create: `cookbooks/microwave/test/fixtures/docker-compose.yml`, `test/fixtures/kitchen_probe/` (metadata plus an empty `recipes/default.rb`)
- Create: `cookbooks/microwave/test/hooks/pre_converge.sh`, `test/hooks/post_destroy.sh`
- Create: `cookbooks/microwave/test/integration/all-in-one/default_test.rb`, `test/integration/split/default_test.rb`
- Create: `cookbooks/microwave/test/integration/helpers.rb`

**Interfaces:**
- Consumes: `microwave::default` and the three recipes.
- Produces: `kitchen test all-in-one` and `kitchen test split` green on a workstation with Docker.

- [ ] **Step 1: Write `.kitchen.yml`**

Driver `dokken`, `platforms: [{ name: debian-13 }]`, provisioner `name: chef_zero`, `product_name: cinc`, `product_version: 19.4.46`, `deprecations_as_errors: true`, `enforce_idempotency: true`; suite `all-in-one` (`run_list ['microwave::default']`, `create_organization true`); suite `split` (`run_list ['microwave::server']`, attributes `database_host: db.microwave.test`, `search_host: search.microwave.test`, with `pre_converge` starting `test/fixtures/docker-compose.yml` and `post_destroy` running `down -v --rmi local`).

- [ ] **Step 2: Write the split fixture**

Compose network `microwave-test`, services `db` (image `debian:13`, `/etc/hosts` alias `db.microwave.test`, converging `microwave::database` with `client_network 172.16.238.0/24`) and `search` (the same for `microwave::search`), both with their CA copied into a shared volume. The server node mounts that volume at `/etc/microwave/backend-ca`; `pre_converge` copies the CA files into the node and drops a wrapper recipe attribute.

- [ ] **Step 3: Write the verifiers**

Shared helper: `status(url)` returning the body and code of `GET /_status`. `all-in-one` asserts: `postgresql` and `opensearch` and `microwave-server` running, `/_status` 200, config mode 0640, `psql "sslmode=require"` connects, `curl --cacert /etc/microwave/tls/ca.crt https://127.0.0.1:9200/_cluster/health` returns green or yellow, `chef-server-ctl org-create demo --association_user pivotal -f /tmp/demo.pem` succeeds, `knife cookbook upload kitchen_probe` succeeds and `knife node list` contains the node. `split` repeats those against the remote backends and asserts the `conf.d` files carry the remote listener.

The final node converge: write `/etc/chef/client.rb` with the server URL, `validation_client_name 'demo-validator'`, `validation_key '/tmp/demo-validator.pem'` and `ssl_verify_mode :verify_none`, then `cinc-client --node-name probe` and assert the run succeeds and `knife node show probe` lists `kitchen_probe`.

- [ ] **Step 4: Run both suites**

Run: `kitchen test all-in-one` and `kitchen test split`
Expected: converge, verify and idempotency checks pass, no container or image left behind.

- [ ] **Step 5: Commit**

```bash
git add cookbooks/microwave/.kitchen.yml cookbooks/microwave/test
git commit -m "test(cookbook): verify both topologies with kitchen"
```

---

### Task 8: CI workflow and documentation

**Files:**
- Create: `.github/workflows/cookbook.yml`
- Modify: `cookbooks/microwave/README.md`
- Modify: `docs/deployment.md` (cookbook section)
- Modify: `AGENTS.md` (verification row)

**Interfaces:**
- Consumes: the suites and specs from Tasks 1-7.
- Produces: the `cookbook` workflow with a `paths` filter on `cookbooks/**`.

- [ ] **Step 1: Write the workflow**

`on: pull_request` and `on: push: branches: [main]` plus a `paths` filter `cookbooks/**` and `.github/workflows/cookbook.yml`; job `changes` on `dorny/paths-filter`; job `chefspec` (ruby 3.3, `bundle install`, `bundle exec rspec`) and job `kitchen` (matrix over the two suites, `timeout-minutes: 45`, `uses: actions/checkout`, installs docker compose, runs `kitchen test ${{ matrix.suite }}`), both conditioned on `changes.outputs.cookbook == 'true'`.

- [ ] **Step 2: Document**

README: what the resources do, the two topologies, attribute table, the TLS and `sslmode=require` limit, and how to run `bundle exec rspec` and the two kitchen suites. `docs/deployment.md` gets a short section pointing at the cookbook. `AGENTS.md` gets the row `cookbooks/**` → `cd cookbooks/microwave && bundle exec rspec` and the two kitchen suites.

- [ ] **Step 3: Verify**

Run: `python3 scripts/check-claims.py` and `gh workflow view cookbook.yml`. Then push a branch that touches `cookbooks/` and confirm the `cookbook` workflow runs; touch only `website/` and confirm it does not.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/cookbook.yml cookbooks/microwave/README.md docs/deployment.md AGENTS.md
git commit -m "ci(cookbook): test the cookbook when it changes"
```
