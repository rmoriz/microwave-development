# Cookbook Garage Backend and Switchable Services Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the Debian cookbook a real Garage object-store backend and `:disable`/`:enable` actions that stop services without touching packages or data and keep them stopped across converges.

**Architecture:** A new `microwave_garage` resource mirrors `microwave_postgresql`/`microwave_opensearch`: system user, data directories, own unit, loopback engine, nginx TLS on the public port, and guarded one-time layout/bucket/key provisioning. `microwave_server` renders `[object_store]` for non-local backends and the server verifies the private CA through the OS trust store. A per-unit marker under `/etc/microwave/disabled` makes `:disable` sticky; only `:enable` or `microwave::enable` clears it.

**Tech Stack:** Chef Infra Client 19.4.46 (ChefSpec on 18.3), Debian 13, nginx, systemd, Garage 2.5.0 static binary, Rust workspace (`ureq` + `rustls-platform-verifier`), Test Kitchen with dokken and InSpec.

**Spec:** `docs/superpowers/specs/2026-11-01-microwave-cookbook-garage-and-disable-design.md`

## Global Constraints

- Debian 13 only, Chef `>= 19.4.46, < 20`, no Supermarket dependencies, `microwave` cookbook.
- Garage version is exactly `2.5.0`; binary URLs are `https://garagehq.deuxfleurs.fr/_releases/v2.5.0/<arch>/garage` with arch `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl`; digests are recorded, never fetched: `x86_64` `2ff667286217cd73ebcb1c03af993822365000441dfe31a80cb64c5189a9dea8`, `aarch64` `69b6040cef428b993500c17dd0d0c1240acf2f3f667b9b05ceba4b9660db4e8c`.
- Garage is single node, `replication_factor = 1`, `db_engine = "sqlite"`; engine S3 on `127.0.0.1:3910`, admin on `127.0.0.1:3903`, nginx TLS on `3900`, `public_endpoint_path_style = true`.
- `:disable` is exactly `stop` + `disable` + marker file; no `mask`, no package removal, no data deletion. `:enable` is marker delete + `enable` + `start`.
- Marks live in `/etc/microwave/disabled/<unit>`; while one exists, `:install` skips the unit's enable/start and a delayed restart must not wake it.
- Existing installs without object-store attributes keep `backend = "local"`.
- Secrets are one file under `/etc/microwave`, written only while missing (`grep -qs "[^[:space:]]"` guard style). The garage key files are `0640 root:microwave`; `rpc_secret_file` and `admin_token_file` are `0600 garage:garage`, because Garage refuses any secret file with a group or other bit (`mode & 0o077 != 0`, "world-readable, expected 0600").
- MCP stays `experimental`, writes on by default, English copy, no invented metrics.
- `README.md` of the cookbook and `docs/deployment.md` stay traceable to the repository or `docs/mcp.md`.
- Every converge must be idempotent: second runs report 0 changed resources in all suites. `post_destroy` ends on `docker compose ... down -v --rmi local`.

## Review Focus

- `microwave::disable` on a server-only node: three of the four units are absent there; every action must skip a missing unit instead of failing. Pinned by the `only_if` guard spec in Task 2.
- A delayed restart on a marked unit: the notification target must evaluate the marker guard, so a config change cannot start a disabled service. Pinned by the marker-guard spec in Task 2.
- An existing local install: no object-store attributes still renders `backend = "local"` and includes no garage. Pinned by the local-render spec in Task 4 and the default-composition spec in Task 5.
- A second converge of the garage resource: layout, bucket, key and permission commands must each have a real state guard, or the suite fails idempotency. Pinned by the guard spec in Task 3.
- Private-CA trust on the S3 hop: the server must accept Garage's certificate through the OS store, and the presigned host must be in the certificate SAN. Pinned by the TLS test in Task 1 and the split download control in Task 6.

---

### Task 1: Verify the S3 hop against the platform trust store

**Files:**
- Modify: `Cargo.toml:67`, `crates/microwave-objectstore/Cargo.toml`
- Test: `crates/microwave-objectstore/src/s3.rs` in `mod tests`

**Interfaces:**
- Produces: `S3ObjectStore::new` builds its ureq agent with `RootCerts::PlatformVerifier`; later cookbook work relies on the process trusting `/etc/ssl/certs` after `update-ca-certificates`.

- [ ] **Step 1: Write the failing test**

Add a TLS bucket server helper and this test to `mod tests`:

```rust
#[cfg(all(unix, not(target_vendor = "apple")))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_check_trusts_a_private_ca_from_the_platform_store() {
    let _network = NETWORK_TEST_LOCK.lock().await;
    let (base_url, ca_pem) = tls_bucket_server(200).await;
    let dir = tempfile::tempdir().unwrap();
    let ca_path = dir.path().join("ca.pem");
    std::fs::write(&ca_path, ca_pem).unwrap();

    // SAFETY: the network tests are serialised by NETWORK_TEST_LOCK and the
    // plain-HTTP tests ignore SSL_CERT_FILE.
    unsafe { std::env::set_var("SSL_CERT_FILE", &ca_path) };
    let store =
        S3ObjectStore::new(S3Config::new(base_url, "chef", "garage", "key", "secret")).unwrap();
    let result = store.health_check();
    unsafe { std::env::remove_var("SSL_CERT_FILE") };

    result.unwrap();
}
```

`tls_bucket_server(status)` generates a throwaway CA with `rcgen` (`IsCa::Ca(BasicConstraints::Unconstrained)`), a CA-signed leaf for `127.0.0.1` with an `IpAddress` SAN and the server-auth EKU, serves the same axum fallback over `axum_server::from_tcp_rustls` on a random local port, and returns `(https_url, ca_pem)`. Wrap the environment change in a small guard struct so a panic still removes `SSL_CERT_FILE`.

- [ ] **Step 2: Run the test and see it fail**

Run: `cargo test -p microwave-objectstore health_check_trusts_a_private_ca_from_the_platform_store`
Expected: FAIL with an invalid-peer-certificate / unknown-issuer error from ureq.

