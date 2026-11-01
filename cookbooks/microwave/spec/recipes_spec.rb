require_relative 'spec_helper'

# The database resource writes the password and the CA at converge time, the
# server recipe reads them lazily. The suite fakes them instead of waiting for
# a real converge; everything else falls through to the real file system.
def stub_backend_material(password: 's3cret', ca: 'CA-PEM')
  allow(::File).to receive(:read).and_call_original
  allow(::File).to receive(:exist?).and_call_original
  allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return("#{password}\n")
  allow(::File).to receive(:read).with(Microwave::TLS.ca_cert).and_return(ca)
  allow(::File).to receive(:exist?).with(Microwave::TLS.ca_cert).and_return(true)
end

def runner(name: 'chef.example.com', &block)
  ChefSpec::SoloRunner.new(platform: 'debian', version: '13') do |node|
    node.name(name)
    node.automatic['fqdn'] = name
    block&.call(node)
  end
end

describe 'microwave::default' do
  before { stub_backend_material }

  let(:chef_run) { runner.converge('microwave-test::all_in_one') }

  it 'converges the database, the search engine and the server in that order' do
    expect(chef_run.resource_collection.map { |r| r.resource_name.to_s })
      .to eq(%w(microwave_postgresql microwave_opensearch microwave_server directory))
  end

  it 'points the server at the loopback backends over TLS' do
    server = chef_run.microwave_server('default')

    expect(server.database_url)
      .to eq('postgres://microwave:s3cret@127.0.0.1:5432/microwave?sslmode=require')
    expect(server.search_url).to eq('https://127.0.0.1:9200')
    expect(server.bind).to eq('192.0.2.10:8000')
    expect(server.ca_certs).to eq(['CA-PEM'])
  end

  it 'trusts no CA when the CA file does not exist yet' do
    allow(::File).to receive(:exist?).with(Microwave::TLS.ca_cert).and_return(false)

    expect(chef_run.microwave_server('default').ca_certs).to eq([])
  end

  it 'leaves public_url unset by default' do
    expect(chef_run.microwave_server('default').public_url).to be_nil
  end
end

describe 'microwave::default with backend material from attributes' do
  before { stub_backend_material }

  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['database_password'] = 'from-attribute'
      node.normal['microwave']['ca_certs'] = ['ATTRIBUTE-CA']
      node.normal['microwave']['public_url'] = 'https://microwave.example.com'
    end.converge('microwave-test::all_in_one')
  end

  it 'prefers the attributes over the local files' do
    server = chef_run.microwave_server('default')

    expect(server.database_url).to include(':from-attribute@127.0.0.1:5432/')
    expect(server.ca_certs).to eq(['ATTRIBUTE-CA'])
    expect(server.public_url).to eq('https://microwave.example.com')
  end
end

describe 'microwave::default under a wrapper with its own backend hosts' do
  before { stub_backend_material }

  let(:chef_run) { runner.converge('microwave-test::all_in_one_wrapper') }

  it 'keeps the wrapper node.default hosts instead of the loopback fallback' do
    server = chef_run.microwave_server('default')

    expect(server.database_url).to include('@db.wrapper.example.com:5432/')
    expect(server.search_url).to eq('https://search.wrapper.example.com:9200')
  end
end

