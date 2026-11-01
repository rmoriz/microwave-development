require_relative 'spec_helper'

describe 'microwave_garage' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_garage']) do |node|
      node.name('garage.example.com')
      node.automatic['fqdn'] = 'garage.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::garage')
  end

  before do
    # The state guards shell out through `system`, not `stub_command`, and the
    # binary is absent on the spec host: they all report false and every
    # execute runs. Only the one backtick read gets a stub, so the commands
    # carry a node id.
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)
    %w(rpc_secret admin_token access_key secret_key).each do |name|
      stub_command("grep -qs \"[^[:space:]]\" /etc/microwave/garage.#{name}").and_return(false)
    end

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/garage.access_key').and_return("AKIAEXAMPLE\n")
    allow(::File).to receive(:read).with('/etc/microwave/garage.secret_key').and_return("SECRETKEY\n")
    allow_any_instance_of(Chef::Provider).to receive(:`).and_return("aa11bb22cc33@127.0.0.1:3901\n")

    allow(::File).to receive(:symlink?).and_call_original
    allow(::File).to receive(:symlink?).with('/etc/nginx/sites-enabled/default').and_return(true)
  end

  def rendered(path)
    ChefSpec::Renderer.new(chef_run, chef_run.template(path)).content
  end

  def converge
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_garage']) do |node|
      node.name('garage.example.com')
      node.automatic['fqdn'] = 'garage.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::garage')
  end

  it 'creates the service accounts and keeps the data directories tight' do
    expect(chef_run).to create_group('microwave').with(system: true)
    expect(chef_run).to create_group('garage').with(system: true)
    expect(chef_run).to create_user('garage').with(system: true, shell: '/usr/sbin/nologin')

    expect(chef_run).to create_directory('/var/lib/garage/meta')
      .with(owner: 'garage', group: 'garage', mode: '0700')
    expect(chef_run).to create_directory('/var/lib/garage/data')
      .with(owner: 'garage', group: 'garage', mode: '0700')
  end

  it 'downloads the pinned static binary from the release path with its digest' do
    expect(chef_run).to create_remote_file('/usr/local/bin/garage').with(
      source: 'https://garagehq.deuxfleurs.fr/_releases/v2.5.0/x86_64-unknown-linux-musl/garage',
      checksum: '2ff667286217cd73ebcb1c03af993822365000441dfe31a80cb64c5189a9dea8',
      owner: 'root',
      group: 'root',
      mode: '0755'
    )
  end

  it 'writes the engine configuration with file-backed secrets' do
    expect(chef_run).to create_template('/etc/garage.toml')
      .with(owner: 'root', group: 'garage', mode: '0640')

    content = rendered('/etc/garage.toml')

    expect(content).to include(
      'replication_factor = 1',
      'db_engine = "sqlite"',
      'rpc_bind_addr = "[::]:3901"',
      'rpc_secret_file = "/etc/microwave/garage.rpc_secret"',
      'api_bind_addr = "127.0.0.1:3910"',
      'public_endpoint = "https://garage.example.com:3900"',
      'public_endpoint_path_style = true',
      'admin_token_file = "/etc/microwave/garage.admin_token"'
    )
    expect(content).not_to match(/^rpc_secret =/)
    expect(content).not_to match(/^admin_token =/)
  end

  it 'leaves a secret file that already has content alone' do
    names = %w(rpc_secret admin_token access_key secret_key)

    names.each do |name|
      stub_command("grep -qs \"[^[:space:]]\" /etc/microwave/garage.#{name}").and_return(true)
    end

    run = converge
    names.each do |name|
      expect(run.file("/etc/microwave/garage.#{name}").action).to eq([:create])
      expect(run.file("/etc/microwave/garage.#{name}").not_if.map(&:command))
        .to eq(["grep -qs \"[^[:space:]]\" /etc/microwave/garage.#{name}"])
    end
  end

  it 'writes the engine secrets for the engine group' do
    expect(chef_run).to create_file('/etc/microwave/garage.rpc_secret')
      .with(owner: 'garage', group: 'garage', mode: '0600')
    expect(chef_run).to create_file('/etc/microwave/garage.admin_token')
      .with(owner: 'garage', group: 'garage', mode: '0600')

    expect(chef_run.file('/etc/microwave/garage.rpc_secret').content).to match(/\A[0-9a-f]{64}\n\z/)
    expect(chef_run.file('/etc/microwave/garage.admin_token').content).to match(/\A[0-9a-f]{64}\n\z/)
  end

  it 'writes the S3 key pair for the server user' do
    expect(chef_run).to create_file('/etc/microwave/garage.access_key')
      .with(owner: 'root', group: 'microwave', mode: '0640')
    expect(chef_run).to create_file('/etc/microwave/garage.secret_key')
      .with(owner: 'root', group: 'microwave', mode: '0640')

    expect(chef_run.file('/etc/microwave/garage.access_key').content).to match(/\A[0-9a-f]{40}\n\z/)
    expect(chef_run.file('/etc/microwave/garage.secret_key').content).to match(/\A[0-9a-f]{64}\n\z/)
  end

  it 'issues a certificate with the loopback SAN and a tight key' do
    expect(chef_run.execute('microwave-create-service-certificate').command)
      .to include('-subj "/CN=garage.example.com"',
                  '-addext "subjectAltName=DNS:garage.example.com,IP:127.0.0.1"',
                  '-extfile /etc/microwave/tls/backend.cnf',
                  '/etc/microwave/tls/garage.key', '/etc/microwave/tls/garage.crt')

    expect(chef_run).to create_file('/etc/microwave/tls/garage.key')
      .with(owner: 'garage', group: 'garage', mode: '0600')
    expect(chef_run).to create_file('/etc/microwave/tls/garage.crt')
      .with(owner: 'garage', group: 'garage', mode: '0640')
  end

  it 'keeps the CA shared and root-owned' do
    expect(chef_run).to create_file('/etc/microwave/tls/ca.key')
      .with(owner: 'root', group: 'root', mode: '0640')
    expect(chef_run).to create_file('/etc/microwave/tls/ca.crt')
      .with(owner: 'root', group: 'root', mode: '0644')
  end

  it 'checks verify, the key pair and the SAN before keeping a certificate' do
    guard = chef_run.execute('microwave-create-service-certificate').not_if.first.command

    expect(guard).to start_with('test -f /etc/microwave/tls/garage.key && test -f /etc/microwave/tls/garage.crt')
    expect(guard).to include('openssl verify', '-CAfile /etc/microwave/tls/ca.crt')
    expect(guard).to include('grep -qE "DNS:garage\.example\.com(,|$)"')
  end

  it 'installs nginx and declares a marker-guarded reload target' do
    expect(chef_run).to install_apt_package('nginx')
    expect(chef_run).to enable_service('nginx')
    expect(chef_run).to start_service('nginx')
    expect(chef_run.service('nginx').supports).to include(reload: true)
    expect(chef_run.service('nginx').not_if.first.block.call).to be(false)
  end

  it 'publishes the engine through nginx TLS on the public port' do
    vhost = rendered('/etc/nginx/sites-available/microwave-garage')

    expect(vhost).to include('listen garage.example.com:3900 ssl;')
    # Loopback is how the server on this node reaches the store.
    expect(vhost).to include('listen 127.0.0.1:3900 ssl;')
    expect(vhost).to include('server_name garage.example.com;')
    expect(vhost).to include('ssl_certificate /etc/microwave/tls/garage.crt;')
    expect(vhost).to include('ssl_certificate_key /etc/microwave/tls/garage.key;')
    expect(vhost).to include('client_max_body_size 0;')
    expect(vhost).to include('proxy_request_buffering off;')
    expect(vhost).to include('access_log /var/log/nginx/microwave-garage.access.log;')
    expect(vhost).to include('proxy_pass http://127.0.0.1:3910;')
    expect(vhost).to include('proxy_set_header Host $http_host;')
    # $host would drop the port the signature covers.
    expect(vhost).not_to match(/Host \$host;/)
    expect(vhost).to include('proxy_set_header X-Real-IP $remote_addr;')
    expect(vhost).to include('proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;')
    expect(vhost).not_to include('allow ')
  end

  it 'enables the vhost and drops the packaged default site' do
    expect(chef_run).to create_link('/etc/nginx/sites-enabled/microwave-garage')
      .with(to: '/etc/nginx/sites-available/microwave-garage')
    expect(chef_run).to delete_link('/etc/nginx/sites-enabled/default')
  end

  it 'reloads nginx when the vhost, the link or the default site changes' do
    expect(chef_run.template('/etc/nginx/sites-available/microwave-garage'))
      .to notify('service[nginx]').to(:reload).delayed
    expect(chef_run.link('/etc/nginx/sites-enabled/microwave-garage'))
      .to notify('service[nginx]').to(:reload).delayed
    expect(chef_run.link('/etc/nginx/sites-enabled/default'))
      .to notify('service[nginx]').to(:reload).delayed
  end

  it 'writes the unit and reloads systemd for it' do
    unit = '/etc/systemd/system/garage.service'

    expect(chef_run).to create_template(unit)
      .with(owner: 'root', group: 'root', mode: '0644')

    content = rendered(unit)

    expect(content).to include(
      'ExecStart=/usr/local/bin/garage -c /etc/garage.toml server',
      'User=garage', 'Group=garage', 'Environment=GARAGE_LOG_TO_JOURNALD=1',
      'Restart=on-failure', 'NoNewPrivileges=true'
    )
    expect(chef_run.template(unit)).to notify('execute[systemctl daemon-reload garage]').immediately
    expect(chef_run.template(unit)).to notify('service[garage]').delayed

    reload = chef_run.execute('systemctl daemon-reload garage')

    expect(reload.command).to eq('systemctl daemon-reload')
    expect(reload.action).to eq([:nothing])
    expect(reload.only_if.first.block).to be_kind_of(Proc)
  end

  it 'enables and starts the unit before it provisions' do
    expect(chef_run).to enable_service('garage')
    expect(chef_run).to start_service('garage')
    expect(chef_run.service('garage').not_if.first.block.call).to be(false)

    names = chef_run.resource_collection.map(&:to_s)
    expect(names.index('service[garage]')).to be < names.index('execute[microwave-garage-wait]')
  end

  it 'waits for the admin API, bounded, before staging the layout' do
    wait = chef_run.execute('microwave-garage-wait')

    expect(wait.command)
      .to eq('for i in $(seq 60); do /usr/local/bin/garage -c /etc/garage.toml status >/dev/null 2>&1 && exit 0; sleep 2; done; exit 1')
    expect(wait.not_if.first.block).to be_kind_of(Proc)
    expect(wait.not_if.first.block.call).to be(false)
    expect(wait.user).to eq('garage')
    expect(wait.group).to eq('garage')
  end

  it 'stages the only node with the configured capacity' do
    assign = chef_run.execute('microwave-garage-layout-assign')

    expect(assign.command)
      .to eq('/usr/local/bin/garage -c /etc/garage.toml layout assign -z dc1 -c 1G aa11bb22cc33')
    expect(assign.not_if.size).to eq(2)
    expect(assign.not_if.first.block.call).to be(false)
    expect(assign.not_if.last.block.call).to be(false)
  end

  it 'applies the staged version once' do
    apply = chef_run.execute('microwave-garage-layout-apply')

    expect(apply.command).to eq('/usr/local/bin/garage -c /etc/garage.toml layout apply --version 1')
    expect(apply.not_if.last).to be_kind_of(Chef::Resource::Conditional)
  end

  it 'creates the bucket guarded by its absence' do
    create = chef_run.execute('microwave-garage-bucket-create')

    expect(create.command).to eq('/usr/local/bin/garage -c /etc/garage.toml bucket create "chef-cookbooks"')
    expect(create.not_if.size).to eq(2)
  end

  it 'imports the generated key pair once, sensitive' do
    import = chef_run.execute('microwave-garage-key-import')

    expect(import.command)
      .to eq('/usr/local/bin/garage -c /etc/garage.toml key import AKIAEXAMPLE SECRETKEY -n microwave --yes')
    expect(import.sensitive).to be(true)
    expect(import.not_if.size).to eq(2)
  end

  it 'grants the key read, write and owner on the bucket' do
    allow_step = chef_run.execute('microwave-garage-bucket-allow')

    expect(allow_step.command)
      .to eq('/usr/local/bin/garage -c /etc/garage.toml bucket allow --read --write --owner --key AKIAEXAMPLE "chef-cookbooks"')
    expect(allow_step.not_if.size).to eq(2)
    expect(allow_step.not_if.first.block.call).to be(false)
    expect(allow_step.not_if.last.block.call).to be(false)
  end

  it 'guards every provisioning step with the marker first and runs as garage' do
    %w(wait layout-assign layout-apply bucket-create key-import bucket-allow).each do |step|
      execute = chef_run.execute("microwave-garage-#{step}")

      expect(execute.user).to eq('garage')
      expect(execute.group).to eq('garage')
      expect(execute.not_if.first.block).to be_kind_of(Proc)
      expect(execute.not_if.size).to eq(2)
    end
  end

  it 'does not run the provisioning against a stopped engine' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/garage').and_return(true)

    run = converge

    %w(wait layout-assign layout-apply bucket-create key-import bucket-allow).each do |step|
      expect(run).not_to run_execute("microwave-garage-#{step}")
    end
    expect(run).not_to enable_service('garage')
  end
end

describe 'microwave_garage with a client network' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_garage']) do |node|
      node.name('garage.example.com')
      node.automatic['fqdn'] = 'garage.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
      node.normal['microwave']['garage_capacity'] = '5G'
      node.normal['microwave']['client_network'] = '10.0.0.0/8'
    end.converge('microwave-test::garage_client_network')
  end

  before do
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)
    %w(rpc_secret admin_token access_key secret_key).each do |name|
      stub_command("grep -qs \"[^[:space:]]\" /etc/microwave/garage.#{name}").and_return(false)
    end

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/garage.access_key').and_return("AKIAEXAMPLE\n")
    allow(::File).to receive(:read).with('/etc/microwave/garage.secret_key').and_return("SECRETKEY\n")
    allow_any_instance_of(Chef::Provider).to receive(:`).and_return("aa11bb22cc33@127.0.0.1:3901\n")

    allow(::File).to receive(:symlink?).and_call_original
    allow(::File).to receive(:symlink?).with('/etc/nginx/sites-enabled/default').and_return(true)
  end

  def rendered(path)
    ChefSpec::Renderer.new(chef_run, chef_run.template(path)).content
  end

  it 'keeps loopback reachable and narrows the network' do
    vhost = rendered('/etc/nginx/sites-available/microwave-garage')

    expect(vhost).to include('listen garage.example.com:3900 ssl;')
    expect(vhost).to include('listen 127.0.0.1:3900 ssl;')
    expect(vhost).to match(%r{^  allow 10\.0\.0\.0/8;$})
    expect(vhost).to match(/^  deny all;$/)
  end

  it 'passes the capacity through' do
    expect(chef_run.execute('microwave-garage-layout-assign').command)
      .to eq('/usr/local/bin/garage -c /etc/garage.toml layout assign -z dc1 -c 5G aa11bb22cc33')
  end
end

describe 'microwave_garage switching' do
  before do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/lib/systemd/system/garage.service').and_return(true)
  end

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_garage'])
                        .converge('microwave-test::garage_disable')
  end

  it 'stops the unit, disables it and drops the marker' do
    expect(chef_run).to stop_service('garage')
    expect(chef_run).to disable_service('garage')
    expect(chef_run).to create_file('/etc/microwave/disabled/garage')
      .with(owner: 'root', group: 'root', mode: '0644')
  end

  it 'skips a unit that does not exist' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/lib/systemd/system/garage.service').and_return(false)
    guard = chef_run.service('garage').only_if.first.block

    expect(guard.call).to be(false)
  end
end