- [ ] **Step 3: Enable and select the platform verifier**

In `crates/microwave-objectstore/Cargo.toml` add dev dependencies `axum-server.workspace = true`, `rcgen.workspace = true`, `rustls.workspace = true`. In the workspace `Cargo.toml` change the line to `ureq = { version = "3.5", features = ["platform-verifier"] }`. In `S3ObjectStore::new`, select the verifier through the TLS builder — ureq 3.5 puts `root_certs` on `TlsConfigBuilder`, not on `ConfigBuilder`, so the chain is `.tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build())`.

- [ ] **Step 4: Run the test and the workspace suite**

Run: `cargo test -p microwave-objectstore health_check_trusts_a_private_ca_from_the_platform_store`
Expected: PASS.
Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets`
Expected: no findings.

```bash
git add Cargo.toml Cargo.lock crates/microwave-objectstore
git commit -m "feat(objectstore): verify S3 TLS against the platform trust store"
```

---

### Task 2: Make disable sticky and add the switch recipes

**Files:**
- Create: `cookbooks/microwave/libraries/disabled.rb`
- Create: `cookbooks/microwave/recipes/disable.rb`
- Create: `cookbooks/microwave/recipes/enable.rb`
- Modify: `cookbooks/microwave/resources/postgresql.rb`, `opensearch.rb`, `server.rb` (service resources, marker directory, server validation)
- Modify: `cookbooks/microwave/metadata.rb` and `spec/metadata_spec.rb` (the two new recipes join the declared list, which the spec asserts with `contain_exactly`)
- Test: `cookbooks/microwave/spec/disabled_spec.rb`, `postgresql_resource_spec.rb`, `opensearch_resource_spec.rb`, `server_resource_spec.rb`, `recipes_spec.rb`

**Interfaces:**
- Produces: `Microwave::Disabled::DIR`, `Microwave::Disabled.marker(unit)`, `.marked?(unit)`, `.unit?(unit)`; actions `:disable`/`:enable` on `microwave_postgresql`, `microwave_opensearch`, `microwave_server`; recipes `microwave::disable` and `microwave::enable`.

- [ ] **Step 1: Write the failing library spec**

```ruby
require_relative 'spec_helper'

describe Microwave::Disabled do
  it 'maps a unit to its marker' do
    expect(described_class.marker('postgresql')).to eq('/etc/microwave/disabled/postgresql')
  end

  it 'reports a marker as set only while the file exists' do
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/garage').and_return(true)

    expect(described_class.marked?('garage')).to be(true)
  end

  it 'finds a unit in any of the three unit directories' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/lib/systemd/system/nginx.service').and_return(true)

    expect(described_class.unit?('nginx')).to be(true)
    expect(described_class.unit?('absent')).to be(false)
  end
end
```

Run: `bundle exec rspec spec/disabled_spec.rb`
Expected: FAIL with `uninitialized constant Microwave::Disabled`.

- [ ] **Step 2: Implement the library**

```ruby
module Microwave
  module Disabled
    DIR = '/etc/microwave/disabled'.freeze
    UNIT_DIRS = ['/etc/systemd/system', '/lib/systemd/system', '/usr/lib/systemd/system'].freeze

    def self.marker(unit)
      ::File.join(DIR, unit)
    end

    def self.marked?(unit)
      ::File.exist?(marker(unit))
    end

    def self.unit?(unit)
      UNIT_DIRS.any? { |dir| ::File.exist?(::File.join(dir, "#{unit}.service")) }
    end
  end
end
```

Run: `bundle exec rspec spec/disabled_spec.rb`
Expected: PASS.

- [ ] **Step 3: Write the failing postgresql action spec**

Append to `postgresql_resource_spec.rb`:

```ruby
describe 'microwave_postgresql switching' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql'])
                        .converge('microwave-test::database_disable')
  end

  it 'stops the unit, disables it and drops the marker' do
    expect(chef_run).to stop_service('postgresql')
    expect(chef_run).to disable_service('postgresql')
    expect(chef_run).to create_file('/etc/microwave/disabled/postgresql')
      .with(owner: 'root', group: 'root', mode: '0644')
  end

  it 'skips a unit that does not exist' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/lib/systemd/system/postgresql.service').and_return(false)
    guard = chef_run.service('postgresql').only_if.first.block

    expect(guard.call).to be(false)
  end

  it 'does not enable or start a marked unit on install' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/postgresql').and_return(true)
    guard = ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql'])
                            .converge('microwave-test::database')
                            .service('postgresql').not_if.first.block

    expect(guard.call).to be(true)
  end
end
```

Create `spec/fixtures/cookbooks/microwave-test/recipes/database_disable.rb`:

```ruby
microwave_postgresql 'default' do
  action :disable
end
```

Run: `bundle exec rspec spec/postgresql_resource_spec.rb`
Expected: FAIL because the action and guards do not exist.

ChefSpec runs the resources, so the `only_if` guard decides whether the switch is applied; the spec stubs the unit file as present so `:disable` yields `stop` and `disable`, and the guard examples above flip it. The same stubs go into every switching describe block.

- [ ] **Step 4: Implement the postgresql switching**

Nested runners (the marker-guard example) repeat the `node.name`/`fqdn`/machine setup of the outer one.

In `:install` add before the service:

```ruby
directory Microwave::Disabled::DIR do
  owner 'root'
  group 'root'
  mode '0755'
end
```

Change the `service 'postgresql'` guard to `not_if { Microwave::Disabled.marked?('postgresql') }`, give the `service 'microwave-server'` restart target the same guard against `microwave-server`, and add:

```ruby
action :disable do
  directory Microwave::Disabled::DIR do
    owner 'root'
    group 'root'
    mode '0755'
  end

  file Microwave::Disabled.marker('postgresql') do
    content "disabled by microwave::disable\n"
    owner 'root'
    group 'root'
    mode '0644'
  end

  service 'postgresql' do
    action %i(stop disable)
    only_if { Microwave::Disabled.unit?('postgresql') }
  end
