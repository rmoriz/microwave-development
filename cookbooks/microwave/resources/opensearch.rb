unified_mode true

description 'Installs OpenSearch 3.9 with a bounded heap and terminates TLS in front of it'

property :hostname, String, default: lazy { node['fqdn'].empty? ? node['hostname'] : node['fqdn'] }
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

  apt_repository 'opensearch' do
    uri 'https://artifacts.opensearch.org/releases/bundle/opensearch/3.x/apt'
    distribution 'stable'
    components %w(main)
    key 'https://artifacts.opensearch.org/publickeys/opensearch.pgp'
    sensitive false
  end

  apt_preference 'opensearch' do
    pin 'version 3.9.0'
    pin_priority '600'
    sensitive false
  end

  apt_package 'opensearch'

  apt_package 'nginx'

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

  file ca_key do
    owner 'root'
    group 'opensearch'
    mode '0640'
    sensitive true
  end

  file ca_cert do
    owner 'root'
    group 'opensearch'
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
    group 'opensearch'
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

  service 'opensearch' do
    action %i(enable start)
  end

  service 'nginx' do
    supports reload: true, restart: true, status: true
    action %i(enable start)
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
  end
end
