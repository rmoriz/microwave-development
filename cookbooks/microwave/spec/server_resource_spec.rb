require_relative 'spec_helper'

describe 'microwave_server' do
  include_context 'release api'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
      node.name('chef.example.com')
      node.automatic['fqdn'] = 'chef.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::server')
  end

  it 'installs the resolved latest package under the asset name with the published digest' do
    expect(chef_run).to create_remote_file('/var/cache/microwave/microwave-server_0.1.0_amd64.deb')
      .with(checksum: '6d1f0c2e9a7b4c3d8e5f6a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f', mode: '0644')
    expect(requested_urls).to include('https://api.github.com/repos/rmoriz/microwave-development/releases/latest')
  end

  it 'keeps the query string of the download URL out of the cache file name' do
    names = Array(chef_run.remote_file('/var/cache/microwave/microwave-server_0.1.0_amd64.deb').source)

    expect(names).to eq(['https://example.invalid/microwave-server_0.1.0_amd64.deb?token=abc'])
    expect(chef_run.resource_collection.map(&:name)).to include('/var/cache/microwave/microwave-server_0.1.0_amd64.deb')
  end

  it 'passes the published digest from the .sha256 asset to dpkg' do
    expect(chef_run).to install_dpkg_package('/var/cache/microwave/microwave-server_0.1.0_amd64.deb')
      .with(options: ['--force-confold'])
  end

  it 'installs the declared package dependency dpkg would not resolve' do
    resources = chef_run.resource_collection.map(&:name)
    update = resources.index('apt-get update')
    package = resources.index('logrotate')
    dpkg = resources.index('/var/cache/microwave/microwave-server_0.1.0_amd64.deb')

    # The index refresh is guard-only, so it does not converge on every run.
    expect(chef_run.execute('apt-get update').not_if.map(&:command))
      .to eq(['apt-cache show logrotate >/dev/null 2>&1'])
    expect(chef_run).to install_apt_package('logrotate')
    expect(update).to be < package
    expect(package).to be < dpkg
  end

  it 'renders the data directory, the URLs and the bind hostname' do
    expect(chef_run).to render_file('/etc/microwave/microwave.toml')
      .with_content(%r{^data_dir = "/var/lib/microwave"$})
      .with_content(%r{^url = "postgres://microwave:.*@db\.example\.com:5432/microwave\?sslmode=require"$})
      .with_content(%r{^url = "https://search\.example\.com:9200"$})
      .with_content(/^bind = "chef\.example\.com:8000"$/)
  end

  it 'writes the configuration 0640 root:microwave, kept out of the run report' do
    expect(chef_run).to create_template('/etc/microwave/microwave.toml')
      .with(owner: 'root', group: 'microwave', mode: '0640')
    expect(chef_run.template('/etc/microwave/microwave.toml').sensitive).to be(true)
  end

  it 'trusts both CAs with consecutive names and refreshes the system store' do
    %w(1 2).each do |index|
      path = "/usr/local/share/ca-certificates/microwave-backend-#{index}.crt"
      expect(chef_run).to create_file(path).with(content: "CA-PEM-#{index}")
      expect(chef_run.file(path)).to notify('execute[update-ca-certificates]').delayed
    end
    expect(chef_run.resource_collection.map(&:name).grep(/microwave-backend-\d+\.crt\z/).size).to eq(2)
    expect(chef_run.execute('update-ca-certificates')).to do_nothing
  end

  it 'enables and starts the service' do
    expect(chef_run).to enable_service('microwave-server')
    expect(chef_run).to start_service('microwave-server')
  end

  it 'orders the unit after the backends, reloads systemd and raises the memlock limit' do
    path = '/etc/systemd/system/microwave-server.service.d/ordering.conf'

    expect(chef_run).to create_template(path).with(owner: 'root', group: 'root', mode: '0644')
    expect(chef_run).to render_file(path)
      .with_content(/^After=postgresql\.service opensearch\.service$/)
      .with_content(/^Wants=postgresql\.service opensearch\.service$/)
      .with_content(/^LimitMEMLOCK=infinity$/)
    expect(chef_run.template(path)).to notify('execute[systemctl daemon-reload server]').immediately
    expect(chef_run.template(path)).to notify('service[microwave-server]').delayed
    expect(chef_run.execute('systemctl daemon-reload server')).to do_nothing
  end

  # The all-in-one layout clones both resources, so the reload name carries the
  # unit and the notification stays unambiguous.
  it 'names its daemon-reload after the unit and guards it on systemd' do
    reloads = chef_run.resource_collection.select { |r| r.name.to_s.start_with?('systemctl daemon-reload') }

    # `systemctl daemon-reload` takes no unit operand, so the bare command stays
    # while the resource name pins the copy.
    expect(reloads.map(&:name)).to eq(['systemctl daemon-reload server'])
    expect(reloads.first.command).to eq('systemctl daemon-reload')
    expect(reloads.first.only_if.first.block).to be_a(Proc)
  end
end

describe 'microwave_server with a concrete version' do
  include_context 'release api'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::server_pinned')
  end

  it 'resolves the tag through the tags endpoint instead of latest' do
    expect(chef_run).to install_dpkg_package('/var/cache/microwave/microwave-server_0.1.0_amd64.deb')
    expect(requested_urls).to include('https://api.github.com/repos/rmoriz/microwave-development/releases/tags/v0.1.0')
    expect(requested_urls).not_to include('https://api.github.com/repos/rmoriz/microwave-development/releases/latest')
  end
