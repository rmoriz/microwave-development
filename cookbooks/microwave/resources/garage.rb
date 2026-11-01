require 'securerandom'

unified_mode true

description 'Installs Garage 2.5.0 with one layout, bucket and key pair, and terminates TLS in front of it'

# `node['fqdn']` is nil on a bare Chef::Node, so the `to_s` covers that case.
# cookstyle:disable Chef/RedundantCode/OhaiAttributeToString
property :hostname, String, default: lazy { node['fqdn'].to_s.empty? ? node['hostname'] : node['fqdn'] }
# cookstyle:enable Chef/RedundantCode/OhaiAttributeToString
property :port, Integer, default: 3900
property :capacity, String, default: lazy { node['microwave']['garage_capacity'] }
property :bucket, String, default: 'chef-cookbooks'
property :region, String, default: 'garage'
property :client_network, [String, nil], default: lazy { node['microwave']['client_network'] }
property :version, String, default: lazy { node['microwave']['garage_version'] }, regex: /\A\d+\.\d+\.\d+\z/

action :install do
  garage = '/usr/local/bin/garage'
  config = '/etc/garage.toml'
  capacity_file = '/var/lib/garage/capacity'
  s3_port = 3910
  vhost = '/etc/nginx/sites-available/microwave-garage'
  vhost_link = '/etc/nginx/sites-enabled/microwave-garage'
  ca_key = Microwave::TLS.ca_key
  ca_cert = Microwave::TLS.ca_cert
  key = Microwave::TLS.key('garage')
  cert = Microwave::TLS.cert('garage')
  csr = ::File.join(Microwave::TLS.ca_dir, 'garage.csr')
  cnf = ::File.join(Microwave::TLS.ca_dir, 'backend.cnf')
  rpc_secret = '/etc/microwave/garage.rpc_secret'
  admin_token = '/etc/microwave/garage.admin_token'
  access_key_file = '/etc/microwave/garage.access_key'
  secret_key_file = '/etc/microwave/garage.secret_key'

  # The digests are recorded rather than fetched, so a moved tag or an unreachable
  # release host fails the converge instead of silently changing the binary.
  arch, digest_key = case node['kernel']['machine']
                     when 'x86_64' then %w(x86_64-unknown-linux-musl x86_64)
                     when 'aarch64', 'arm64' then %w(aarch64-unknown-linux-musl aarch64)
                     else raise "unsupported architecture #{node['kernel']['machine']} for Garage"
                     end
  digest = node['microwave']['garage_digests'][digest_key]
  raise "no Garage digest recorded for #{digest_key}" if digest.to_s.empty?

  # The admin commands read the node id and the key pair at converge time; the
  # procs keep the reads out of a converge the marker skips.
  node_id = -> { `#{garage} -c #{config} node id -q 2>/dev/null`.split('@').first.to_s }
  access_key = -> { ::File.read(access_key_file).strip }
  secret_key = -> { ::File.read(secret_key_file).strip }

  group 'microwave' do
    system true
  end

  group 'garage' do
    system true
  end

  user 'garage' do
    system true
    # Without the gid, useradd refuses: the group above already exists, and it
    # would try to create a second group of the same name.
    gid 'garage'
    shell '/usr/sbin/nologin'
  end

  directory '/etc/microwave' do
    owner 'root'
    group 'root'
    mode '0755'
  end

  directory Microwave::TLS.ca_dir do
    owner 'root'
    group 'root'
    mode '0755'
  end

  directory Microwave::Disabled::DIR do
    owner 'root'
    group 'root'
    mode '0755'
  end

  # The data directory is also where the assign records the accepted capacity,
  # so the engine user has to own it; the guard reads it as root.
  directory '/var/lib/garage' do
    owner 'garage'
    group 'garage'
    mode '0700'
  end

  %w(meta data).each do |dir|
    directory ::File.join('/var/lib/garage', dir) do
      owner 'garage'
      group 'garage'
      mode '0700'
    end
  end

  remote_file garage do
    source "https://garagehq.deuxfleurs.fr/_releases/v#{new_resource.version}/#{arch}/garage"
    checksum digest
    owner 'root'
    group 'root'
    mode '0755'
    sensitive false
    notifies :restart, 'service[garage]', :delayed
  end

  # Only a file with a non-blank line counts as written: an existing but empty
  # secret would otherwise be handed to the engine or the server as a blank.
  # The engine secrets restart the engine; the key pair is rendered into the
  # server config, so a rotation restarts the server there (its unit is absent
  # on an object-store-only node and the target is guarded).
  #
  # Garage runs as `garage` and refuses a secret file that is group or world
  # readable, so its two own secrets belong to that user 0600. The key pair is
  # read by the server, which is in the microwave group, so it stays there.
  { rpc_secret => ['garage', '0600', 'garage', 'service[garage]'],
    admin_token => ['garage', '0600', 'garage', nil],
    access_key_file => ['root', '0640', 'microwave', 'service[microwave-server]'],
    secret_key_file => ['root', '0640', 'microwave', 'service[microwave-server]'] }
    .each do |path, (owner, mode, grp, target)|
    hex = path.end_with?('access_key') ? 20 : 32

    file path do
      content "#{SecureRandom.hex(hex)}\n"
      owner owner
      group grp
      mode mode
      sensitive true
      notifies :restart, target, :delayed if target
      not_if "grep -qs \"[^[:space:]]\" #{path}"
    end
  end

  execute 'microwave-generate-ca-key' do
    command "openssl genrsa -out #{ca_key} 2048"
    sensitive true
    not_if { ::File.exist?(ca_key) }
  end

  execute 'microwave-create-ca' do
    command "openssl req -new -x509 -key #{ca_key} -out #{ca_cert} -days 3650 -subj \"/CN=microwave-ca\""
    sensitive false
    not_if <<~GUARD
      test -f #{ca_key} && test -f #{ca_cert} &&
      [ "$(openssl x509 -in #{ca_cert} -pubkey -noout)" = "$(openssl pkey -in #{ca_key} -pubout)" ]
    GUARD
    notifies :restart, 'service[garage]', :delayed
  end

  # The CA material is shared by the backend resources, so it belongs to root
  # rather than either service group; otherwise the runs of the pair keep
  # flipping the group and every converge reports a change.
  file ca_key do
    owner 'root'
    group 'root'
    mode '0640'
    sensitive true
  end

  file ca_cert do
    owner 'root'
    group 'root'
    mode '0644'
    sensitive false
  end

  execute 'microwave-generate-service-key' do
    command "openssl genrsa -out #{key} 2048"
    sensitive true
    not_if { ::File.exist?(key) }
  end

  file key do
    owner 'garage'
    group 'garage'
    mode '0600'
    sensitive true
  end

  template cnf do
    source 'backend-cert.cnf.erb'
    cookbook 'microwave'
    owner 'root'
    group 'root'
    mode '0644'
    sensitive false
    variables(hostname: new_resource.hostname)
  end

  execute 'microwave-create-service-certificate' do
    command <<~CMD
      openssl req -new -key #{key} -out #{csr} -subj "/CN=#{new_resource.hostname}" \
        -addext "subjectAltName=DNS:#{new_resource.hostname},IP:127.0.0.1" &&
      openssl x509 -req -in #{csr} -CA #{ca_cert} -CAkey #{ca_key} -CAcreateserial \
        -out #{cert} -days 825 -copy_extensions copy -extfile #{cnf} -extensions ext
    CMD
    sensitive false
    not_if <<~GUARD
      test -f #{key} && test -f #{cert} &&
      openssl verify -CAfile #{ca_cert} #{cert} >/dev/null 2>&1 &&
      [ "$(openssl x509 -in #{cert} -pubkey -noout)" = "$(openssl pkey -in #{key} -pubout)" ] &&
      openssl x509 -in #{cert} -noout -ext subjectAltName 2>/dev/null | grep -qE "DNS:#{Regexp.escape(new_resource.hostname)}(,|$)"
    GUARD
    notifies :restart, 'service[garage]', :delayed
  end

  file csr do
    owner 'garage'
    group 'garage'
    mode '0640'
    sensitive false
  end

  file cert do
    owner 'garage'
    group 'garage'
    mode '0640'
    sensitive false
  end

  template config do
    source 'garage.toml.erb'
    cookbook 'microwave'
    owner 'root'
    group 'garage'
    mode '0640'
    sensitive false
    variables(
      hostname: new_resource.hostname,
      port: new_resource.port,
      region: new_resource.region
    )
    notifies :restart, 'service[garage]', :delayed
  end

  template '/etc/systemd/system/garage.service' do
    source 'garage.service.erb'
    cookbook 'microwave'
    owner 'root'
    group 'root'
    mode '0644'
    sensitive false
    notifies :run, 'execute[systemctl daemon-reload garage]', :immediately
    notifies :restart, 'service[garage]', :delayed
  end

  # The name carries the unit, the command does not: `systemctl daemon-reload`
  # takes no operand. In the all-in-one layout the other resources declare their
  # own reload, and two clones under one name would let Chef pick either copy.
  execute 'systemctl daemon-reload garage' do
    command 'systemctl daemon-reload'
    action :nothing
    # A container without systemd starts the daemon directly; calling systemctl
    # there would fail even though the unit file was written.
    only_if { ::File.exist?('/run/systemd/system') }
  end

  # A garage-only node and the all-in-one node both need the proxy, so the
  # resource installs it itself instead of borrowing another backend's package.
  # A minimal container ships no package lists, so refresh them first; the
  # periodic action is a no-op while the lists are younger than a day, which
  # keeps a second converge change-free.
  apt_update 'nginx'

  apt_package 'nginx'

  template vhost do
    source 'garage-proxy.nginx.erb'
    cookbook 'microwave'
    owner 'root'
    group 'root'
    mode '0644'
    sensitive false
    variables(
      port: new_resource.port,
      upstream: s3_port,
      hostname: new_resource.hostname,
      client_network: new_resource.client_network,
      cert: cert,
      key: key
    )
    notifies :reload, 'service[nginx]', :delayed
  end

  link vhost_link do
    to vhost
    sensitive false
    notifies :reload, 'service[nginx]', :delayed
  end

  # The packaged default site answers on port 80 and is in the way.
  link '/etc/nginx/sites-enabled/default' do
    action :delete
    only_if { ::File.symlink?('/etc/nginx/sites-enabled/default') }
    notifies :reload, 'service[nginx]', :delayed
  end

  # The proxy is shared by the backends, so it carries its own marker.
  service 'nginx' do
    supports reload: true, restart: true, status: true
    action %i(enable start)
    not_if { Microwave::Disabled.marked?('nginx') }
  end

  service 'garage' do
    # Garage ships no init script, so a container without systemd cannot enable
    # or start it through sysvinit: update-rc.d aborts on the missing script.
    # The daemon is started directly there; a host with systemd keeps its unit.
    # The restart returns before the CLI can work again, so it is followed by a
    # wait of its own; the steps below call the CLI right after this resource.
    notifies :run, 'execute[microwave-garage-restart-wait]', :delayed
    if Microwave::DirectService.systemd?
      action %i(enable start)
    else
      Microwave::DirectService.commands(
        self,
        pidfile: '/run/garage.pid',
        exec: '/usr/local/bin/garage',
        chuid: 'garage:garage',
        chdir: '/var/lib/garage',
        args: '-c /etc/garage.toml server',
        background: true
      )
      action :start
    end
    not_if { Microwave::Disabled.marked?('garage') }
  end

  # Delayed restart target for a key or secret change. On an object-store-only
  # node the server unit is absent, so the restart is guarded.
  service 'microwave-server' do
    action :nothing
    only_if do
      ['/etc/systemd/system/microwave-server.service',
       '/lib/systemd/system/microwave-server.service'].any? { |path| ::File.exist?(path) }
    end
    not_if { Microwave::Disabled.marked?('microwave-server') }
  end

  # Every command is guarded by the state it would create. The marker guard is
  # first in each chain because Chef short-circuits the `not_if` list: a stopped
  # node never shells out to the admin API, and a second converge is a no-op.
  execute 'microwave-garage-wait' do
    command lazy { "for i in $(seq 60); do #{garage} -c #{config} status >/dev/null 2>&1 && exit 0; sleep 2; done; exit 1" }
    user 'garage'
    group 'garage'
    not_if { Microwave::Disabled.marked?('garage') }
    not_if { system("#{garage} -c #{config} status >/dev/null 2>&1") }
  end

  # Reached only through the notification above: a restart drops the pidfile,
  # so this one waits without an up-guard that could race the old process.
  execute 'microwave-garage-restart-wait' do
    # Declared here but only run through the notification, so it never changes a
    # converged run.
    action :nothing
    command lazy { "for i in $(seq 60); do #{garage} -c #{config} status >/dev/null 2>&1 && exit 0; sleep 2; done; exit 1" }
    user 'garage'
    group 'garage'
    not_if { Microwave::Disabled.marked?('garage') }
  end

  # `layout show` prints the capacity humanised and rounded (`2G` becomes
  # `1.9 GiB`), so a parsed comparison cannot be exact. The assign records the
  # accepted string instead and the guard reads that record: the exact value is
  # the state, a superseded stage is restaged, and no capacity spelling can be
  # mis-parsed. A missing record (first converge) stages. The record alone is a
  # mirror though — an out-of-band layout change or a wiped data directory
  # leaves a stale one — so the node also has to be in the live layout.
  execute 'microwave-garage-layout-assign' do
    command lazy {
      "#{garage} -c #{config} layout assign -z dc1 -c '#{new_resource.capacity}' #{node_id.call} && " \
        "printf '%s' '#{new_resource.capacity}' > #{capacity_file}"
    }
    user 'garage'
    group 'garage'
    sensitive false
    not_if { Microwave::Disabled.marked?('garage') }
    not_if do
      next false unless ::File.exist?(capacity_file)

      record = begin
        ::File.read(capacity_file)
               rescue SystemCallError
                 nil
      end
      next false unless record == new_resource.capacity

      id = node_id.call
      next false if id.empty?

      system("#{garage} -c #{config} layout show 2>/dev/null | grep -q #{id[0, 16]}")
    end
  end

  # A staged section is the only reason to apply; the node sitting in the
  # current layout alone is not one. The pending version is the current one plus
  # one, read in the shell so a capacity change is what actually lands.
  execute 'microwave-garage-layout-apply' do
    command lazy {
      "#{garage} -c #{config} layout apply --version " \
        "$(($(#{garage} -c #{config} layout show 2>/dev/null | " \
        "sed -n 's/^Current cluster layout version: \\([0-9]*\\)$/\\1/p') + 1))"
    }
    user 'garage'
    group 'garage'
    sensitive false
    not_if { Microwave::Disabled.marked?('garage') }
    not_if { !system("#{garage} -c #{config} layout show 2>/dev/null | grep -q '==== STAGED ROLE CHANGES ===='") }
  end

  execute 'microwave-garage-bucket-create' do
    command lazy { "#{garage} -c #{config} bucket create #{new_resource.bucket.dump}" }
    user 'garage'
    group 'garage'
    sensitive false
    not_if { Microwave::Disabled.marked?('garage') }
    not_if { system("#{garage} -c #{config} bucket info #{new_resource.bucket.dump} >/dev/null 2>&1") }
  end

  # The secret key ends up on the command line, so the import is sensitive.
  execute 'microwave-garage-key-import' do
    command lazy { "#{garage} -c #{config} key import #{access_key.call} #{secret_key.call} -n microwave --yes" }
    user 'garage'
    group 'garage'
    sensitive true
    not_if { Microwave::Disabled.marked?('garage') }
    not_if { system("#{garage} -c #{config} key info #{access_key.call} >/dev/null 2>&1") }
  end

  execute 'microwave-garage-bucket-allow' do
    command lazy { "#{garage} -c #{config} bucket allow --read --write --owner --key #{access_key.call} #{new_resource.bucket.dump}" }
    user 'garage'
    group 'garage'
    sensitive false
    not_if { Microwave::Disabled.marked?('garage') }
    not_if { system("#{garage} -c #{config} bucket info #{new_resource.bucket.dump} 2>/dev/null | grep -q \"RWO.*#{access_key.call}\"") }
  end