describe 'microwave::default with create_organization' do
  before do
    stub_backend_material
    stub_command('chef-server-ctl --config /etc/microwave/microwave.toml org-list | grep -qx demo')
      .and_return(false)
  end

  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['create_organization'] = true
    end.converge('microwave-test::all_in_one')
  end

  let(:ctl) { 'chef-server-ctl --config /etc/microwave/microwave.toml' }
  let(:status_url) { 'http://192.0.2.10:8000/_status' }

  it 'creates the admin only when it is missing, then the organization and the membership' do
    command = chef_run.execute('microwave_organization').command

    expect(command.split("\n").map(&:strip)).to eq(
      [
        'for i in $(seq 1 20); do curl -fsS --max-time 5 http://192.0.2.10:8000/_status && break; ' \
        '[ "$i" -lt 20 ] && sleep 3; done &&',
        "{ #{ctl} user-list | grep -qx pivotal || " \
        "#{ctl} user-create pivotal --email pivotal@example.com --admin -f /var/lib/microwave/pivotal.pem ; } &&",
        "#{ctl} org-create demo --full-name 'Microwave Demo' --association-user pivotal -f /tmp/demo-validator.pem &&",
        "#{ctl} org-user-add demo pivotal",
      ]
    )
  end

  it 'waits for the status endpoint inside the command, bounded and on the bind address' do
    command = chef_run.execute('microwave_organization').command

    expect(command).to include("for i in $(seq 1 20); do curl -fsS --max-time 5 #{status_url} && break")
    expect(command).to include('[ "$i" -lt 20 ] && sleep 3; done &&')
    expect(command).to include(status_url)
    expect(command).to start_with('for i in $(seq 1 20); do curl -fsS --max-time 5 http://192.0.2.10:8000/_status')
  end

  it 'follows a non-default bind in the status URL' do
    run = runner do |node|
      node.normal['microwave']['bind'] = 'other.example.com'
      node.normal['microwave']['create_organization'] = true
    end.converge('microwave-test::all_in_one')

    expect(run.execute('microwave_organization').command)
      .to include('curl -fsS --max-time 5 http://192.0.2.11:8000/_status')
  end

  it 'installs curl before the execute, because the package does not ship it' do
    resources = chef_run.resource_collection.to_a
    package = resources.index { |resource| resource.name == 'curl' }
    execute = resources.index { |resource| resource.name == 'microwave_organization' }

    expect(chef_run).to install_apt_package('curl')
    expect(package).to be < execute
  end

  it 'converges only once the organization is absent' do
    expect(chef_run.execute('microwave_organization').not_if.map(&:command))
      .to eq(["#{ctl} org-list | grep -qx demo"])
  end

  it 'keeps the missing organization as the only guard' do
    organization = chef_run.execute('microwave_organization')

    expect(organization.only_if).to be_empty
    expect(organization.not_if.size).to eq(1)
  end

  it 'retries the command itself, with retries behind it' do
    organization = chef_run.execute('microwave_organization')

    expect(organization.retries).to eq(3)
    expect(organization.retry_delay).to eq(3)
  end

  it 'runs after the server resource' do
    resources = chef_run.resource_collection.to_a
    execute = resources.index { |resource| resource.name == 'microwave_organization' }
    server = resources.index { |resource| resource.resource_name.to_s == 'microwave_server' }

    expect(execute).to eq(resources.size - 1)
    expect(execute).to be > server
  end
end

describe 'microwave::default with create_organization when only demo2 exists' do
  # the fake ctl answers org-list with whatever lives in ORG_LIST, so the guard
  # can be exercised against both directions
  def fake_ctl(dir)
    ctl = ::File.join(dir, 'chef-server-ctl')
    ::File.write(ctl, "#!/bin/sh\ncase \"$*\" in *org-list*) echo \"$ORG_LIST\" ;; esac\n")
    ::File.chmod(0o755, ctl)
    ctl
  end

  around do |example|
    Dir.mktmpdir do |dir|
      fake_ctl(dir)
      path = ENV.fetch('PATH')
      ENV['PATH'] = "#{dir}:#{path}"
      begin
        example.run
      ensure
        ENV['PATH'] = path
      end
    end
  end

  before do
    stub_backend_material
    stub_command("#{ctl} org-list | grep -qx demo").and_return(false)
  end

  let(:ctl) { 'chef-server-ctl --config /etc/microwave/microwave.toml' }

  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['create_organization'] = true
    end.converge('microwave-test::all_in_one')
  end

  it 'does not mistake demo2 for the demo organization' do
    guard = chef_run.execute('microwave_organization').not_if.first.command

    # run the real guard against an org list that only carries demo2
    `ORG_LIST=demo2 #{guard} > /dev/null 2>&1`

    expect($CHILD_STATUS).not_to be_success
    expect(chef_run).to run_execute('microwave_organization')
  end

  it 'skips the execute when the org list carries demo' do
    guard = chef_run.execute('microwave_organization').not_if.first.command

    # positive control: the same guard must now succeed, which makes not_if skip
    `ORG_LIST=demo #{guard} > /dev/null 2>&1`

    expect($CHILD_STATUS).to be_success
  end
end

