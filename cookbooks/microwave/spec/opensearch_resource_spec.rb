require_relative 'spec_helper'

describe 'microwave_opensearch' do
  include_context 'opensearch certificate guards'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_opensearch']) do |node|
      node.name('search.example.com')
      node.automatic['fqdn'] = 'search.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::search')
  end

  before do
    stub_certificate_guards
    stub_command(%r{test -d /usr/share/opensearch/plugins/opensearch-security}).and_return(nil)

    allow(::File).to receive(:symlink?).and_call_original
    allow(::File).to receive(:symlink?).with('/etc/nginx/sites-enabled/default').and_return(true)

    # The spec host decides whether systemd is there; pin it so the systemd
    # branch is taken everywhere and the direct-start example flips it.
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/run/systemd/system').and_return(true)
  end

  def rendered(path)
    ChefSpec::Renderer.new(chef_run, chef_run.template(path)).content
  end

  it 'adds the OpenSearch 3.x repository signed by the shipped keyring' do
    # The key file at artifacts.opensearch.org is stale and apt's sequoia
    # verifier rejects the InRelease, so the signing certificate is shipped.
    expect(chef_run).to create_cookbook_file('/etc/apt/keyrings/opensearch.asc')
      .with(source: 'opensearch.asc', cookbook: 'microwave', mode: '0644')
    expect(chef_run).to add_apt_repository('opensearch').with(
      uri: 'https://artifacts.opensearch.org/releases/bundle/opensearch/3.x/apt',
      distribution: 'stable',
      components: %w(main),
      signed_by: '/etc/apt/keyrings/opensearch.asc'
    )
  end

  it 'pins 3.9.0 at priority 600' do
    expect(chef_run).to add_apt_preference('opensearch')
      .with(pin: 'version 3.9.0', pin_priority: '600')
  end

  it 'installs the pinned package without asking for an admin password' do
    expect(chef_run).to install_apt_package('opensearch').with(
      environment: {
        'DISABLE_INSTALL_DEMO_CONFIG' => 'true',
        'DISABLE_SECURITY_PLUGIN' => 'true',
      }
    )
  end

  it 'writes the single-node configuration on the internal port' do
    expect(rendered('/etc/opensearch/opensearch.yml')).to eq(<<~YML)
      cluster.name: microwave
      node.name: search.example.com
      network.host: 127.0.0.1
      http.port: 9210
      discovery.type: single-node
      # No `plugins.security.disabled`: the resource deletes the security plugin
      # directory, and with the plugin gone the key is unknown to OpenSearch, so it
      # would only warn. The proxy in front is the only access control.
      path.data: /var/lib/opensearch/_data
      path.logs: /var/log/opensearch/_logs
      bootstrap.memory_lock: false
      # cgroup memory limits are unreadable under systemd, so size the buffer
      # absolutely instead of by a percentage of a total we cannot read.
      indices.memory.native_index_buffer_size: 256mb
    YML
  end

  it 'drops plugins.security.disabled, which is unknown once the plugin is gone' do
    expect(rendered('/etc/opensearch/opensearch.yml')).not_to match(/^plugins\.security/)
  end

  it 'trusts nginx to terminate TLS, so OpenSearch itself stays plain' do
    content = rendered('/etc/opensearch/opensearch.yml')

    expect(content).not_to include('plugins.security.ssl.http.enabled')
    expect(content).not_to include('security.demo.config')
  end

  it 'disables the security plugin, guarded by its presence' do
    plugin = '/usr/share/opensearch/plugins/opensearch-security'
    stub_command("test -d #{plugin}").and_return(true)

    expect(chef_run).to delete_directory(plugin).with(recursive: true)
    expect(chef_run.directory(plugin).only_if.first.command).to eq("test -d #{plugin}")
  end

  it 'restarts OpenSearch once the plugin is gone' do
    expect(chef_run.directory('/usr/share/opensearch/plugins/opensearch-security'))
      .to notify('service[opensearch]').delayed
  end

  it 'writes a heap file from the attribute, 0644 root:root' do
    expect(chef_run).to create_directory('/etc/opensearch/jvm.options.d')
      .with(owner: 'root', group: 'root', mode: '0755')
    expect(chef_run).to create_template('/etc/opensearch/jvm.options.d/heap.options')
      .with(owner: 'root', group: 'root', mode: '0644')
    expect(rendered('/etc/opensearch/jvm.options.d/heap.options')).to eq("-Xms1g\n-Xmx1g\n")
  end

  it 'restarts OpenSearch when the heap changes' do
    expect(chef_run.template('/etc/opensearch/jvm.options.d/heap.options'))
      .to notify('service[opensearch]').delayed
  end

  it 'raises vm.max_map_count through sysctl' do
    expect(chef_run).to apply_sysctl('vm.max_map_count').with(value: '262144')
    expect(chef_run.sysctl('vm.max_map_count'))
      .to notify('service[opensearch]').delayed
  end

  it 'raises the locked memory limit on the OpenSearch unit, which maps it' do
    path = '/etc/systemd/system/opensearch.service.d/memlock.conf'

    expect(chef_run).to create_directory('/etc/systemd/system/opensearch.service.d')
      .with(owner: 'root', group: 'root', mode: '0755')
    expect(chef_run).to create_template(path).with(owner: 'root', group: 'root', mode: '0644')
    expect(rendered(path)).to include('LimitMEMLOCK=infinity')
    expect(chef_run.template(path)).to notify('execute[systemctl daemon-reload opensearch]').immediately
    expect(chef_run.template(path)).to notify('service[opensearch]').delayed
    expect(chef_run.execute('systemctl daemon-reload opensearch')).to do_nothing
  end

  # The all-in-one layout clones both resources, so the reload name carries the
  # unit and the notification stays unambiguous.
  it 'names its daemon-reload after the unit and guards it on systemd' do
    reloads = chef_run.resource_collection.select { |r| r.name.to_s.start_with?('systemctl daemon-reload') }

    # `systemctl daemon-reload` takes no unit operand, so the bare command stays
    # while the resource name pins the copy.
    expect(reloads.map(&:name)).to eq(['systemctl daemon-reload opensearch'])
    expect(reloads.first.command).to eq('systemctl daemon-reload')
    expect(reloads.first.only_if.first.block).to be_a(Proc)
  end

  it 'issues a CA and a search certificate named for the hostname' do
    stub_certificate_guards(pair: false)

    expect(chef_run).to run_execute('microwave-generate-ca-key')
    expect(chef_run.execute('microwave-create-ca').command)
      .to include('-x509', '-days 3650', '-subj "/CN=microwave-ca"', '/etc/microwave/tls/ca.key',
                  '/etc/microwave/tls/ca.crt')

    expect(chef_run).to run_execute('microwave-generate-service-key')
    expect(chef_run.execute('microwave-create-service-certificate').command)
      .to include('-subj "/CN=search.example.com"',
                  '-addext "subjectAltName=DNS:search.example.com,IP:127.0.0.1"',
                  '-copy_extensions copy', '-extfile /etc/microwave/tls/backend.cnf',
                  '-extensions ext', '-days 825',
                  '/etc/microwave/tls/search.key', '/etc/microwave/tls/search.crt')
  end

  it 'keeps an existing certificate pair with a matching name' do
    expect(chef_run).not_to run_execute('microwave-create-service-certificate')
  end

  it 're-issues the certificate when the certificate and the key are not a pair' do
    stub_certificate_guards(pair: false)

    expect(chef_run).to run_execute('microwave-create-service-certificate')
  end

  it 're-issues the certificate when the SAN no longer names the hostname' do
    stub_certificate_guards(san: false)

    expect(chef_run).to run_execute('microwave-create-service-certificate')
  end

  it 'checks verify, the key pair and the SAN before keeping a certificate' do
    guard = chef_run.execute('microwave-create-service-certificate').not_if.first.command

    expect(guard).to start_with('test -f /etc/microwave/tls/search.key && test -f /etc/microwave/tls/search.crt')
    expect(guard).to include('openssl verify', '-CAfile /etc/microwave/tls/ca.crt')
    expect(guard).to include(
      '[ "$(openssl x509 -in /etc/microwave/tls/search.crt -pubkey -noout)" = ' \
      '"$(openssl pkey -in /etc/microwave/tls/search.key -pubout)" ]'
    )
    expect(guard).to include('grep -qE "DNS:search\.example\.com(,|$)"')
  end

  it 're-issues the CA when the certificate and the key belong to different pairs' do
    guard = chef_run.execute('microwave-create-ca').not_if.first.command

    expect(guard).to start_with('test -f /etc/microwave/tls/ca.key && test -f /etc/microwave/tls/ca.crt')
    expect(guard).to include(
      '[ "$(openssl x509 -in /etc/microwave/tls/ca.crt -pubkey -noout)" = ' \
      '"$(openssl pkey -in /etc/microwave/tls/ca.key -pubout)" ]'
    )
  end

  it 'owns the key material root or opensearch and keeps it 0640' do
    # The CA is shared with the database resource, so it stays with root.
    expect(chef_run).to create_file('/etc/microwave/tls/ca.key')
      .with(owner: 'root', group: 'root', mode: '0640')
    expect(chef_run).to create_file('/etc/microwave/tls/search.key')
      .with(owner: 'opensearch', group: 'opensearch', mode: '0640')
    expect(chef_run).to create_file('/etc/microwave/tls/search.crt')
      .with(owner: 'opensearch', group: 'opensearch', mode: '0640')
  end

  it 'declares the shared SAN extension file like the database does' do
    expect(chef_run).to create_template('/etc/microwave/tls/backend.cnf')
      .with(source: 'backend-cert.cnf.erb', cookbook: 'microwave',
            owner: 'root', group: 'root', mode: '0644')
    expect(rendered('/etc/microwave/tls/backend.cnf'))
      .to include('subjectAltName = DNS:search.example.com,IP:127.0.0.1')
  end

  it 'restarts OpenSearch when a certificate changes' do
    expect(chef_run.execute('microwave-create-service-certificate'))
      .to notify('service[opensearch]').delayed
  end

  it 'keeps the data and log state directories for the service account' do
    expect(chef_run).to create_directory('/var/lib/opensearch/_data')
      .with(owner: 'opensearch', group: 'opensearch')
    expect(chef_run).to create_directory('/var/log/opensearch/_logs')
      .with(owner: 'opensearch', group: 'opensearch')
  end

  it 'installs nginx and terminates TLS on the public port' do
    expect(chef_run).to install_apt_package('nginx').with(
      environment: {
        'DISABLE_INSTALL_DEMO_CONFIG' => 'true',
        'DISABLE_SECURITY_PLUGIN' => 'true',
      }
    )

    vhost = rendered('/etc/nginx/sites-available/microwave-opensearch')

    expect(vhost).to include('listen 127.0.0.1:9200 ssl;')
    expect(vhost).to include('server_name search.example.com;')
    expect(vhost).to include('ssl_certificate /etc/microwave/tls/search.crt;')
    expect(vhost).to include('ssl_certificate_key /etc/microwave/tls/search.key;')
    expect(vhost).to include('ssl_protocols TLSv1.2 TLSv1.3;')
    expect(vhost).to include('client_max_body_size 100m;')
    expect(vhost).to include('proxy_read_timeout 300s;')
    expect(vhost).to include('proxy_send_timeout 300s;')
    expect(vhost).to include('access_log /var/log/nginx/microwave-opensearch.access.log;')
    expect(vhost).to include('proxy_pass http://127.0.0.1:9210;')
    expect(vhost).to include('proxy_set_header Host $host;')
    expect(vhost).to include('proxy_set_header X-Real-IP $remote_addr;')
    expect(vhost).to include('proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;')
  end

  it 'leaves the vhost open when no client network is set' do
    vhost = rendered('/etc/nginx/sites-available/microwave-opensearch')

    expect(vhost).not_to include('allow ')
    expect(vhost).not_to include('deny all;')
  end

  it 'enables the vhost and drops the packaged default site' do
    expect(chef_run).to create_link('/etc/nginx/sites-enabled/microwave-opensearch')
      .with(to: '/etc/nginx/sites-available/microwave-opensearch')
    expect(chef_run).to delete_link('/etc/nginx/sites-enabled/default')
  end

  it 'reloads nginx when the vhost, the link or the default site changes' do
    expect(chef_run.template('/etc/nginx/sites-available/microwave-opensearch'))
      .to notify('service[nginx]').to(:reload).delayed
    expect(chef_run.link('/etc/nginx/sites-enabled/microwave-opensearch'))
      .to notify('service[nginx]').to(:reload).delayed
    expect(chef_run.link('/etc/nginx/sites-enabled/default'))
      .to notify('service[nginx]').to(:reload).delayed
  end

  it 'enables and starts OpenSearch before nginx' do
    expect(chef_run).to enable_service('opensearch')
    expect(chef_run).to start_service('opensearch')
    expect(chef_run).to enable_service('nginx')
    expect(chef_run).to start_service('nginx')

    names = chef_run.resource_collection.map(&:to_s)
    expect(names.index('service[opensearch]')).to be < names.index('service[nginx]')
  end

  # Without systemd the packaged init script cannot start or stop the daemon, so
  # the resource drives it itself and has to provide the pid directory.
  it 'starts OpenSearch directly when systemd is absent' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/run/systemd/system').and_return(false)

    expect(chef_run).to create_directory('/var/run/opensearch')
      .with(owner: 'opensearch', group: 'opensearch', mode: '0755')

    service = chef_run.service('opensearch')
    expect(service.start_command).to include('start-stop-daemon')
    expect(service.start_command).to include('--chuid opensearch')
    expect(service.start_command).to include('OPENSEARCH_PATH_CONF=/etc/opensearch')
    expect(service.stop_command).to include('--retry TERM/30/KILL/5')
    expect(service.restart_command).to include('--stop')
    expect(service.status_command).to include('--status')
    expect(chef_run).to start_service('opensearch')
    expect(chef_run).not_to enable_service('opensearch')
  end

  it 'installs curl for the readiness probe, after refreshing the lists' do
    expect(chef_run).to periodic_apt_update('curl')
    expect(chef_run).to install_apt_package('curl')

    names = chef_run.resource_collection.map(&:to_s)
    expect(names.index('apt_update[curl]')).to be < names.index('apt_package[curl]')
  end

  # A restart right after the start caught the engine mid-boot, so the probe
  # runs as its own step and repeats itself after the delayed restart.
  it 'waits for the engine and re-probes after a restart' do
    wait = chef_run.execute('microwave-opensearch-wait')

    expect(wait.command)
      .to eq('for i in $(seq 150); do curl -fsS --max-time 5 http://127.0.0.1:9210/ >/dev/null 2>&1 && exit 0; sleep 2; done; exit 1')
    expect(wait.not_if.first.block.call).to be(false)
    # A serving engine makes the probe a no-op, so a second converge that
    # changes nothing stays change-free.
    expect(wait.not_if.last.command).to eq('curl -fsS --max-time 5 http://127.0.0.1:9210/ >/dev/null 2>&1')

    names = chef_run.resource_collection.map(&:to_s)
    expect(names.index('service[opensearch]')).to be < names.index('execute[microwave-opensearch-wait]')
    expect(names.index('execute[microwave-opensearch-wait]')).to be < names.index('service[nginx]')
  end

  # Right after a restart the old process can still answer, which would skip the
  # guarded probe. The restart therefore notifies a second, unguarded wait.
  it 'notifies an unguarded wait for a restart' do
    restart_wait = chef_run.execute('microwave-opensearch-restart-wait')

    expect(chef_run.service('opensearch'))
      .to notify('execute[microwave-opensearch-restart-wait]').to(:run).delayed
    expect(restart_wait.command).to eq(chef_run.execute('microwave-opensearch-wait').command)
    expect(restart_wait.not_if.map(&:command)).to eq([nil])
    expect(restart_wait.action).to eq([:nothing])

    names = chef_run.resource_collection.map(&:to_s)
    expect(names.index('execute[microwave-opensearch-wait]'))
      .to be < names.index('execute[microwave-opensearch-restart-wait]')
  end