end

action :enable do
  file Microwave::Disabled.marker('postgresql') do
    action :delete
  end

  service 'postgresql' do
    action %i(enable start)
    only_if { Microwave::Disabled.unit?('postgresql') }
  end
end
```

Run: `bundle exec rspec spec/postgresql_resource_spec.rb`
Expected: PASS.

- [ ] **Step 5: Repeat for opensearch**

Write the same three examples in `opensearch_resource_spec.rb` (fixture `spec/fixtures/cookbooks/microwave-test/recipes/search_disable.rb` with `action :disable`), then implement on `opensearch.rb`: marker directory, `not_if` on both `service 'opensearch'` and `service 'nginx'` (unit names `opensearch`, `nginx`), and the two actions. A re-enabled backend must not start nginx, so `:enable` in this resource only touches `opensearch`.

Run: `bundle exec rspec spec/opensearch_resource_spec.rb`
Expected: PASS.

- [ ] **Step 6: Repeat for the server and make its URLs optional**

Create the fixture `spec/fixtures/cookbooks/microwave-test/recipes/server_disable.rb` (`microwave_server 'default' do action :disable end`) and `.../server_disable_install.rb` (`microwave_server 'default'`, `action :install`, no URLs).

In `server_resource_spec.rb` add a nested `describe 'microwave_server switching'` with a `chef_run` that converges `microwave-test::server_disable` and the same three examples as Step 3 (unit `microwave-server`), plus:

```ruby
it 'does not need database_url or search_url to disable' do
  expect(chef_run.microwave_server('default').database_url).to be_nil
  expect(chef_run.microwave_server('default').search_url).to be_nil
end

it 'raises on install without a database_url' do
  expect do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server'])
                        .converge('microwave-test::server_disable_install')
  end.to raise_error(/database_url/)
end
```

Implement: drop `required: true` from `database_url` and `search_url`; put the two `raise`s for empty values at the very top of `action :install`, above the `Microwave::Release.asset` lookup, so a URL-less install fails on the property and never reaches the network; add the marker directory, the `not_if` on `service 'microwave-server'`, and both actions exactly as in Step 4 with unit `microwave-server`.

Run: `bundle exec rspec spec/server_resource_spec.rb`
Expected: PASS.

- [ ] **Step 7: Write the failing recipe specs**

Add to `recipes_spec.rb`:

```ruby
describe 'microwave::disable' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13',
                             step_into: %w(microwave_server microwave_opensearch microwave_postgresql))
                        .converge('microwave::disable')
  end

  it 'disables the three services in dependency order and nginx last' do
    # `step_into` leaves the resource wrappers in the collection; the order that
    # matters is the one inside them.
    inner = chef_run.resource_collection.reject { |r| r.resource_name.to_s.start_with?('microwave_') }

    expect(inner.map { |r| r.resource_name.to_s }).to eq(
      %w(directory file service directory file service directory file service file service)
    )
    expect(chef_run.resource_collection.select { |r| r.resource_name == :service }.map(&:name))
      .to eq(%w(microwave-server opensearch postgresql nginx))
  end
end

describe 'microwave::enable' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13',
                             step_into: %w(microwave_server microwave_opensearch microwave_postgresql))
                        .converge('microwave::enable')
  end

  it 'starts nginx first and the server last' do
    expect(chef_run.resource_collection.select { |r| r.resource_name == :service }.map(&:name))
      .to eq(%w(nginx postgresql opensearch microwave-server))
  end
end
```

Run: `bundle exec rspec spec/recipes_spec.rb`
Expected: FAIL because the recipes do not exist.

- [ ] **Step 8: Write the recipes**

`recipes/disable.rb`:

```ruby
# Stops and disables every service, in reverse dependency order. The server
# first so it stops answering, nginx last because both proxies share it.
microwave_server 'default' do
  action :disable
end

microwave_opensearch 'default' do
  action :disable
end

microwave_postgresql 'default' do
  action :disable
end

file Microwave::Disabled.marker('nginx') do
  content "disabled by microwave::disable\n"
  owner 'root'
  group 'root'
  mode '0644'
end

service 'nginx' do
  action %i(stop disable)
  only_if { Microwave::Disabled.unit?('nginx') }
end
```

`recipes/enable.rb` is the exact inverse:

```ruby
# The inverse of microwave::disable. nginx first because both proxies need it
# listening, the server last so it does not probe a backend that is still down.
file Microwave::Disabled.marker('nginx') do
  action :delete
end

service 'nginx' do
  action %i(enable start)
  only_if { Microwave::Disabled.unit?('nginx') }
end

microwave_postgresql 'default' do
  action :enable
end

microwave_opensearch 'default' do
  action :enable
end

microwave_server 'default' do
  action :enable