end

describe 'microwave_server with a public URL' do
  include_context 'release api'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
      node.name('chef.example.com')
      node.automatic['fqdn'] = 'chef.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::server_public_url')
  end

  it 'renders public_url in the server section' do
    expect(chef_run).to render_file('/etc/microwave/microwave.toml')
      .with_content(/^bind = "chef\.example\.com"$/)
      .with_content(%r{^public_url = "https://microwave\.example\.com"$})
  end
end

describe 'microwave_server with strings that need TOML escaping' do
  include_context 'release api'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::server_escaped')
  end

  it 'escapes backslashes and double quotes' do
    expect(chef_run).to render_file('/etc/microwave/microwave.toml')
      .with_content('bind = "a\"b\\\\c:8000"')
      .with_content('url = "postgres://microwave:gen\"erated@db.example.com:5432/microwave"')
  end

  it 'escapes the control characters a TOML string cannot carry raw' do
    expect(chef_run).to render_file('/etc/microwave/microwave.toml')
      .with_content('public_url = "https://microwave.example.com/a\\tb\\nc\\rd"')
  end
end

describe 'microwave_server with an incomplete release' do
  include_context 'release api'

  let(:release_json) do
    '{"tag_name":"v0.1.0","assets":[' \
      '{"name":"microwave-server_0.1.0_amd64.deb.sha256",' \
      '"browser_download_url":"https://example.invalid/microwave-server_0.1.0_amd64.deb.sha256"}]}'
  end

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::server')
  end

  it 'raises Missing and names the tag and the .deb asset' do
    expect { chef_run }.to raise_error(
      Microwave::Release::Missing,
      /release v0\.1\.0 has no asset microwave-server_0\.1\.0_amd64\.deb\z/
    )
  end
end

describe 'microwave_server switching' do
  include_context 'release api'

  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
      node.name('chef.example.com')
      node.automatic['fqdn'] = 'chef.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::server_disable')
  end

  before do
    # The unit exists on the converged host, so the switch guard lets the action
    # through; the examples below flip it where they care.
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/lib/systemd/system/microwave-server.service')
                                     .and_return(true)
  end

  it 'stops the unit, disables it and drops the marker' do
    expect(chef_run).to stop_service('microwave-server')
    expect(chef_run).to disable_service('microwave-server')
    expect(chef_run).to create_file('/etc/microwave/disabled/microwave-server')
      .with(owner: 'root', group: 'root', mode: '0644')
  end

  it 'does not need database_url or search_url to disable' do
    expect(chef_run.microwave_server('default').database_url).to be_nil
    expect(chef_run.microwave_server('default').search_url).to be_nil
  end

  it 'skips a unit that does not exist' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/lib/systemd/system/microwave-server.service')
                                     .and_return(false)
    guard = chef_run.service('microwave-server').only_if.first.block

    expect(guard.call).to be(false)
  end

  it 'does not enable or start a marked unit on install' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/microwave-server').and_return(true)
    guard = ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
      node.name('chef.example.com')
      node.automatic['fqdn'] = 'chef.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::server').service('microwave-server').not_if.first.block

    expect(guard.call).to be(true)
  end

  it 'raises on install without a database_url' do
    expect do
      ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server'])
                          .converge('microwave-test::server_disable_install')
    end.to raise_error(/database_url/)
  end
end

describe 'microwave_server object store' do
  include_context 'release api'

  let(:chef_run) { object_store_run('server') }
  let(:garage_run) { object_store_run('server_garage') }
  let(:s3_run) { object_store_run('server_s3') }
  let(:broken_run) { object_store_run('server_garage_broken') }

  def object_store_run(recipe, access_key: "ACCESS\n", secret_key: "SECRET\n")
    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/garage.access_key').and_return(access_key)
    allow(::File).to receive(:read).with('/etc/microwave/garage.secret_key').and_return(secret_key)
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
      node.name('chef.example.com')
      node.automatic['fqdn'] = 'chef.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge("microwave-test::#{recipe}")
  end

  it 'keeps the local backend when nothing is set' do
    expect(chef_run).to render_file('/etc/microwave/microwave.toml')
      .with_content(/^\[object_store\]\nbackend = "local"$/)
  end

  # The key files belong to the garage resource and are absent on a local node.
  it 'does not read the key files for the local backend' do
    chef_run
    expect(::File).not_to have_received(:read).with('/etc/microwave/garage.access_key')
    expect(::File).not_to have_received(:read).with('/etc/microwave/garage.secret_key')
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

  # A truncated publish leaves an empty file on the split node; the server would
  # start and fail every S3 call with an opaque 403.
  it 'raises naming a key file that is only whitespace' do
    expect { object_store_run('server_garage', access_key: " \n") }
      .to raise_error(%r{object_store_access_key_file /etc/microwave/garage\.access_key is empty\z})
  end

  it 'escapes quotes and backslashes in a key' do
    expect(object_store_run('server_garage', secret_key: "a\"b\\c\n"))
      .to render_file('/etc/microwave/microwave.toml')
      .with_content('secret_key = "a\"b\\\\c"')
  end

  it 'raises naming a key file it cannot read' do
    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/garage.access_key').and_raise(Errno::ENOENT)
    expect do
      ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_server']) do |node|
        node.name('chef.example.com')
        node.automatic['kernel']['machine'] = 'x86_64'
      end.converge('microwave-test::server_garage')
    end.to raise_error(%r{object_store_access_key_file /etc/microwave/garage\.access_key})
  end
end
