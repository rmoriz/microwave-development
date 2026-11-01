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
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(true)

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return("#{password}\n")
  end

  def rendered(path)
    ChefSpec::Renderer.new(chef_run, chef_run.template(path)).content
  end

  it 'adds the PGDG repository signed by the shipped keyring' do
    expect(chef_run).to create_cookbook_file('/etc/apt/keyrings/pgdg.asc')
      .with(source: 'pgdg.asc', cookbook: 'microwave', mode: '0644')
    expect(chef_run).to add_apt_repository('pgdg').with(
      uri: 'https://apt.postgresql.org/pub/repos/apt',
      distribution: 'trixie-pgdg',
      components: %w(main 19),
      signed_by: '/etc/apt/keyrings/pgdg.asc'
    )
  end

  it 'pins postgresql-19 to the 19 series at priority 600' do
    expect(chef_run).to add_apt_preference('postgresql-19')
      .with(pin: 'version 19*', pin_priority: '600')
  end

  it 'installs the pinned server package' do
    expect(chef_run).to install_apt_package('postgresql-19')
  end

  it 'writes a 32 character password, 0640 root:microwave' do
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(false)

    expect(chef_run).to create_file('/etc/microwave/db.password')
      .with(owner: 'root', group: 'microwave', mode: '0640')
    expect(chef_run.file('/etc/microwave/db.password').content).to match(/\A[0-9a-f]{32}\n\z/)
  end

  it 'leaves a password file that has content alone' do
    expect(chef_run.file('/etc/microwave/db.password').action).to eq([:create])
    expect(chef_run.file('/etc/microwave/db.password').not_if.map(&:command))
      .to eq(['grep -qs "[^[:space:]]" /etc/microwave/db.password'])
  end

  it 'rewrites an existing but empty file, which would set an empty role password' do
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(false)
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return('')

    expect(chef_run).to create_file('/etc/microwave/db.password')
  end

  it 'rewrites a whitespace-only file, which the server recipe would strip to nothing' do
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(false)
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return(" \t\n")

    expect(chef_run).to create_file('/etc/microwave/db.password')
  end

  it 'leaves a file whose content survives a strip alone' do
    guard = chef_run.file('/etc/microwave/db.password').not_if.first.command

    Dir.mktmpdir do |dir|
      path = ::File.join(dir, 'db.password')
      ::File.write(path, "s3cret\n")

      expect(system(guard.sub('/etc/microwave/db.password', path))).to be(true)
    end
  end

  it 'runs the real guard on a whitespace-only file and finds it unwritten' do
    guard = chef_run.file('/etc/microwave/db.password').not_if.first.command

    Dir.mktmpdir do |dir|
      path = ::File.join(dir, 'db.password')
      ::File.write(path, " \t\n")

      expect(system(guard.sub('/etc/microwave/db.password', path))).to be(false)
    end
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
                  '-copy_extensions copy', '-extfile /etc/microwave/tls/backend.cnf',
                  '-extensions ext', '-days 825',
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
    expect(guard).to include('grep -qE "DNS:db\.example\.com(,|$)"')
  end

  it 're-issues the certificate when the key no longer exists' do
    guard = chef_run.execute('microwave-create-service-certificate').not_if.first.command

    expect(guard).to start_with('test -f /etc/microwave/tls/db.key && test -f /etc/microwave/tls/db.crt')
  end

  it 're-issues the certificate when the key does not belong to it' do
    guard = chef_run.execute('microwave-create-service-certificate').not_if.first.command

    expect(guard).to include(
      '[ "$(openssl x509 -in /etc/microwave/tls/db.crt -pubkey -noout)" = ' \
      '"$(openssl pkey -in /etc/microwave/tls/db.key -pubout)" ]'
    )
  end

  it 're-issues the CA when the certificate and the key belong to different pairs' do
    guard = chef_run.execute('microwave-create-ca').not_if.first.command

    expect(guard).to start_with('test -f /etc/microwave/tls/ca.key && test -f /etc/microwave/tls/ca.crt')
    expect(guard).to include(
      '[ "$(openssl x509 -in /etc/microwave/tls/ca.crt -pubkey -noout)" = ' \
      '"$(openssl pkey -in /etc/microwave/tls/ca.key -pubout)" ]'
    )
  end

  it 'owns the key material root or postgres and keeps it tight' do
    # The CA is shared with the search resource, so it stays with root.
    expect(chef_run).to create_file('/etc/microwave/tls/ca.key')
      .with(owner: 'root', group: 'root', mode: '0640')
    # postgres prefers 0600 for a key the database user owns.
    expect(chef_run).to create_file('/etc/microwave/tls/db.key')
      .with(owner: 'postgres', group: 'postgres', mode: '0600')
    expect(chef_run).to create_file('/etc/microwave/tls/db.crt')
      .with(owner: 'postgres', group: 'postgres', mode: '0640')
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
      .with_content(%r{^ssl_ca_file = '/etc/microwave/tls/ca\.crt'$})
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
      local replication all peer
      hostssl replication all 127.0.0.1/32 scram-sha-256
      hostssl replication all ::1/128 scram-sha-256
    HBA
  end

  it 'creates the role guarded by its absence' do
    role = chef_run.execute('microwave-create-role')

    expect(role.command).to eq("psql -p 5432 -c \"CREATE ROLE \\\"microwave\\\" LOGIN PASSWORD '#{password}'\"")
    expect(role.not_if.first.command)
      .to eq("psql -p 5432 -tAc \"SELECT 1 FROM pg_roles WHERE rolname='microwave'\" | grep -q 1")
    expect(role.user).to eq('postgres')
  end

  it 'grants the role no cluster privileges beyond login' do
    command = chef_run.execute('microwave-create-role').command

    expect(command).to include('LOGIN', 'PASSWORD')
    expect(command).not_to include('SUPERUSER', 'CREATEDB', 'CREATEROLE')
  end

  it 'creates the database owned by the role, guarded by its absence' do
    database = chef_run.execute('microwave-create-database')

    expect(database.command).to eq('psql -p 5432 -c "CREATE DATABASE \\"microwave\\" OWNER \\"microwave\\""')
    expect(database.not_if.first.command)
      .to eq("psql -p 5432 -tAc \"SELECT 1 FROM pg_database WHERE datname='microwave'\" | grep -q 1")
    expect(database.user).to eq('postgres')
  end

  it 'rotates through ALTER ROLE, guarded by the role and failing loudly' do
    alter = chef_run.execute('microwave-alter-role-password')

    expect(alter.command).to eq("psql -p 5432 -c \"ALTER ROLE \\\"microwave\\\" LOGIN PASSWORD '#{password}'\"")
    expect(alter.only_if.last.command)
      .to eq("psql -p 5432 -tAc \"SELECT 1 FROM pg_roles WHERE rolname='microwave'\" | grep -q 1")
    expect(alter.ignore_failure).to be(false)
  end

  it 'restarts the server after a rotation so it picks up the new password' do
    expect(chef_run.file('/etc/microwave/db.password'))
      .to notify('service[microwave-server]').delayed
    expect(chef_run.service('microwave-server').action).to eq([:nothing])
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
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(true)

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