end
```

- [ ] **Step 9: Run the area and commit**

Run: `bundle exec rspec && bundle exec cookstyle --fail-level refactor`
Expected: PASS, no offenses.

```bash
git add cookbooks/microwave
git commit -m "feat(cookbook): add sticky disable and enable actions"
```

---

### Task 3: Add the Garage resource

**Files:**
- Create: `cookbooks/microwave/resources/garage.rb`
- Create: `cookbooks/microwave/recipes/garage.rb`
- Create: `cookbooks/microwave/templates/garage.toml.erb`, `garage.service.erb`, `garage-proxy.nginx.erb`
- Modify: `cookbooks/microwave/attributes/default.rb`, `metadata.rb`, `recipes/disable.rb`, `recipes/enable.rb`
- Test: `cookbooks/microwave/spec/garage_resource_spec.rb`, `recipes_spec.rb`

**Interfaces:**
- Consumes: `Microwave::Disabled` from Task 2.
- Produces: `microwave_garage` with `:install`/`:disable`/`:enable`; properties `hostname`, `port` (3900), `capacity` (`1G`), `bucket` (`chef-cookbooks`), `region` (`garage`), `client_network`; key files `/etc/microwave/garage.access_key`/`.secret_key`; unit `garage`.

- [ ] **Step 1: Write the failing resource spec**

Create `spec/garage_resource_spec.rb` with a fixture recipe `spec/fixtures/cookbooks/microwave-test/recipes/garage.rb` that steps into `microwave_garage`. The guards use `system`, not `stub_command`, and the binary is absent on the spec host, so they all report false and every execute runs; stub `::File.read` for the three key files the commands interpolate. Examples:

- creates the `microwave` and `garage` system groups, the `garage` user, and `/var/lib/garage/{meta,data}` as `0700 garage:garage`;
- downloads `https://garagehq.deuxfleurs.fr/_releases/v2.5.0/x86_64-unknown-linux-musl/garage` to `/usr/local/bin/garage` with the `x86_64` digest and mode `0755`;
- writes `/etc/garage.toml` `0640 root:garage` containing `replication_factor = 1`, `db_engine = "sqlite"`, `api_bind_addr = "127.0.0.1:3910"`, `public_endpoint = "https://garage.example.com:3900"`, `public_endpoint_path_style = true`, and no literal `rpc_secret =`;
- writes the rpc secret and admin token `0600 garage:garage` (Garage rejects group/other bits on secret files) and the access/secret key `0640 root:microwave`;
- issues a certificate with SAN `DNS:garage.example.com,IP:127.0.0.1`, key `0600 garage:garage`;
- installs `nginx` and declares a marker-guarded reload target for it;
- renders the nginx site with `listen garage.example.com:3900 ssl;`, `client_max_body_size 0;`, `proxy_request_buffering off;` and `proxy_pass http://127.0.0.1:3910;`;
- declares `service 'garage'` and the daemon-reload execute;
- gives the layout, bucket, key and permission commands a chicken-and-egg-free state guard behind the marker guard, and marks the key import `sensitive`.

Run: `bundle exec rspec spec/garage_resource_spec.rb`
Expected: FAIL with an unknown resource/recipe.

- [ ] **Step 2: Add the fixture recipe and attributes**

`spec/fixtures/cookbooks/microwave-test/recipes/garage.rb`:

```ruby
microwave_garage 'default' do
  hostname 'garage.example.com'
end
```

The runner sets `node.automatic['fqdn'] = 'garage.example.com'` and `node.automatic['kernel']['machine'] = 'x86_64'`, like the database and search specs. `garage.rb` starts with `require 'securerandom'`.

Attributes in `attributes/default.rb`:

```ruby
default['microwave']['garage_version'] = '2.5.0'
default['microwave']['garage_capacity'] = '1G'
default['microwave']['garage_digests'] = {
  'x86_64' => '2ff667286217cd73ebcb1c03af993822365000441dfe31a80cb64c5189a9dea8',
  'aarch64' => '69b6040cef428b993500c17dd0d0c1240acf2f3f667b9b05ceba4b9660db4e8c',
}
```

- [ ] **Step 3: Implement the properties and install prefix**

Properties exactly:

```ruby
property :hostname, String, default: lazy { node['fqdn'].to_s.empty? ? node['hostname'] : node['fqdn'] }
property :port, Integer, default: 3900
property :capacity, String, default: lazy { node['microwave']['garage_capacity'] }
property :bucket, String, default: 'chef-cookbooks'
property :region, String, default: 'garage'
property :client_network, [String, nil]
property :version, String, default: lazy { node['microwave']['garage_version'] }, regex: /\A\d+\.\d+\.\d+\z/
```

`action :install` starts with `garage = '/usr/local/bin/garage'`, `config = '/etc/garage.toml'`, `capacity_file = '/var/lib/garage/capacity'`, and an arch map (`x86_64` → `x86_64-unknown-linux-musl`, `aarch64`/`arm64` → `aarch64-unknown-linux-musl`) that raises on anything else and rejects an absent digest. Create `group 'microwave'`, `group 'garage'`, `user 'garage'` (system, `/usr/sbin/nologin`, no home), `/etc/microwave`, `Microwave::TLS.ca_dir`, the marker directory, the data directories, a guarded `remote_file` with `checksum`, the two `SecureRandom.hex(32)` secret files, the key pair files (`hex(20)`/`hex(32)`), the config template, the unit template, the full TLS block from `postgresql.rb`: the CA key and CA certificate executes plus their root-owned `0640`/`0644` files, the service key generate, the `backend-cert.cnf.erb` template, the certificate execute with the `DNS:<hostname>,IP:127.0.0.1` SAN, and the key file as `0600 garage:garage`. The split garage node is the only TLS issuer there, so the CA block is not optional; keep its `not_if` guards and point every restart notification at `service[garage]` instead of `service[postgresql]`. Then `apt_package 'nginx'`, the nginx vhost/link and default-site deletion, a marker-guarded `service 'nginx'` (the reload target for the vhost and link, `supports reload: true`), the marker-guarded `service 'garage'`, and the provisioning executes. A garage-only node and the all-in-one node both need nginx, so the resource installs it itself instead of borrowing the OpenSearch resource's package. `:enable` in this resource does not start nginx, like OpenSearch.

- [ ] **Step 4: Implement the provisioning and both actions**

Order after the service: readiness wait, layout assign, layout apply, bucket create, key import, bucket allow. Every execute runs `user 'garage'`, `group 'garage'`, has `not_if { Microwave::Disabled.marked?('garage') }` **first** (Chef short-circuits the `not_if` chain, so a stopped node never shells out to the admin API), and then a state guard:

```ruby
# wait: bounded readiness loop, only while the admin API does not answer
command "for i in $(seq 60); do #{garage} -c #{config} status >/dev/null 2>&1 && exit 0; sleep 2; done; exit 1"
not_if { system("#{garage} -c #{config} status >/dev/null 2>&1") }
# assign: stage the only node with the configured capacity and record it
command "#{garage} -c #{config} layout assign -z dc1 -c #{capacity} #{node_id} && printf '%s' '#{capacity}' > #{capacity_file}"
not_if do
  # `layout show` renders capacities humanised and rounded (`2G` becomes
  # `1.9 GiB`), so the exact recorded string is the state instead: a missing
  # record stages, a superseded stage restages, and no spelling can be mixed up
  ::File.exist?(capacity_file) && ::File.read(capacity_file) == capacity
end
# apply: enact the staged version once
command "#{garage} -c #{config} layout apply --version 1"
not_if { system("#{garage} -c #{config} layout show 2>/dev/null | grep -qE '^Current cluster layout version: [1-9]'") }
# create the bucket
command "#{garage} -c #{config} bucket create #{bucket.dump}"
not_if { system("#{garage} -c #{config} bucket info #{bucket.dump} >/dev/null 2>&1") }
# import the pre-generated key pair (secret key is in the command line)
command "#{garage} -c #{config} key import #{access_key} #{secret_key} -n microwave --yes"
sensitive true
not_if { system("#{garage} -c #{config} key info #{access_key} >/dev/null 2>&1") }
# grant it read/write/owner on the bucket
command "#{garage} -c #{config} bucket allow --read --write --owner --key #{access_key} #{bucket.dump}"
not_if { system("#{garage} -c #{config} bucket info #{bucket.dump} 2>/dev/null | grep -q \"RWO.*#{access_key}\"") }
```

`node_id`, `access_key` and `secret_key` are read lazily: `node_id` from `garage node id -q`, the two keys from their files. Wrap the affected commands in `lazy { }` (assign, import, allow) and read the same values lazily, so a resource the marker skips never reads a file or shells out. The config template writes `metadata_dir = "/var/lib/garage/meta"`, `data_dir = "/var/lib/garage/data"`, `db_engine = "sqlite"`, `replication_factor = 1`, `rpc_bind_addr = "[::]:3901"`, `rpc_public_addr`, `rpc_secret_file`, `[s3_api]` with the loopback bind, `s3_region`, `public_endpoint https://<hostname>:<port>`, `public_endpoint_path_style = true`, and `[admin]` loopback `3903` with `admin_token_file`. The unit template uses `ExecStart=/usr/local/bin/garage -c /etc/garage.toml server`, `User=garage`, `Group=garage`, `Environment=GARAGE_LOG_TO_JOURNALD=1`, `Restart=on-failure`, `TimeoutStopSec=60`, `LimitNOFILE=42000`, `NoNewPrivileges=true`, `WantedBy=multi-user.target`.

`:disable`/`:enable` follow Task 2's service/unit/marker pattern with unit `garage`; `:disable` and `:install` both guard the service on the marker, and the execute guards carry it as above. Add `microwave_garage 'default' do action :disable end` to `recipes/disable.rb` before nginx, and `action :enable` right after the nginx service of `recipes/enable.rb` (nginx stays first, the server stays last).

`recipes/garage.rb` is `microwave_garage 'default'` with the `capacity`, `bucket` and `client_network` attributes passed through. Add `recipe` lines in `metadata.rb` for `microwave::garage`, `microwave::disable` and `microwave::enable`, and extend the `metadata_spec.rb` list to `'microwave::default', 'microwave::server', 'microwave::database', 'microwave::search', 'microwave::garage', 'microwave::disable', 'microwave::enable'`.

Update the Task 2 recipe specs for the new rows: the resource-name array gains another `directory file service` triple (now 14 entries, `%w(directory file service directory file service directory file service directory file service file service)`), and `select { |r| r.resource_name == :service }.map(&:name)` becomes `%w(microwave-server opensearch postgresql garage nginx)` for disable and `%w(nginx garage postgresql opensearch microwave-server)` for enable.

Run: `bundle exec rspec spec/garage_resource_spec.rb spec/recipes_spec.rb spec/metadata_spec.rb`
Expected: PASS.

- [ ] **Step 5: Run the area and commit**

Run: `bundle exec rspec && bundle exec cookstyle --fail-level refactor`
Expected: PASS.

```bash
git add cookbooks/microwave
git commit -m "feat(cookbook): add the Garage object store resource"
```

---

### Task 4: Render the object store from the server resource

**Files:**
- Modify: `cookbooks/microwave/resources/server.rb`, `cookbooks/microwave/templates/microwave.toml.erb`
- Test: `cookbooks/microwave/spec/server_resource_spec.rb`

**Interfaces:**
- Produces: properties `object_store_backend`, `object_store_bucket`, `object_store_endpoint`, `object_store_region`, `object_store_path_style`, `object_store_access_key_file`, `object_store_secret_key_file`; a fully populated `[object_store]` block for non-local backends.

- [ ] **Step 1: Write the failing specs**

Add fixtures `server_garage.rb`, `server_garage_broken.rb` (same, but no endpoint) and `server_s3.rb` that set every object-store property, plus:

```ruby
let(:garage_run) { object_store_run('server_garage') }
let(:s3_run) { object_store_run('server_s3') }
let(:broken_run) { object_store_run('server_garage_broken') }

def object_store_run(recipe)
  allow(::File).to receive(:read).and_call_original
  allow(::File).to receive(:read).with('/etc/microwave/garage.access_key').and_return("ACCESS\n")
  allow(::File).to receive(:read).with('/etc/microwave/garage.secret_key').and_return("SECRET\n")
  ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
    node.name('chef.example.com')
    node.automatic['fqdn'] = 'chef.example.com'
    node.automatic['kernel']['machine'] = 'x86_64'
  end.converge("microwave-test::#{recipe}")
end
```

The `server_s3` fixture sets `path_style false` and its own endpoint; `server_garage_broken` leaves `object_store_endpoint` empty.

Examples:

