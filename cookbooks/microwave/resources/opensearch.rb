unified_mode true

description 'Installs OpenSearch 3.9 with a bounded heap and terminates TLS in front of it'

# `node['fqdn']` is nil on a bare Chef::Node, so the `to_s` covers that case.
# cookstyle:disable Chef/RedundantCode/OhaiAttributeToString
property :hostname, String, default: lazy { node['fqdn'].to_s.empty? ? node['hostname'] : node['fqdn'] }
# cookstyle:enable Chef/RedundantCode/OhaiAttributeToString
property :heap, String, default: lazy { node['microwave']['opensearch_heap'] }
property :client_network, [String, nil], default: lazy { node['microwave']['client_network'] }

action :install do
  http_port = 9210
  proxy_port = 9200
  config = '/etc/opensearch/opensearch.yml'
  heap_file = '/etc/opensearch/jvm.options.d/heap.options'
  vhost = '/etc/nginx/sites-available/microwave-opensearch'
  vhost_link = '/etc/nginx/sites-enabled/microwave-opensearch'
  ca_key = Microwave::TLS.ca_key
  ca_cert = Microwave::TLS.ca_cert
  key = Microwave::TLS.key('search')
  cert = Microwave::TLS.cert('search')
  csr = ::File.join(Microwave::TLS.ca_dir, 'search.csr')
  cnf = ::File.join(Microwave::TLS.ca_dir, 'backend.cnf')

  # The key published under /publickeys is stale: it does not carry the subkey
  # that signs the 3.x repository, so apt's sequoia verifier rejects the
  # InRelease. The file shipped here is exactly the signing certificate,
  # fingerprint 530B46D4B4C3E62A4CD6E7A04E9275EE6BA2427F. It is installed as a
  # keyring file and referenced with signed-by instead of going through the
  # `key` property, which would re-import it from the cache every run.
  directory '/etc/apt/keyrings' do
    owner 'root'
    group 'root'
    mode '0755'
  end

  cookbook_file '/etc/apt/keyrings/opensearch.asc' do
    source 'opensearch.asc'
    cookbook 'microwave'
    owner 'root'
    group 'root'
    mode '0644'
    sensitive false
  end

  apt_repository 'opensearch' do
    uri 'https://artifacts.opensearch.org/releases/bundle/opensearch/3.x/apt'
    distribution 'stable'
    components %w(main)
    signed_by '/etc/apt/keyrings/opensearch.asc'
    sensitive false
  end

  apt_preference 'opensearch' do
    pin 'version 3.9.0'
    pin_priority '600'
    sensitive false
  end

  # The packaged postinst runs the security demo installer, which wants an admin
  # password and aborts without one. The plugin is deleted below and the config
  # is rendered from our template, so skip both steps. The environment is set on
  # both packages: apt may defer the configure of a half-installed package to
  # the next install and run the postinst from that process instead.
  packaged_env = {
    'DISABLE_INSTALL_DEMO_CONFIG' => 'true',
    'DISABLE_SECURITY_PLUGIN' => 'true',
  }

  apt_package 'opensearch' do
    environment packaged_env
  end

  apt_package 'nginx' do
    environment packaged_env
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

  directory '/etc/opensearch/jvm.options.d' do
    owner 'root'
    group 'root'
    mode '0755'
  end

  directory '/var/lib/opensearch/_data' do
    owner 'opensearch'
    group 'opensearch'
    mode '0750'
  end

  directory '/var/log/opensearch/_logs' do
    owner 'opensearch'
    group 'opensearch'
    mode '0750'
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
    notifies :restart, 'service[opensearch]', :delayed
  end

  # The CA material is shared by both backend resources, so it belongs to root
  # rather than either service group; otherwise the two runs of the pair keep
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
    owner 'opensearch'
    group 'opensearch'
    mode '0640'
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
    notifies :restart, 'service[opensearch]', :delayed
  end

  file csr do
    owner 'opensearch'
    group 'opensearch'
    mode '0640'
    sensitive false
  end

  file cert do
    owner 'opensearch'
    group 'opensearch'
    mode '0640'
    sensitive false
  end

  template config do
    source 'opensearch.yml.erb'
    cookbook 'microwave'
    owner 'root'
    group 'opensearch'
    mode '0644'
    sensitive false
    variables(
      hostname: new_resource.hostname,
      http_port: http_port
    )
    notifies :restart, 'service[opensearch]', :delayed
  end

  template heap_file do
    source 'heap.options.erb'
    cookbook 'microwave'
    owner 'root'
    group 'root'
    mode '0644'
    sensitive false
    variables(heap: new_resource.heap)
    notifies :restart, 'service[opensearch]', :delayed
  end

  # The engine maps its JNA and index buffers, so the unit raises the locked
  # memory limit; without it the mapping can be capped by the default.
  directory '/etc/systemd/system/opensearch.service.d' do
    owner 'root'
    group 'root'
    mode '0755'
  end

  template '/etc/systemd/system/opensearch.service.d/memlock.conf' do
    source 'opensearch.service.d/memlock.conf.erb'
    cookbook 'microwave'
    owner 'root'
    group 'root'
    mode '0644'
    notifies :run, 'execute[systemctl daemon-reload opensearch]', :immediately
    notifies :restart, 'service[opensearch]', :delayed
  end

  # The name carries the unit, the command does not: `systemctl daemon-reload`
  # takes no operand. In the all-in-one layout the server resource declares its
  # own reload, and two clones under one name would let Chef pick either copy
  # and drop the other's attributes and notification.
  execute 'systemctl daemon-reload opensearch' do
    command 'systemctl daemon-reload'
    action :nothing
    # A container without systemd starts the daemon directly; calling systemctl
    # there would fail even though the drop-in file was written.
    only_if { ::File.exist?('/run/systemd/system') }
  end

  # Microwave's OpenSearch client sends no credentials, so the security plugin
  # has to stay off. Deleting it keeps an upgraded package from pulling a demo
  # configuration back in.
  directory '/usr/share/opensearch/plugins/opensearch-security' do
    action :delete
    recursive true
    sensitive false
    only_if 'test -d /usr/share/opensearch/plugins/opensearch-security'
    notifies :restart, 'service[opensearch]', :delayed
  end

  sysctl 'vm.max_map_count' do
    value '262144'
    sensitive false
    notifies :restart, 'service[opensearch]', :delayed
  end

  # The packaged init script creates the pid directory itself; start-stop-daemon
  # does not, so create it before the direct start below.
  directory '/var/run/opensearch' do
    owner 'opensearch'
    group 'opensearch'
    mode '0755'
    not_if { ::File.exist?('/run/systemd/system') }
  end

  directory Microwave::Disabled::DIR do
    owner 'root'
    group 'root'
    mode '0755'
  end

  service 'opensearch' do
    # The packaged sysv init script aborts under `set -e`: `pidofproc` returns 3
    # while the pid file does not exist yet, so a systemd-less container can
    # never start through it. Start the daemon directly there; systemd hosts
    # keep their unit.
    unless ::File.exist?('/run/systemd/system')
      start_command 'env OPENSEARCH_PATH_CONF=/etc/opensearch ' \
                    '/usr/sbin/start-stop-daemon --start --quiet ' \
                    '--pidfile /var/run/opensearch/opensearch.pid ' \
                    '--chdir /usr/share/opensearch --chuid opensearch ' \
                    '--exec /usr/share/opensearch/bin/opensearch -- ' \
                    '-d -p /var/run/opensearch/opensearch.pid'
    end
    action %i(enable start)
    not_if { Microwave::Disabled.marked?('opensearch') }
  end

  # The proxy is shared by both backends, so it carries its own marker: the
  # search resource stops it, the recipe starts it again.
  service 'nginx' do
    supports reload: true, restart: true, status: true
    action %i(enable start)
    not_if { Microwave::Disabled.marked?('nginx') }
  end

  template vhost do
    source 'opensearch-proxy.nginx.erb'
    cookbook 'microwave'
    owner 'root'
    group 'root'
    mode '0644'
    sensitive false
    variables(
      listen: new_resource.client_network ? new_resource.hostname : '127.0.0.1',
      port: proxy_port,
      upstream: http_port,
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
end

action :disable do
  directory Microwave::Disabled::DIR do
    owner 'root'
    group 'root'
    mode '0755'
  end

  file Microwave::Disabled.marker('opensearch') do
    content "disabled by microwave::disable\n"
    owner 'root'
    group 'root'
    mode '0644'
  end

  service 'opensearch' do
    action %i(stop disable)
    only_if { Microwave::Disabled.unit?('opensearch') }
  end
end

# nginx is started by the recipe, not here: both backends share it, and a
# re-enabled backend must not bring the proxy back on its own.
action :enable do
  file Microwave::Disabled.marker('opensearch') do
    action :delete
  end

  service 'opensearch' do
    action %i(enable start)
    only_if { Microwave::Disabled.unit?('opensearch') }
  end
end
