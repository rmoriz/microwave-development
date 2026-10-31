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

  it 'renders the data directory, the URLs and the bind hostname' do
    expect(chef_run).to render_file('/etc/microwave/microwave.toml')
      .with_content(%r{^data_dir = "/var/lib/microwave"$})
      .with_content(%r{^url = "postgres://microwave:.*@db\.example\.com:5432/microwave\?sslmode=require"$})
      .with_content(%r{^url = "https://search\.example\.com:9200"$})
      .with_content(/^bind = "chef\.example\.com:8000"$/)
  end

  it 'writes the configuration 0640 root:microwave' do
    expect(chef_run).to create_template('/etc/microwave/microwave.toml')
      .with(owner: 'root', group: 'microwave', mode: '0640')
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
    expect(chef_run.template(path)).to notify('execute[systemctl daemon-reload]').immediately
    expect(chef_run.template(path)).to notify('service[microwave-server]').delayed
    expect(chef_run.execute('systemctl daemon-reload')).to do_nothing
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
