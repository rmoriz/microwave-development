require 'securerandom'

unified_mode true

description 'Installs PostgreSQL 19 with the Microwave role, database and TLS material'

# `node['fqdn']` is nil on a bare Chef::Node, so the `to_s` covers that case.
# cookstyle:disable Chef/RedundantCode/OhaiAttributeToString
property :hostname, String, default: lazy { node['fqdn'].to_s.empty? ? node['hostname'] : node['fqdn'] }
# cookstyle:enable Chef/RedundantCode/OhaiAttributeToString
property :port, Integer, default: lazy { node['microwave']['database_port'] }
property :database, String, default: lazy { node['microwave']['database_name'] }
property :user, String, default: lazy { node['microwave']['database_user'] }
property :client_network, [String, nil], default: lazy { node['microwave']['client_network'] }
property :ca_certs, Array, default: [],
                           description: 'PEM CA bundle PostgreSQL uses to verify client certificates'

action :install do
  ca_key = Microwave::TLS.ca_key
  ca_cert = Microwave::TLS.ca_cert
  key = Microwave::TLS.key('db')
  cert = Microwave::TLS.cert('db')
  csr = ::File.join(Microwave::TLS.ca_dir, 'db.csr')
  cnf = ::File.join(Microwave::TLS.ca_dir, 'backend.cnf')
  ca_bundle = ::File.join(Microwave::TLS.ca_dir, 'client-ca.crt')
  data_dir = '/etc/postgresql/19/main'

  # The keyring is a plain shipped file rather than an `apt_repository` key:
  # the resource would copy it into the chef cache, which is empty on the next
  # run, and re-import it every converge. apt 3 reads the armored file directly
  # through signed-by, so nothing in this block changes between runs.
  directory '/etc/apt/keyrings' do
    owner 'root'
    group 'root'
    mode '0755'
  end

  cookbook_file '/etc/apt/keyrings/pgdg.asc' do
    source 'pgdg.asc'
    cookbook 'microwave'
    owner 'root'
    group 'root'
    mode '0644'
    sensitive false
  end

  apt_repository 'pgdg' do
    uri 'https://apt.postgresql.org/pub/repos/apt'
    distribution 'trixie-pgdg'
    components %w(main 19)
    signed_by '/etc/apt/keyrings/pgdg.asc'
    sensitive false
  end

  apt_preference 'postgresql-19' do
    pin 'version 19*'
    pin_priority '600'
    sensitive false
  end

  apt_package 'postgresql-19'

  group 'microwave' do
    system true
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
    notifies :restart, 'service[postgresql]', :delayed
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
    owner 'postgres'
    group 'postgres'
    # PostgreSQL refuses a server key the database user owns with group access.
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
    notifies :restart, 'service[postgresql]', :delayed
  end

  file csr do
    owner 'postgres'
    group 'postgres'
    mode '0640'
    sensitive false
  end

  file cert do
    owner 'postgres'
    group 'postgres'
    mode '0640'
    sensitive false
  end

  file '/etc/microwave/db.password' do
    content "#{SecureRandom.hex(16)}\n"
    owner 'root'
    group 'microwave'
    mode '0640'
    sensitive true
    notifies :run, 'execute[microwave-alter-role-password]', :immediately
    notifies :restart, 'service[microwave-server]', :delayed
    action :create_if_missing
  end

  directory ::File.join(data_dir, 'conf.d') do
    owner 'root'
    group 'root'
    mode '0755'
  end

  file ca_bundle do
    content lazy { "#{new_resource.ca_certs.join("\n")}\n" }
    owner 'root'
    group 'postgres'
    mode '0640'
    sensitive false
    only_if { new_resource.ca_certs.any? }
    notifies :restart, 'service[postgresql]', :delayed
  end

  template ::File.join(data_dir, 'conf.d', 'microwave.conf') do
    source 'postgresql-microwave.conf.erb'
    cookbook 'microwave'
    owner 'root'
    group 'postgres'
    mode '0640'
    sensitive false
    variables(
      cert: cert,
      key: key,
      ca_file: new_resource.ca_certs.empty? ? ca_cert : ca_bundle,
      listen: new_resource.client_network ? '*' : '127.0.0.1',
      port: new_resource.port
    )
    notifies :restart, 'service[postgresql]', :delayed
  end

  template ::File.join(data_dir, 'pg_hba.conf') do
    source 'pg_hba-microwave.conf.erb'
    cookbook 'microwave'
    owner 'postgres'
    group 'postgres'
    mode '0640'
    sensitive false
    variables(
      database: new_resource.database,
      user: new_resource.user,
      client_network: new_resource.client_network
    )
    notifies :restart, 'service[postgresql]', :delayed
  end

  service 'postgresql' do
    action %i(enable start)
  end

  # Delayed restart target for a rotation. On a database-only node the server
  # unit is absent, so the restart is guarded instead of failing the converge.
  service 'microwave-server' do
    action :nothing
    only_if do
      ['/etc/systemd/system/microwave-server.service',
       '/lib/systemd/system/microwave-server.service'].any? { |path| ::File.exist?(path) }
    end
  end

  execute 'microwave-create-role' do
    command lazy { "psql -p #{new_resource.port} -c \"CREATE ROLE \\\"#{new_resource.user}\\\" LOGIN PASSWORD '#{::File.read('/etc/microwave/db.password').strip}'\"" }
    user 'postgres'
    group 'postgres'
    sensitive true
    not_if "psql -p #{new_resource.port} -tAc \"SELECT 1 FROM pg_roles WHERE rolname='#{new_resource.user}'\" | grep -q 1"
  end

  execute 'microwave-alter-role-password' do
    command lazy { "psql -p #{new_resource.port} -c \"ALTER ROLE \\\"#{new_resource.user}\\\" LOGIN PASSWORD '#{::File.read('/etc/microwave/db.password').strip}'\"" }
    user 'postgres'
    group 'postgres'
    sensitive true
    action :nothing
    only_if "psql -p #{new_resource.port} -tAc \"SELECT 1 FROM pg_roles WHERE rolname='#{new_resource.user}'\" | grep -q 1"
  end

  execute 'microwave-create-database' do
    command "psql -p #{new_resource.port} -c \"CREATE DATABASE \\\"#{new_resource.database}\\\" OWNER \\\"#{new_resource.user}\\\"\""
    user 'postgres'
    group 'postgres'
    sensitive false
    not_if "psql -p #{new_resource.port} -tAc \"SELECT 1 FROM pg_database WHERE datname='#{new_resource.database}'\" | grep -q 1"
  end
end