end

describe 'microwave_opensearch with a client network' do
  include_context 'opensearch certificate guards'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_opensearch']) do |node|
      node.name('search.example.com')
      node.automatic['fqdn'] = 'search.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
      node.normal['microwave']['client_network'] = '10.0.0.0/8'
    end.converge('microwave-test::search')
  end

  before do
    stub_certificate_guards
    stub_command(%r{test -d /usr/share/opensearch/plugins/opensearch-security}).and_return(nil)

    allow(::File).to receive(:symlink?).and_call_original
    allow(::File).to receive(:symlink?).with('/etc/nginx/sites-enabled/default').and_return(true)

    # The spec host decides whether systemd is there; pin it so the systemd
    # branch is taken everywhere and the direct-start example flips it.
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/run/systemd/system').and_return(true)
  end

  def rendered(path)
    ChefSpec::Renderer.new(chef_run, chef_run.template(path)).content
  end

  it 'binds the TLS listener to the hostname and keeps loopback reachable' do
    vhost = rendered('/etc/nginx/sites-available/microwave-opensearch')

    expect(vhost).to include('listen search.example.com:9200 ssl;')
    expect(vhost).to include('listen 127.0.0.1:9200 ssl;')
    expect(vhost).to include('proxy_pass http://127.0.0.1:9210;')
    expect(chef_run).to render_file('/etc/opensearch/opensearch.yml')
      .with_content(/^network\.host: 127\.0\.0\.1$/)
  end

  it 'allows only the client network and loopback, since security is off' do
    vhost = rendered('/etc/nginx/sites-available/microwave-opensearch')

    expect(vhost).to match(%r{^  allow 10\.0\.0\.0/8;$})
    expect(vhost).to match(/^  allow 127\.0\.0\.1;$/)
    expect(vhost).to match(/^  allow ::1;$/)
    expect(vhost).to match(/^  deny all;$/)
  end