describe 'microwave_postgresql on a custom port' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql']) do |node|
      node.name('db.example.com')
      node.automatic['fqdn'] = 'db.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
      node.normal['microwave']['database_port'] = 5433
    end.converge('microwave-test::database')
  end

  before do
    stub_command(/SELECT 1 FROM pg_roles/).and_return(nil)
    stub_command(/SELECT 1 FROM pg_database/).and_return(nil)
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(true)

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return('a' * 32)
  end

  it 'routes every psql call through the port' do
    expect(chef_run.execute('microwave-create-role').command).to start_with('psql -p 5433 -c')
    expect(chef_run.execute('microwave-create-role').not_if.first.command).to start_with('psql -p 5433 -tAc')
    expect(chef_run.execute('microwave-alter-role-password').command).to start_with('psql -p 5433 -c')
    expect(chef_run.execute('microwave-alter-role-password').only_if.last.command).to start_with('psql -p 5433 -tAc')
    expect(chef_run.execute('microwave-create-database').command).to start_with('psql -p 5433 -c')
    expect(chef_run.execute('microwave-create-database').not_if.first.command).to start_with('psql -p 5433 -tAc')
  end

  it 'listens on the port' do
    expect(chef_run).to render_file('/etc/postgresql/19/main/conf.d/microwave.conf')
      .with_content(/^port = 5433$/)
  end
end

describe 'microwave_postgresql without an FQDN' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql']) do |node|
      node.automatic['fqdn'] = ''
      node.automatic['hostname'] = 'db-short'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::database')
  end

  before do
    stub_command(/SELECT 1 FROM pg_roles/).and_return(nil)
    stub_command(/SELECT 1 FROM pg_database/).and_return(nil)
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(true)

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return('a' * 32)
  end

  it 'names the certificate after the hostname' do
    expect(chef_run.execute('microwave-create-service-certificate').command)
      .to include('-subj "/CN=db-short"', 'DNS:db-short,IP:127.0.0.1')
  end
