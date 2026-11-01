unified_mode true

description 'Installs the Microwave Server package, its configuration and its service'

property :database_url, String, required: true
property :search_url, String, required: true
property :bind, String, default: lazy { node['microwave']['bind'] }
property :public_url, String
property :version, String, default: 'latest', regex: /\A(latest|\d+\.\d+\.\d+)\z/
property :ca_certs, Array, default: []

action :install do
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
      search_url: new_resource.search_url
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
    notifies :run, 'execute[systemctl daemon-reload]', :immediately
    notifies :restart, 'service[microwave-server]', :delayed
  end

  execute 'systemctl daemon-reload' do
    action :nothing
  end

  service 'microwave-server' do
    action %i(enable start)
  end
end