end

describe 'microwave_opensearch with a custom heap' do
  include_context 'opensearch certificate guards'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_opensearch']) do |node|
      node.automatic['fqdn'] = 'search.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
      node.normal['microwave']['opensearch_heap'] = '2g'
    end.converge('microwave-test::search')
  end

  before do
    stub_certificate_guards
    stub_command(%r{test -d /usr/share/opensearch/plugins/opensearch-security}).and_return(nil)

    allow(::File).to receive(:symlink?).and_call_original
    allow(::File).to receive(:symlink?).with('/etc/nginx/sites-enabled/default').and_return(true)

    # The spec host decides whether systemd is there; pin it so the systemd
    # branch is taken everywhere and the direct-start example flips it.
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/run/systemd/system').and_return(true)
  end

  it 'uses the heap for both bounds' do
    expect(ChefSpec::Renderer.new(chef_run, chef_run.template('/etc/opensearch/jvm.options.d/heap.options')).content)
      .to eq("-Xms2g\n-Xmx2g\n")
  end
end

describe 'microwave_opensearch without an FQDN' do
  include_context 'opensearch certificate guards'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_opensearch']) do |node|
      node.automatic['fqdn'] = ''
      node.automatic['hostname'] = 'search-short'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::search')
  end

  before do
    stub_certificate_guards
    stub_command(%r{test -d /usr/share/opensearch/plugins/opensearch-security}).and_return(nil)

    allow(::File).to receive(:symlink?).and_call_original
    allow(::File).to receive(:symlink?).with('/etc/nginx/sites-enabled/default').and_return(true)

    # The spec host decides whether systemd is there; pin it so the systemd
    # branch is taken everywhere and the direct-start example flips it.
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/run/systemd/system').and_return(true)
  end

  it 'names the certificate after the hostname' do
    expect(chef_run.execute('microwave-create-service-certificate').command)
      .to include('-subj "/CN=search-short"', 'DNS:search-short,IP:127.0.0.1')
  end
