require_relative 'spec_helper'

describe 'microwave_postgresql' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql']) do |node|
      node.name('db.example.com')
      node.automatic['fqdn'] = 'db.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::database')
  end

  let(:password) { 'a' * 32 }

  before do
    stub_command(/SELECT 1 FROM pg_roles/).and_return(nil)
    stub_command(/SELECT 1 FROM pg_database/).and_return(nil)
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return("#{password}\n")
  end

  def rendered(path)
    ChefSpec::Renderer.new(chef_run, chef_run.template(path)).content
  end

  it 'adds the PGDG repository with its key' do
    expect(chef_run).to add_apt_repository('pgdg').with(
      uri: 'https://apt.postgresql.org/pub/repos/apt',
      distribution: 'trixie-pgdg',
      components: %w(main 19),
      key: ['https://www.postgresql.org/media/keys/ACCC4CF8.asc']
    )
  end

  it 'pins postgresql-19 to the 19 series at priority 600' do
    expect(chef_run).to add_apt_preference('postgresql-19')
      .with(pin: 'version 19*', pin_priority: '600')
  end

  it 'installs the pinned server package' do
    expect(chef_run).to install_apt_package('postgresql-19')
  end

  it 'writes a 32 character password once, 0640 root:microwave' do
    expect(chef_run).to create_file_if_missing('/etc/microwave/db.password')
      .with(owner: 'root', group: 'microwave', mode: '0640')
    expect(chef_run.file('/etc/microwave/db.password').content).to match(/\A[0-9a-f]{32}\n\z/)
  end

  it 'never rewrites an existing password file' do
    expect(chef_run.file('/etc/microwave/db.password').action).to eq([:create_if_missing])
  end

  it 'rotates the role password only when the file was just written' do
    expect(chef_run.file('/etc/microwave/db.password'))
      .to notify('execute[microwave-alter-role-password]').immediately
    expect(chef_run.execute('microwave-alter-role-password')).to do_nothing
  end

  it 'issues a CA and a service certificate named for the hostname' do
    expect(chef_run).to run_execute('microwave-generate-ca-key')
    expect(chef_run.execute('microwave-create-ca').command)
      .to include('-x509', '-days 3650', '-subj "/CN=microwave-ca"', '/etc/microwave/tls/ca.key',
                  '/etc/microwave/tls/ca.crt')

    expect(chef_run).to run_execute('microwave-generate-service-key')
    expect(chef_run.execute('microwave-create-service-certificate').command)
      .to include('-subj "/CN=db.example.com"',
                  '-addext "subjectAltName=DNS:db.example.com,IP:127.0.0.1"',
                  '-copy_extensions copy', '-days 825',
                  '/etc/microwave/tls/db.key', '/etc/microwave/tls/db.crt')
  end

  it 'keeps an existing certificate pair with a matching name' do
    stub_command(/openssl verify/).and_return(true)

    expect(chef_run).not_to run_execute('microwave-create-service-certificate')
  end

  it 're-issues the certificate when the name no longer matches' do
    stub_command(/openssl verify/).and_return(nil)

    expect(chef_run).to run_execute('microwave-create-service-certificate')
  end

  it 'checks the name on the existing certificate before keeping it' do
    guard = chef_run.execute('microwave-create-service-certificate').not_if.first.command

    expect(guard).to include('openssl verify', '-CAfile /etc/microwave/tls/ca.crt')
    expect(guard).to include(
      'openssl x509 -in /etc/microwave/tls/db.crt -noout -ext subjectAltName'
    )
    expect(guard).to include('grep -q "DNS:db.example.com"')
  end

  it 'owns the key material root or postgres and keeps it 0640' do
    expect(chef_run).to create_file('/etc/microwave/tls/ca.key')
      .with(owner: 'root', group: 'postgres', mode: '0640')
    expect(chef_run).to create_file('/etc/microwave/tls/db.key')
      .with(owner: 'postgres', group: 'postgres', mode: '0640')
    expect(chef_run).to create_file('/etc/microwave/tls/db.crt')
      .with(owner: 'postgres', group: 'postgres', mode: '0644')
  end

  it 'marks the password and the certificate change sensitive' do
    expect(chef_run.file('/etc/microwave/db.password').sensitive).to be(true)
    expect(chef_run.execute('microwave-create-role').sensitive).to be(true)
    expect(chef_run.execute('microwave-alter-role-password').sensitive).to be(true)
  end

  it 'turns TLS on and points PostgreSQL at the certificate' do
    expect(chef_run).to create_directory('/etc/postgresql/19/main/conf.d')
      .with(owner: 'root', group: 'root', mode: '0755')
    expect(chef_run).to render_file('/etc/postgresql/19/main/conf.d/microwave.conf')
      .with_content(/^ssl = on$/)
      .with_content(%r{^ssl_cert_file = '/etc/microwave/tls/db\.crt'$})
      .with_content(%r{^ssl_key_file = '/etc/microwave/tls/db\.key'$})
      .with_content(/^listen_addresses = '127\.0\.0\.1'$/)
      .with_content(/^port = 5432$/)
      .with_content(/^password_encryption = 'scram-sha-256'$/)
  end

  it 'renders the whole pg_hba from scratch, TLS only on loopback' do
    expect(rendered('/etc/postgresql/19/main/pg_hba.conf')).to eq(<<~HBA)
      local all postgres peer
      local all all peer
      hostssl all all 127.0.0.1/32 scram-sha-256
      hostssl all all ::1/128 scram-sha-256
      replication local all peer
      replication hostssl all all 127.0.0.1/32 scram-sha-256
    HBA
  end

  it 'creates the role guarded by its absence' do
    role = chef_run.execute('microwave-create-role')

    expect(role.command).to eq("psql -c \"CREATE ROLE microwave LOGIN PASSWORD '#{password}'\"")
    expect(role.not_if.first.command)
      .to eq("psql -tAc \"SELECT 1 FROM pg_roles WHERE rolname='microwave'\" | grep -q 1")
    expect(role.user).to eq('postgres')
  end

  it 'creates the database owned by the role, guarded by its absence' do
    database = chef_run.execute('microwave-create-database')

    expect(database.command).to eq('psql -c "CREATE DATABASE microwave OWNER microwave"')
    expect(database.not_if.first.command)
      .to eq("psql -tAc \"SELECT 1 FROM pg_database WHERE datname='microwave'\" | grep -q 1")
    expect(database.user).to eq('postgres')
  end

  it 'starts the service before it touches the role or the database' do
    expect(chef_run).to enable_service('postgresql')
    expect(chef_run).to start_service('postgresql')

    names = chef_run.resource_collection.map(&:to_s)
    expect(names.index('service[postgresql]')).to be < names.index('execute[microwave-create-role]')
    expect(names.index('service[postgresql]')).to be < names.index('execute[microwave-create-database]')
  end

  it 'restarts PostgreSQL when the configuration or a certificate changes' do
    expect(chef_run.template('/etc/postgresql/19/main/pg_hba.conf'))
      .to notify('service[postgresql]').delayed
    expect(chef_run.template('/etc/postgresql/19/main/conf.d/microwave.conf'))
      .to notify('service[postgresql]').delayed
    expect(chef_run.execute('microwave-create-service-certificate'))
      .to notify('service[postgresql]').delayed
  end

  it 'announces the non-secret configuration paths' do
    expect(chef_run.template('/etc/postgresql/19/main/pg_hba.conf').sensitive).to be(false)
    expect(chef_run.template('/etc/postgresql/19/main/conf.d/microwave.conf').sensitive).to be(false)
    expect(chef_run.template('/etc/microwave/tls/backend.cnf').sensitive).to be(false)
  end
end

describe 'microwave_postgresql with a client network' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql']) do |node|
      node.name('db.example.com')
      node.automatic['fqdn'] = 'db.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
      node.normal['microwave']['client_network'] = '10.0.0.0/8'
    end.converge('microwave-test::database')
  end

  before do
    stub_command(/SELECT 1 FROM pg_roles/).and_return(nil)
    stub_command(/SELECT 1 FROM pg_database/).and_return(nil)
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return('a' * 32)
  end

  it 'adds the client network as a TLS-only scram rule' do
    content = ChefSpec::Renderer.new(chef_run, chef_run.template('/etc/postgresql/19/main/pg_hba.conf')).content

    expect(content).to include('hostssl microwave microwave 10.0.0.0/8 scram-sha-256')
    expect(content.lines.last).to eq("hostssl microwave microwave 10.0.0.0/8 scram-sha-256\n")
  end

  it 'opens the listener' do
    expect(chef_run).to render_file('/etc/postgresql/19/main/conf.d/microwave.conf')
      .with_content(/^listen_addresses = '\*'$/)
  end
end