end

describe 'microwave_postgresql with client CAs' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql']) do |node|
      node.name('db.example.com')
      node.automatic['fqdn'] = 'db.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::database_ca_certs')
  end

  before do
    stub_command(/SELECT 1 FROM pg_roles/).and_return(nil)
    stub_command(/SELECT 1 FROM pg_database/).and_return(nil)
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(true)

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return('a' * 32)
  end

  it 'writes the client CA bundle 0640 root:postgres' do
    expect(chef_run).to create_file('/etc/microwave/tls/client-ca.crt')
      .with(owner: 'root', group: 'postgres', mode: '0640', content: "CA-PEM\n")
  end

  it 'verifies clients against the bundle' do
    expect(chef_run).to render_file('/etc/postgresql/19/main/conf.d/microwave.conf')
      .with_content(%r{^ssl_ca_file = '/etc/microwave/tls/client-ca\.crt'$})
  end
end

describe 'microwave_postgresql switching' do
  let(:chef_run) do
    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql'])
                        .converge('microwave-test::database_disable')
  end

  before do
    stub_command(/SELECT 1 FROM pg_roles/).and_return(nil)
    stub_command(/SELECT 1 FROM pg_database/).and_return(nil)
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(true)

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return("#{'a' * 32}\n")

    # The unit exists on the converged host, so the switch guard lets the action
    # through; the examples below flip it where they care.
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/lib/systemd/system/postgresql.service').and_return(true)
  end

  it 'stops the unit, disables it and drops the marker' do
    expect(chef_run).to stop_service('postgresql')
    expect(chef_run).to disable_service('postgresql')
    expect(chef_run).to create_file('/etc/microwave/disabled/postgresql')
      .with(owner: 'root', group: 'root', mode: '0644')
  end

  it 'skips a unit that does not exist' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/lib/systemd/system/postgresql.service').and_return(false)
    guard = chef_run.service('postgresql').only_if.first.block

    expect(guard.call).to be(false)
  end

  it 'does not enable or start a marked unit on install' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/postgresql').and_return(true)
    guard = ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql']) do |node|
      node.name('db.example.com')
      node.automatic['fqdn'] = 'db.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::database').service('postgresql').not_if.first.block

    expect(guard.call).to be(true)
  end
end

describe 'microwave_postgresql switching on a stopped cluster' do
  before do
    stub_command(/SELECT 1 FROM pg_roles/).and_return(nil)
    stub_command(/SELECT 1 FROM pg_database/).and_return(nil)
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(true)

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return("#{'a' * 32}\n")
    allow(::File).to receive(:exist?).and_call_original
  end

  let(:chef_run) do
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/postgresql').and_return(true)
    allow(::File).to receive(:exist?).with('/lib/systemd/system/postgresql.service').and_return(true)

    ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql']) do |node|
      node.name('db.example.com')
      node.automatic['fqdn'] = 'db.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::database')
  end

  it 'does not run the role or database work against a stopped cluster' do
    %w(microwave-create-role microwave-alter-role-password microwave-create-database).each do |name|
      expect(chef_run.execute(name).only_if.first.block).to be_kind_of(Proc)
      expect(chef_run).not_to run_execute(name)
    end
  end
end

describe 'microwave_postgresql with a disabled server' do
  before do
    stub_command(/SELECT 1 FROM pg_roles/).and_return(nil)
    stub_command(/SELECT 1 FROM pg_database/).and_return(nil)
    stub_command(/openssl verify/).and_return(nil)
    stub_command(/openssl x509 -in/).and_return(nil)
    stub_command('grep -qs "[^[:space:]]" /etc/microwave/db.password').and_return(true)

    allow(::File).to receive(:read).and_call_original
    allow(::File).to receive(:read).with('/etc/microwave/db.password').and_return("#{'a' * 32}\n")
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/microwave-server').and_return(true)
  end

  it 'does not restart the marked server from the database resource' do
    guard = ChefSpec::SoloRunner.new(platform: 'debian', version: '13', step_into: ['microwave_postgresql']) do |node|
      node.name('db.example.com')
      node.automatic['fqdn'] = 'db.example.com'
      node.automatic['kernel']['machine'] = 'x86_64'
    end.converge('microwave-test::database').service('microwave-server').not_if.first.block

    expect(guard.call).to be(true)
  end
end