describe 'microwave::default without create_organization' do
  before { stub_backend_material }

  let(:chef_run) { runner.converge('microwave-test::all_in_one') }

  it 'declares no organization execute' do
    expect(chef_run.execute('microwave_organization')).to be_nil
  end
end

describe 'microwave::server' do
  before { stub_backend_material }

  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
      node.normal['microwave']['database_port'] = 6432
      node.normal['microwave']['search_host'] = 'search.example.com'
    end.converge('microwave-test::server_recipe')
  end

  it 'declares the server, its config directory and nothing else' do
    expect(chef_run.resource_collection.map { |r| r.resource_name.to_s })
      .to eq(%w(microwave_server directory))
  end

  it 'widens /etc/microwave so the backend users can read the TLS material' do
    expect(chef_run).to create_directory('/etc/microwave')
      .with(owner: 'root', group: 'root', mode: '0755')
  end

  it 'builds the URLs from the host and port attributes' do
    server = chef_run.microwave_server('default')

    expect(server.database_url)
      .to eq('postgres://microwave:s3cret@db.example.com:6432/microwave?sslmode=require')
    expect(server.search_url).to eq('https://search.example.com:9200')
  end
end

describe 'microwave::server listener address' do
  before do
    stub_backend_material
    allow(Resolv).to receive(:getaddress).and_raise(SocketError)
  end

  let(:bind) do
    runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
      node.normal['microwave']['search_host'] = 'search.example.com'
      node.automatic['ipaddress'] = '198.51.100.7'
    end.converge('microwave-test::server_recipe').microwave_server('default').bind
  end

  it 'falls back to the primary address when the client-facing name does not resolve' do
    expect(bind).to eq('198.51.100.7:8000')
  end

  it 'brackets an IPv6 address, which check-config would otherwise misread' do
    allow(Resolv).to receive(:getaddress).and_return('2001:db8::5')

    expect(bind).to eq('[2001:db8::5]:8000')
  end

  # Stock Debian maps the hostname to 127.0.1.1, so the default bind would
  # otherwise leave the served and presigned URLs loopback-only.
  it 'prefers the primary address when the name resolves to loopback' do
    allow(Resolv).to receive(:getaddress).and_return('127.0.1.1')

    expect(bind).to eq('198.51.100.7:8000')
  end

  it 'prefers the primary address for an IPv6 loopback result as well' do
    allow(Resolv).to receive(:getaddress).and_return('::1')

    expect(bind).to eq('198.51.100.7:8000')
  end

  it 'keeps a resolved address that is reachable' do
    allow(Resolv).to receive(:getaddress).and_return('203.0.113.9')

    expect(bind).to eq('203.0.113.9:8000')
  end

  # A literal loopback bind is deliberate, for example in front of a local TLS
  # proxy, so it must not be swapped for the primary address.
  it 'keeps a literal 127.0.0.1 bind verbatim' do
    run = runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
      node.normal['microwave']['search_host'] = 'search.example.com'
      node.normal['microwave']['bind'] = '127.0.0.1'
      node.automatic['ipaddress'] = '198.51.100.7'
    end.converge('microwave-test::server_recipe')

    expect(Resolv).not_to receive(:getaddress)
    expect(run.microwave_server('default').bind).to eq('127.0.0.1:8000')
  end

  it 'keeps a literal ::1 bind verbatim and brackets it' do
    run = runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
      node.normal['microwave']['search_host'] = 'search.example.com'
      node.normal['microwave']['bind'] = '::1'
      node.automatic['ipaddress'] = '198.51.100.7'
    end.converge('microwave-test::server_recipe')

    expect(run.microwave_server('default').bind).to eq('[::1]:8000')
  end

  it 'keeps a literal wildcard bind verbatim' do
    run = runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
      node.normal['microwave']['search_host'] = 'search.example.com'
      node.normal['microwave']['bind'] = '0.0.0.0'
      node.automatic['ipaddress'] = '198.51.100.7'
    end.converge('microwave-test::server_recipe')

    expect(run.microwave_server('default').bind).to eq('0.0.0.0:8000')
  end

  it 'falls back to all interfaces when neither the name nor a primary address resolves' do
    allow(Resolv).to receive(:getaddress).and_return('127.0.1.1')
    run = runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
      node.normal['microwave']['search_host'] = 'search.example.com'
      node.automatic['ipaddress'] = ''
    end.converge('microwave-test::server_recipe')

    expect(run.microwave_server('default').bind).to eq('0.0.0.0:8000')
  end