```ruby
it 'keeps the local backend when nothing is set' do
  expect(chef_run).to render_file('/etc/microwave/microwave.toml')
    .with_content(/^\[object_store\]\nbackend = "local"$/)
end

it 'renders the garage backend with keys read from the files' do
  expect(garage_run).to render_file('/etc/microwave/microwave.toml').with_content(<<~TOML)
    [object_store]
    backend = "garage"
    bucket = "chef-cookbooks"
    endpoint = "https://garage.example.com:3900"
    region = "garage"
    path_style = true
    access_key = "ACCESS"
    secret_key = "SECRET"
  TOML
end

it 'renders the s3 backend with path style off' do
  expect(s3_run).to render_file('/etc/microwave/microwave.toml')
    .with_content(/^backend = "s3"$/).with_content(/^path_style = false$/)
end

it 'raises when a non-local backend has no endpoint' do
  expect { broken_run }.to raise_error(/object_store_endpoint/)
end
```

Run: `bundle exec rspec spec/server_resource_spec.rb`
Expected: FAIL.

- [ ] **Step 2: Implement the properties and template**

Properties:

```ruby
property :object_store_backend, String, default: lazy { node['microwave']['object_store_backend'] }, equal_to: %w(local s3 garage)
property :object_store_bucket, String, default: lazy { node['microwave']['object_store_bucket'] }
property :object_store_endpoint, String
property :object_store_region, String, default: lazy { node['microwave']['object_store_region'] }
property :object_store_path_style, [true, false], default: lazy { node['microwave']['object_store_path_style'] }
property :object_store_access_key_file, String
property :object_store_secret_key_file, String
```

In `:install`, after the URL validation, for a non-local backend require a non-empty endpoint, bucket, region and readable key files with messages naming the property. Build an `object_store` hash whose access/secret values are `::File.read(file).strip`. Template variables gain `object_store:`.

The template keeps rendering the `[object_store]` table and its `backend` line unconditionally (the current behaviour, so a local install renders identically); it adds `bucket`, `endpoint`, `region`, `path_style`, `access_key` and `secret_key` only when the backend is not `local`, escaping every string with the existing `toml.` lambda.

- [ ] **Step 3: Run the area and commit**

Run: `bundle exec rspec spec/server_resource_spec.rb && bundle exec cookstyle --fail-level refactor`
Expected: PASS.

```bash
git add cookbooks/microwave
git commit -m "feat(cookbook): render the object store from server properties"
```

---

### Task 5: Wire attributes, recipes and the local default

**Files:**
- Modify: `cookbooks/microwave/attributes/default.rb`, `recipes/default.rb`, `recipes/server.rb`, `recipes/garage.rb`
- Test: `cookbooks/microwave/spec/attributes_spec.rb`, `recipes_spec.rb`

**Interfaces:**
- Consumes: everything from Tasks 2-4.
- Produces: `object_store_*` attributes; `microwave::default` includes garage only for `garage` and fills a loopback or hostname endpoint; `microwave::server` passes all properties.

- [ ] **Step 1: Write the failing attribute and recipe specs**

```ruby
it 'defaults the object store to local so existing installs keep working' do
  expect(node_for('microwave.example').dig('microwave', 'object_store_backend')).to eq('local')
end

it 'defaults the garage key files next to the password file' do
  node = node_for('microwave.example')
  expect(node.dig('microwave', 'object_store_access_key_file')).to eq('/etc/microwave/garage.access_key')
end
```

In `recipes_spec.rb`, first extend `stub_backend_material` to also stub `/etc/microwave/garage.access_key` and `/etc/microwave/garage.secret_key` (the garage install and the server both read them once a task steps in):

```ruby
describe 'microwave::default object store' do
  before { stub_backend_material }

  it 'runs no garage resource and keeps the local backend by default' do
    expect(runner.converge('microwave::default').resource_collection.map(&:resource_name))
      .not_to include('microwave_garage')
  end

  it 'includes garage before the server and points at loopback when asked' do
    run = runner { |n| n.normal['microwave']['object_store_backend'] = 'garage' }.converge('microwave::default')
    names = run.resource_collection.map(&:resource_name)
    expect(names.index('microwave_garage')).to be < names.index('microwave_server')
    expect(run.microwave_server('default').object_store_endpoint).to eq('https://127.0.0.1:3900')
  end
end
```

Run: `bundle exec rspec spec/attributes_spec.rb spec/recipes_spec.rb`
Expected: FAIL.

- [ ] **Step 2: Implement the attributes and composition**

Attributes: `object_store_backend 'local'`, `object_store_bucket 'chef-cookbooks'`, `object_store_region 'garage'`, `object_store_endpoint nil`, `object_store_host nil`, `object_store_path_style true`, `object_store_access_key_file '/etc/microwave/garage.access_key'`, `object_store_secret_key_file '/etc/microwave/garage.secret_key'`.

`recipes/default.rb`:

```ruby
if node['microwave']['object_store_backend'] == 'garage'
  if node['microwave']['object_store_endpoint'].to_s.empty?
    store_host = node['microwave']['object_store_host'].to_s
    if store_host.empty?
      store_host = '127.0.0.1'
      store_host = node['fqdn'].to_s.empty? ? node['hostname'] : node['fqdn'] if node['microwave']['client_network']
    end
    node.default['microwave']['object_store_endpoint'] = "https://#{store_host}:3900"
  end
  include_recipe 'microwave::garage'
end
```

The endpoint follows the certificate name (the resource `hostname`: FQDN, else
hostname) plus `127.0.0.1`, not `server.bind`, because the SAN and the vhost
listener are built from that name.

The include must come after `microwave::search` and before the server. `recipes/server.rb` passes all seven properties; `recipes/garage.rb` passes `capacity`, `bucket` and `client_network`.

- [ ] **Step 3: Guard the organization step on the marker**

Add `not_if { Microwave::Disabled.marked?('microwave-server') }` to `execute 'microwave_organization'` and a spec that the guard block returns true while marked, so a `microwave::default` re-converge on a disabled node does not wait for `/_status`.

- [ ] **Step 4: Run the full area and commit**

Run: `bundle exec rspec && bundle exec cookstyle --fail-level refactor`
Expected: PASS.

```bash
git add cookbooks/microwave
git commit -m "feat(cookbook): wire the object store through attributes and recipes"
```

