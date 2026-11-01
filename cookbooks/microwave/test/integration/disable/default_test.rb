require_relative '../helpers'

# The switch, end to end: `microwave::disable` stops and disables every unit, the
# data outlives it, a later `microwave::default` converge leaves the node down,
# and `microwave::enable` brings the services, the stored cookbook and the status
# endpoint back. The node is registered as kitchen-disable.
control 'microwave disable setup' do
  mw = Microwave.new(inspec)

  # psql and knife ride along with the install exactly as in the other suites.
  it('installs the psql client') { expect(mw.install_psql).to be_nil }
  it('installs knife') { expect(mw.install_knife).to be true }
  it('writes the knife config') { expect(mw.write_knife_config.exit_status).to eq 0 }
  it 'uploads the probe cookbook' do
    upload = mw.knife('cookbook', 'upload', 'kitchen_probe', '--force',
                      '--cookbook-path', '/opt/fixtures')
    expect(upload.exit_status).to eq 0
  end
  it 'writes marker data into the database' do
    result = mw.psql('127.0.0.1', '/etc/microwave/db.password',
                     sql: 'CREATE TABLE disable_probe (id int); INSERT INTO disable_probe VALUES (1)')
    expect(result.exit_status).to eq 0, "psql said: #{result.stderr.to_s.strip}"
  end
end

control 'microwave disable stops everything' do
  mw = Microwave.new(inspec)
  result = mw.converge('microwave::disable')

  it('converges') { expect(result.exit_status).to eq(0), (result.stdout.to_s + result.stderr.to_s)[-2000..] }

  %w(microwave-server opensearch postgresql garage nginx).each do |unit|
    describe service(unit) do
      it { should_not be_running }
      it { should_not be_enabled }
    end

    describe "the unit file state of #{unit}" do
      it('is disabled') do
        expect(inspec.command("systemctl show -p UnitFileState --value #{unit}").stdout.strip).to eq 'disabled'
      end
    end
  end
end

control 'microwave disable keeps the data' do
  mw = Microwave.new(inspec)

  # Nothing serves while the switch is on, so this control looks at what is on
  # disk; the readable proof (rows, bucket grant, stored cookbook) follows once
  # `microwave::enable` brought the stack back.
  it 'keeps the store key pair' do
    key = mw.read('/etc/microwave/garage.access_key').strip

    expect(key).to match(/\A[0-9a-f]+\z/)
    expect(inspec.file('/etc/microwave/garage.secret_key')).to exist
  end

  it 'keeps the database cluster' do
    found = inspec.command('find /var/lib/postgresql -name PG_VERSION -size +0 | grep -q .')
    expect(found.exit_status).to eq 0
  end

  it 'keeps the stored objects' do
    expect(inspec.command('test -n "$(ls -A /var/lib/garage/data)"').exit_status).to eq 0
  end
end

control 'microwave default keeps the switch off' do
  mw = Microwave.new(inspec)
  result = mw.converge('microwave::default')

  it('converges') { expect(result.exit_status).to eq(0), (result.stdout.to_s + result.stderr.to_s)[-2000..] }

  %w(microwave-server opensearch postgresql garage nginx).each do |unit|
    describe service(unit) do
      it { should_not be_running }
      it { should_not be_enabled }
    end
  end
end

control 'microwave enable brings everything back' do
  mw = Microwave.new(inspec)
  result = mw.converge('microwave::enable')

  it('converges') { expect(result.exit_status).to eq(0), (result.stdout.to_s + result.stderr.to_s)[-2000..] }

  %w(nginx garage postgresql opensearch microwave-server).each do |unit|
    describe service(unit) do
      it { should be_running }
      it { should be_enabled }
    end
  end

  it 'keeps the database rows' do
    result = mw.psql('127.0.0.1', '/etc/microwave/db.password',
                     sql: 'SELECT count(*) FROM disable_probe')
    expect(result.stdout.strip).to eq '1', "psql said: #{result.stderr.to_s.strip}"
  end

  # The grant and the objects are read back through the admin CLI: a restarted
  # engine that lost its metadata would fail here even with nginx up. The admin
  # API is not up the instant systemd reports the unit started, so the probe
  # waits for it, bounded.
  it 'keeps the bucket grant and its objects' do
    status = inspec.command(
      'for i in $(seq 30); do garage -c /etc/garage.toml status >/dev/null 2>&1 && exit 0; ' \
      'sleep 1; done; exit 1'
    )
    expect(status.exit_status).to eq 0, "garage did not answer: #{status.stderr.to_s.strip}"

    key = mw.read('/etc/microwave/garage.access_key').strip

    info = mw.garage('bucket', 'info', 'chef-cookbooks').stdout
    # The key table pads its permission column, so the flags are followed by
    # spaces rather than the tab of the raw row.
    expect(info).to match(/\ARWO /)
    expect(info).to include(key)
    expect(info).to match(/^Objects:\s+[1-9]/)
  end

  it 'serves the uploaded cookbook again' do
    download = mw.knife('cookbook', 'download', 'kitchen_probe', '0.1.0', '--force',
                        '-d', '/tmp/disable-download')
    expect(download.exit_status).to eq 0
    expect(inspec.file('/tmp/disable-download/kitchen_probe-0.1.0/recipes/default.rb')).to exist
  end

  describe 'the status endpoint' do
    body, code = mw.status

    it('answers 200') { expect(code).to eq 200 }
    it('reports every dependency healthy') { expect(JSON.parse(body)['upstreams'].values).to all(eq 'pong') }
  end
end