end

describe 'microwave::server with backend material from files' do
  before do
    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:read).with('/mnt/backend/db.password').and_return("remote-secret\n")
    allow(::File).to receive(:read).with('/mnt/backend/ca.crt').and_return('REMOTE-CA')
    allow(::File).to receive(:exist?).with('/mnt/backend/ca.crt').and_return(true)
    allow(::File).to receive(:exist?).with(Microwave::TLS.ca_cert).and_return(false)
  end

  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
      node.normal['microwave']['search_host'] = 'search.example.com'
      node.normal['microwave']['database_password_file'] = '/mnt/backend/db.password'
      node.normal['microwave']['ca_cert_file'] = '/mnt/backend/ca.crt'
    end.converge('microwave-test::server_recipe')
  end

  it 'reads the password from the configured password file' do
    expect(chef_run.microwave_server('default').database_url)
      .to eq('postgres://microwave:remote-secret@db.example.com:5432/microwave?sslmode=require')
  end

  it 'trusts the CA from the configured CA file instead of the local one' do
    expect(chef_run.microwave_server('default').ca_certs).to eq(['REMOTE-CA'])
  end
end

describe 'microwave::server with a CA file that is not there yet' do
  before do
    stub_backend_material
    allow(::File).to receive(:exist?).with('/mnt/backend/ca.crt').and_return(false)
  end

  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
      node.normal['microwave']['search_host'] = 'search.example.com'
      node.normal['microwave']['ca_cert_file'] = '/mnt/backend/ca.crt'
    end.converge('microwave-test::server_recipe')
  end

  it 'falls back to the local CA' do
    expect(chef_run.microwave_server('default').ca_certs).to eq(['CA-PEM'])
  end
end

describe 'microwave::server without a database host' do
  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['search_host'] = 'search.example.com'
    end.converge('microwave-test::server_recipe')
  end

  it 'raises and names the attribute' do
    expect { chef_run }.to raise_error(/node\['microwave'\]\['database_host'\]/)
  end
end

describe 'microwave::server without a password anywhere' do
  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
      node.normal['microwave']['search_host'] = 'search.example.com'
    end.converge('microwave-test::server_recipe')
  end

  it 'raises naming the attribute instead of leaking Errno::ENOENT' do
    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password')
                                   .and_raise(Errno::ENOENT, '/etc/microwave/db.password')

    expect { chef_run.microwave_server('default').database_url }
      .to raise_error(RuntimeError, /node\['microwave'\]\['database_password'\]/)
  end
end

describe 'microwave::server without a search host' do
  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['database_host'] = 'db.example.com'
    end.converge('microwave-test::server_recipe')
  end

  it 'raises and names the attribute' do
    expect { chef_run }.to raise_error(/node\['microwave'\]\['search_host'\]/)
  end
end

describe 'microwave::database' do
  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['database_name'] = 'appdb'
      node.normal['microwave']['database_user'] = 'appuser'
      node.normal['microwave']['client_network'] = '10.0.0.0/24'
    end.converge('microwave-test::database_recipe')
  end

  it 'declares the database resource with the attribute values' do
    expect(chef_run.resource_collection.map { |r| r.resource_name.to_s }).to eq(['microwave_postgresql'])

    postgresql = chef_run.microwave_postgresql('default')
    expect(postgresql.database).to eq('appdb')
    expect(postgresql.user).to eq('appuser')
    expect(postgresql.client_network).to eq('10.0.0.0/24')
  end
end

describe 'microwave::search' do
  let(:chef_run) do
    runner do |node|
      node.normal['microwave']['opensearch_heap'] = '2g'
      node.normal['microwave']['client_network'] = '10.0.0.0/24'
    end.converge('microwave-test::search_recipe')
  end

  it 'declares the search resource with the attribute values' do
    expect(chef_run.resource_collection.map { |r| r.resource_name.to_s }).to eq(['microwave_opensearch'])

    search = chef_run.microwave_opensearch('default')
    expect(search.heap).to eq('2g')
    expect(search.client_network).to eq('10.0.0.0/24')
  end
end