---

### Task 6: Prove both topologies and the disable action in Kitchen

**Files:**
- Modify: `cookbooks/microwave/.kitchen.yml`, `test/fixtures/docker-compose.yml`, `test/fixtures/cookbooks/microwave-test/recipes/{search_node,garage_node}.rb`, `test/hooks/pre_converge.sh`, `test/integration/helpers.rb`, `test/integration/all-in-one/default_test.rb`, `test/integration/split/default_test.rb`, `Rakefile`, `.github/workflows/cookbook.yml`
- Create: `test/fixtures/cookbooks/microwave-test/recipes/garage_node.rb`, `test/integration/disable/default_test.rb`

**Interfaces:**
- Consumes: recipes and attributes from Tasks 2-5.
- Produces: the `disable` suite, `kitchen:disable`, a third compose backend, and `mw.converge(recipe)`.

- [ ] **Step 1: Extend the fixture stack**

Add `garage` to `docker-compose.yml` like the other nodes, hostname `garage.microwave.test`, network alias, `depends_on: search: condition: service_healthy`, command `cinc-client ... -o 'microwave-test::garage_node' --json-attributes /tmp/kitchen/attrs.json && exec sleep infinity`, and healthcheck `test -s /mnt/shared/garage.s3 && curl -sS --cacert /mnt/shared/garage-ca.crt https://garage.microwave.test:3900/ -o /dev/null` (no `-f`: the S3 root answers `403` by design, and `-f` would turn that into a failure) (interval 5s, timeout 5s, retries 240, start_period 10s).

`garage_node.rb` includes `microwave::garage`, publishes `garage.access_key`, `garage.secret_key`, `garage-ca.crt`, runs `garage -c /etc/garage.toml bucket info chef-cookbooks > /mnt/shared/garage-bucket.txt`, and writes `/mnt/shared/garage.s3` last. `search_node.rb` gains `/mnt/shared/search.converged` as its last resource; `search` gains the healthcheck `test -s /mnt/shared/search.converged && curl -fsS --cacert /mnt/shared/search-ca.crt https://search.microwave.test:9200/_cluster/health | grep -Eq '"status":"(green|yellow)"'` (interval 5s, timeout 5s, retries 240, start_period 10s) so `garage` starts only after the search converge finished.

`pre_converge.sh`, in the existing shapes:

- `digest()` and `both_running()` cover `garage` too, and `ready()` additionally requires `-s fixtures/shared/garage.s3` and a successful `$compose exec -T garage curl -sS --cacert /mnt/shared/garage-ca.crt https://garage.microwave.test:3900/ >/dev/null`. No `-f`: the unsigned root GET answers `403` by design, and any answer, `403` included, proves the proxy is up.
- the cache-permission loop is `for service in db search garage`.
- `ca.crt` is built from `db-ca.crt`, `search-ca.crt` and `garage-ca.crt`.
- publish `garage-vhost.conf` (`$compose exec -T garage cat /etc/nginx/sites-available/microwave-garage`) next to the existing `search-vhost.conf`; the recipe already writes `garage-bucket.txt` into the shared directory, and `chmod 0644` covers both through the existing glob.

- [ ] **Step 2: Set the backend in the suites**

Both existing suites get `object_store_backend: garage`; `split` additionally gets `object_store_endpoint: https://garage.microwave.test:3900`, `object_store_access_key_file: /mnt/backend/garage.access_key`, `object_store_secret_key_file: /mnt/backend/garage.secret_key`. `garage_node.rb` passes `client_network node['microwave']['client_network']` so attrs.json's `172.28.0.0/16` gives the vhost its `allow` line. Add `service('garage')` running+enabled to the all-in-one controls and `backend = "garage"` to its config assertion.

- [ ] **Step 3: Add the verifier helper and the split checks**

`helpers.rb`:

```ruby
def converge(recipe)
  @backend.command(
    "CHEF_LICENSE=accept-no-persist cinc-client -z -c /opt/kitchen/client.rb " \
    "-j /opt/kitchen/dna.json -o '#{recipe}'"
  )
end
```

In the split controls assert the status endpoint still reports `bookshelf` as `pong` (the existing control already requires the key), read `/mnt/backend/garage.access_key` into a local, assert `/mnt/backend/garage-bucket.txt` contains both `RWO` and that key, assert `/mnt/backend/garage-vhost.conf` contains `listen garage.microwave.test:3900 ssl;` and `allow 172.28.0.0/16;`, and after the knife upload download it again with `mw.knife('cookbook', 'download', 'kitchen_probe', '0.1.0', '--force', '-d', '/tmp/kitchen-download')` and assert the downloaded recipe file exists.

- [ ] **Step 4: Add the disable suite**

Suite `disable`, hostname `kitchen-disable`, run list `microwave::default`, attributes as `all-in-one` with `object_store_backend: garage`, `enforce_idempotency: true` and `multiple_converge: 2`, no network mode, lifecycle `pre_create` + cache permission + `post_destroy`. Its control file, in order:

1. Setup: `mw.install_psql`, `mw.install_knife`, `mw.write_knife_config`, upload `kitchen_probe`, `mw.psql('127.0.0.1', '/etc/microwave/db.password', sql: 'CREATE TABLE IF NOT EXISTS disable_probe (id int); TRUNCATE disable_probe; INSERT INTO disable_probe VALUES (1)')`.
2. `expect(mw.converge('microwave::disable').exit_status).to eq(0)`.
3. For `microwave-server`, `opensearch`, `postgresql`, `garage`, `nginx`: `systemctl is-active` is `inactive`, `is-enabled` is `disabled`, `systemctl show -p UnitFileState --value` is `disabled`.
4. Data survived: `mw.psql('127.0.0.1', '/etc/microwave/db.password', sql: 'SELECT count(*) FROM disable_probe').stdout.strip` is `'1'`; `garage -c /etc/garage.toml bucket info chef-cookbooks` (run on the node) still lists `RWO` for the imported key and at least one object; match the row with `^RWO `, not `\ARWO `, because the output starts with a `==== BUCKET INFORMATION ====` banner; the uploaded cookbook downloads again into `/tmp/disable-download`.
5. `mw.converge('microwave::default')` returns 0 and the units stay `inactive`/`disabled`.
6. `mw.converge('microwave::enable')` returns 0, the units are `active`/`enabled`, and `/_status` is 200.