end

describe 'microwave_opensearch switching' do
  include_context 'opensearch certificate guards'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_opensearch']) do |node|
      node.name('search.example.com')
      node.automatic['fqdn'] = 'search.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::search_disable')
  end

  before do
    stub_certificate_guards
    stub_command(%r{test -d /usr/share/opensearch/plugins/opensearch-security}).and_return(nil)

    allow(::File).to receive(:symlink?).and_call_original
    allow(::File).to receive(:symlink?).with('/etc/nginx/sites-enabled/default').and_return(true)

    # The unit is looked up in three directories, so the switch guard would
    # otherwise depend on the spec host; it exists everywhere, and the examples
    # below flip it where they care. The systemd pin makes the guard's branch
    # independent of the host as well.
    allow(::File).to receive(:exist?).and_call_original
    Microwave::Disabled::UNIT_DIRS.each do |dir|
      allow(::File).to receive(:exist?).with("#{dir}/opensearch.service").and_return(true)
    end
    allow(::File).to receive(:exist?).with('/run/systemd/system').and_return(true)
  end

  it 'stops the unit, disables it and drops the marker' do
    expect(chef_run).to stop_service('opensearch')
    expect(chef_run).to disable_service('opensearch')
    expect(chef_run).to create_file('/etc/microwave/disabled/opensearch')
      .with(owner: 'root', group: 'root', mode: '0644')
  end

  # nginx belongs to the recipe, so enabling the backend does not start it again.
  it 'leaves nginx to the recipe rather than the resource' do
    expect(chef_run.resource_collection.select { |r| r.resource_name == :service }.map(&:name))
      .to eq(%w(opensearch))
  end

  it 'starts the daemon directly again on enable' do
    allow(::File).to receive(:exist?).and_call_original
    Microwave::Disabled::UNIT_DIRS.each do |dir|
      allow(::File).to receive(:exist?).with("#{dir}/opensearch.service").and_return(true)
    end
    allow(::File).to receive(:exist?).with('/run/systemd/system').and_return(false)

    run = ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_opensearch']) do |node|
      node.name('search.example.com')
      node.automatic['fqdn'] = 'search.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::search_enable')

    expect(run).to start_service('opensearch')
    expect(run).not_to enable_service('opensearch')
  end

  it 'skips a unit that does not exist' do
    allow(::File).to receive(:exist?).and_call_original
    Microwave::Disabled::UNIT_DIRS.each do |dir|
      allow(::File).to receive(:exist?).with("#{dir}/opensearch.service").and_return(false)
    end
    allow(::File).to receive(:exist?).with('/run/systemd/system').and_return(true)
    guard = chef_run.service('opensearch').only_if.first.block

    expect(guard.call).to be(false)
  end

  it 'does not enable or start a marked unit on install' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/opensearch').and_return(true)
    guard = ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_opensearch']) do |node|
      node.name('search.example.com')
      node.automatic['fqdn'] = 'search.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::search').service('opensearch').not_if.first.block

    expect(guard.call).to be(true)
  end

  it 'does not start the nginx proxy for a marked nginx on install' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/nginx').and_return(true)
    guard = ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_opensearch']) do |node|
      node.name('search.example.com')
      node.automatic['fqdn'] = 'search.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::search').service('nginx').not_if.first.block

    expect(guard.call).to be(true)
  end
end