end

action :disable do
  directory Microwave::Disabled::DIR do
    owner 'root'
    group 'root'
    mode '0755'
  end

  file Microwave::Disabled.marker('garage') do
    content "disabled by microwave::disable\n"
    owner 'root'
    group 'root'
    mode '0644'
  end

  service 'garage' do
    if Microwave::DirectService.systemd?
      action %i(stop disable)
    else
      Microwave::DirectService.commands(
        self,
        pidfile: '/run/garage.pid',
        exec: '/usr/local/bin/garage',
        chuid: 'garage:garage',
        chdir: '/var/lib/garage',
        args: '-c /etc/garage.toml server',
        background: true
      )
      action :stop
    end
    only_if { Microwave::Disabled.unit?('garage') }
  end
end

# nginx is left alone here, like the search resource: both proxies share it and
# the recipe starts it again.
action :enable do
  file Microwave::Disabled.marker('garage') do
    action :delete
  end

  service 'garage' do
    if Microwave::DirectService.systemd?
      action %i(enable start)
    else
      Microwave::DirectService.commands(
        self,
        pidfile: '/run/garage.pid',
        exec: '/usr/local/bin/garage',
        chuid: 'garage:garage',
        chdir: '/var/lib/garage',
        args: '-c /etc/garage.toml server',
        background: true
      )
      action :start
    end
    only_if { Microwave::Disabled.unit?('garage') }
  end
end