`microwave::disable` and `::enable` run through `mw.converge`, so they reuse the suite's node attributes via `/tmp/kitchen/dna.json`.

- [ ] **Step 5: Rakefile and CI**

Add `kitchen:disable` with the same wrapper. In the workflow add `disable` to the dispatch `options` and default, to the `case` branch (`all-in-one|split|disable`), and to the default matrix JSON (`{"suite":"disable","task":"kitchen:disable"}`).

- [ ] **Step 6: Run all three suites**

Run: `bundle exec rake kitchen:all_in_one`
Expected: green, second converge 0 changed resources.
Run: `bundle exec rake kitchen:split`
Expected: green, second converge 0 changed resources.
Run: `bundle exec rake kitchen:disable`
Expected: green; units down, data intact, down again after `microwave::default`, up after `microwave::enable`.

- [ ] **Step 7: Lint and commit**

Run: `actionlint .github/workflows/cookbook.yml && bundle exec cookstyle --fail-level refactor`
Expected: PASS.

```bash
git add cookbooks/microwave .github/workflows/cookbook.yml
git commit -m "test(cookbook): prove garage and the disable suite"
```

---

### Task 7: Document the backend and the switches

**Files:**
- Modify: `cookbooks/microwave/README.md`, `docs/deployment.md`
- Test: `python3 scripts/check-claims.py`

- [ ] **Step 1: Update the cookbook README**

The README has to cover the Garage installation end to end, in its existing
sections, not only mention it:

- **Intro** (the paragraph that names the three resources): four resources now,
  `microwave_garage` beside them.
- **Resource table**: a `microwave_garage` row — the pinned Garage static binary
  from the official release URL, nginx; manages `/etc/garage.toml`, the
  `/etc/microwave/garage.*` secret files, the `garage` unit, the nginx TLS
  proxy on `3900`, the certificate pair, and the layout/bucket/key provisioning.
- **New `### microwave_garage` subsection** after `microwave_opensearch`, in the
  same shape as the other two: a property table (`hostname`, `port` `3900`,
  `capacity` `1G`, `bucket`, `region` `garage`, `client_network`) and prose that
  covers: the engine listens on `127.0.0.1:3910`, nginx terminates TLS on
  `3900`; the binary is pinned by `garage_version` and both digests live in
  `garage_digests` (`x86_64`/`aarch64`), verified by `remote_file`; there is no
  package and no container, and no source build; `replication_factor = 1` on a
  single node, for evaluation and small installations, with no multi-node
  support; the certificate SAN is the node name plus `127.0.0.1`; the endpoint
  is handed to clients inside presigned URLs, so it must be reachable from them
  and appear in the SAN; the key pair is generated once and imported
  (`garage key import … --yes`) and granted `RWO` on the bucket, and the bucket
  is created before the key is granted; a second converge provisions nothing
  again; the resource installs nginx itself because either backend may be the
  only proxy on a node.
- **Digest provenance**: where the two digests came from (the release artifact
  for `2.5.0`) and the re-record command on a version bump,
  `curl -fsSL <release-url> | shasum -a 256`, plus that a missing architecture
  raises instead of downloading unverified.
- **Recipes table**: `microwave::garage` (Garage only, for a node a wrapper
  points the server at), `microwave::disable` and `microwave::enable`, and
  `microwave::default` now composes garage when `object_store_backend` is
  `garage`.
- **A new `### The service switches` subsection** under Topologies or after it:
  `:disable` is exactly `stop` + `disable` plus a marker under
  `/etc/microwave/disabled/<unit>` — no `mask`, no package removal, no data
  deletion; the marker keeps every later `microwave::default` converge from
  enabling or starting the unit again; only `:enable` or `microwave::enable`
  removes it; `microwave::disable` disables the server first and nginx last,
  `microwave::enable` does the reverse; a single resource's `:enable` never
  starts nginx. Say that the unit names are `microwave-server`, `postgresql`,
  `opensearch`, `garage`, `nginx`.
- **Attributes table**: `object_store_backend` (`local`), `object_store_bucket`
  (follows `garage_bucket`), `object_store_region`, `object_store_endpoint`,
  `object_store_host`, `object_store_path_style`, the two key-file attributes,
  and `garage_version`, `garage_capacity`, `garage_bucket`, `garage_digests` —
  each with its purpose, and that a wrapper setting only `garage_bucket`
  changes the bucket the store and the server share.
- **Topologies**: extend the split example with a garage node
  (`include_recipe 'microwave::garage'`, `client_network`) and the server's
  `object_store_*` lines, and note that `object_store_host` names the store node
  when the endpoint is not given explicitly.
- **TLS section**: the server verifies the S3 hop against the system trust
  store, which is why the store's CA has to reach `ca_certs`/`ca_cert_file`; the
  garage certificate is a separate pair from the database and search ones.
- **MCP paragraph** stays as it is: MCP is still experimental, writes on by
  default, `writes = false` turns them off.

- [ ] **Step 2: Update `docs/deployment.md`**

Extend the caveat section with the Garage option instead of only the local one: the cookbook's resource, the single-node/`replication_factor = 1` limit, that the endpoint must be client-facing because it is the presigned host, that its certificate SAN must carry that host, and that migrating existing `/bookshelf` objects into the bucket is an operator step. Keep the local-backend paragraph; it still applies to `backend = "local"`.

- [ ] **Step 3: Check claims and commit**

Run: `python3 scripts/check-claims.py`
Expected: PASS.

```bash
git add cookbooks/microwave/README.md docs/deployment.md
git commit -m "docs(cookbook): document garage and the service switches"
```
