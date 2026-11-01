unified_mode true

description 'Installs the Microwave Server package, its configuration and its service'

property :database_url, String
property :search_url, String
property :bind, String, default: lazy { node['microwave']['bind'] }
property :public_url, String
property :version, String, default: 'latest', regex: /\A(latest|\d+\.\d+\.\d+)\z/
property :ca_certs, Array, default: []
# The attributes are wired by the recipes; the fallbacks keep the resource
# usable on its own and match the attribute defaults. The reads go through
# `node['microwave'][key]`, not `.fetch` on the mash: `[]` resolves a lazy
# attribute, `fetch` hands the raw evaluator to the property validation.
property :object_store_backend, String, default: lazy { node['microwave']['object_store_backend'] || 'local' }, equal_to: %w(local s3 garage)
property :object_store_bucket, String, default: lazy { node['microwave']['object_store_bucket'] || 'chef-cookbooks' }
property :object_store_endpoint, String
property :object_store_region, String, default: lazy { node['microwave']['object_store_region'] || 'garage' }
property :object_store_path_style, [true, false],
         default: lazy { node['microwave']['object_store_path_style'].nil? || node['microwave']['object_store_path_style'] }
property :object_store_access_key_file, String
property :object_store_secret_key_file, String

action :install do
  # The URLs are optional so `:disable` and `:enable` work without them. An
  # install still needs both, and fails before the release lookup reaches the
  # network.
  raise 'database_url is required to install the server' if new_resource.database_url.to_s.empty?
  raise 'search_url is required to install the server' if new_resource.search_url.to_s.empty?

  # The local backend needs nothing else. A remote one needs every connection
  # detail and both keys before the release lookup reaches the network. The
  # keys are read here, so a local install neither touches the files nor fails
  # when they are absent.
  object_store = {
    backend: new_resource.object_store_backend,
    bucket: new_resource.object_store_bucket,
    endpoint: new_resource.object_store_endpoint,
    region: new_resource.object_store_region,
    path_style: new_resource.object_store_path_style,
  }

  unless object_store[:backend] == 'local'
    # The key file properties follow the naming of the values they carry.
    object_store[:access_key_file] = new_resource.object_store_access_key_file
    object_store[:secret_key_file] = new_resource.object_store_secret_key_file

    { endpoint: object_store[:endpoint],
      bucket: object_store[:bucket],
      region: object_store[:region],
      access_key_file: object_store[:access_key_file],
      secret_key_file: object_store[:secret_key_file] }.each do |name, value|
      raise "object_store_#{name} is required for the #{object_store[:backend]} backend" if value.to_s.empty?
    end

    { access_key: object_store[:access_key_file],
      secret_key: object_store[:secret_key_file] }.each do |name, path|
      # A truncated key file passes a mere readability check and then fails
      # every S3 call with an opaque 403, so a blank value is rejected here too.
      object_store[name] =
        begin
          value = ::File.read(path).strip
          raise "object_store_#{name}_file #{path} is empty" if value.empty?

          value
        rescue SystemCallError => e
          raise "object_store_#{name}_file #{path} is not readable: #{e.message}"
        end
    end
  end

  release = Microwave::Release.asset(version: new_resource.version, arch: node['kernel']['machine'])
  package_path = ::File.join('/var/cache/microwave', release[:name])

  # dpkg installs the .deb without resolving its declared dependencies, so the
  # package has to be there first. A node that runs no backend package (the
  # split layout) has nothing else pulling logrotate in, and no apt index that
  # knows it either; the index is refreshed only while it cannot.
  # `apt_update` would report an update on every converge and break the
  # idempotency check, hence the guarded execute.
  execute 'apt-get update' do # cookstyle:disable Chef/Modernize/ExecuteAptUpdate
    not_if 'apt-cache show logrotate >/dev/null 2>&1'
  end

  apt_package 'logrotate'

  directory '/var/cache/microwave' do
    owner 'root'
    group 'root'
    mode '0755'
  end

  remote_file package_path do
    source release[:url]
    checksum release[:sha256]
    mode '0644'
    notifies :install, "dpkg_package[#{package_path}]", :immediately
  end

  dpkg_package package_path do
    options '--force-confold'
    notifies :restart, 'service[microwave-server]', :delayed
  end

  directory Microwave::TLS.ca_dir do
    owner 'root'
    group 'root'
    mode '0755'
  end

  new_resource.ca_certs.each_with_index do |pem, index|
    file "/usr/local/share/ca-certificates/microwave-backend-#{index + 1}.crt" do
      content pem
      owner 'root'
      group 'root'
      mode '0644'
      notifies :run, 'execute[update-ca-certificates]', :delayed
    end
  end

  execute 'update-ca-certificates' do
    action :nothing
  end

  template '/etc/microwave/microwave.toml' do
    source 'microwave.toml.erb'
    cookbook 'microwave'
    owner 'root'
    group 'microwave'
    mode '0640'
    # The rendered URL carries the database password.
    sensitive true
    variables(
      bind: new_resource.bind,
      public_url: new_resource.public_url,
      database_url: new_resource.database_url,
      search_url: new_resource.search_url,
      object_store: object_store
    )
    notifies :restart, 'service[microwave-server]', :delayed
  end

  directory '/etc/systemd/system/microwave-server.service.d' do
    owner 'root'
    group 'root'
    mode '0755'
  end

  template '/etc/systemd/system/microwave-server.service.d/ordering.conf' do
    source 'microwave-server.service.d/ordering.conf.erb'
    cookbook 'microwave'
    owner 'root'
    group 'root'
    mode '0644'
    notifies :run, 'execute[systemctl daemon-reload server]', :immediately
    notifies :restart, 'service[microwave-server]', :delayed
  end

  # The name carries the unit, the command does not: `systemctl daemon-reload`
  # takes no operand. In the all-in-one layout the OpenSearch resource declares
  # its own reload, and two clones under one name would let Chef pick either
  # copy and drop the other's attributes and notification.
  execute 'systemctl daemon-reload server' do
    command 'systemctl daemon-reload'
    action :nothing
    # A container without systemd starts the binary directly; calling systemctl
    # there would fail even though the drop-in file was written.
    only_if { ::File.exist?('/run/systemd/system') }
  end

  directory Microwave::Disabled::DIR do
    owner 'root'
    group 'root'
    mode '0755'
  end

  service 'microwave-server' do
    action %i(enable start)
    not_if { Microwave::Disabled.marked?('microwave-server') }
  end
end

action :disable do
  directory Microwave::Disabled::DIR do
    owner 'root'
    group 'root'
    mode '0755'
  end

  file Microwave::Disabled.marker('microwave-server') do
    content "disabled by microwave::disable\n"
    owner 'root'
    group 'root'
    mode '0644'
  end

  service 'microwave-server' do
    action %i(stop disable)
    only_if { Microwave::Disabled.unit?('microwave-server') }
  end
end

action :enable do
  file Microwave::Disabled.marker('microwave-server') do
    action :delete
  end

  service 'microwave-server' do
    action %i(enable start)
    only_if { Microwave::Disabled.unit?('microwave-server') }
  end
end
