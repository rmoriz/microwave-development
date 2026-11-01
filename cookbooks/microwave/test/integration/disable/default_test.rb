require_relative '../helpers'

# The switch, end to end: `microwave::disable` stops and disables every unit, the
# data outlives it, a later `microwave::default` converge leaves the node down,
# and `microwave::enable` brings the services, the stored cookbook and the status
# endpoint back. The node is registered as kitchen-disable.
#
# InSpec collects a control body before any of its tests run and knows no `it` at
# that level, so every phase's converge and data read sits in an example inside a
# `describe`: examples run in registration order, which is the order the phases
# need.
control 'microwave disable setup' do
  mw = Microwave.new(inspec)

  describe 'the verify prerequisites' do
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
      # Repeatable, so a verify on a node that was not recreated still leaves
      # exactly one row behind.
      result = mw.psql('127.0.0.1', '/etc/microwave/db.password',
                       sql: 'CREATE TABLE IF NOT EXISTS disable_probe (id int); ' \
                            'TRUNCATE disable_probe; INSERT INTO disable_probe VALUES (1)')
      expect(result.exit_status).to(eq(0), "psql said: #{result.stderr.to_s.strip}")
    end
  end
end

control 'microwave disable stops everything' do
  mw = Microwave.new(inspec)

  describe 'the disable converge' do
    it('succeeds') { expect(mw.converge('microwave::disable').exit_status).to eq(0) }
  end

  %w(microwave-server opensearch postgresql garage nginx).each do |unit|
    describe service(unit) do
      it { should_not be_running }
      it { should_not be_enabled }

      describe "the unit file state of #{unit}" do
        it('is disabled') do
          expect(inspec.command("systemctl show -p UnitFileState --value #{unit}").stdout.strip).to eq 'disabled'
        end
      end
    end
  end
end

control 'microwave disable keeps the data' do
  mw = Microwave.new(inspec)

  # Nothing serves while the switch is on, so this control looks at what is on
  # disk; the readable proof (rows, bucket grant, stored cookbook) follows once
  # `microwave::enable` brought the stack back.
  describe 'the stored data' do
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
end

control 'microwave default keeps the switch off' do
  mw = Microwave.new(inspec)

  describe 'the default converge' do
    it('succeeds') { expect(mw.converge('microwave::default').exit_status).to eq(0) }
  end

  %w(microwave-server opensearch postgresql garage nginx).each do |unit|
    describe service(unit) do
      it { should_not be_running }
      it { should_not be_enabled }
    end
  end
end

control 'microwave enable brings everything back' do
  mw = Microwave.new(inspec)

  describe 'the restored node' do
    it('converges') { expect(mw.converge('microwave::enable').exit_status).to eq(0) }

    %w(nginx garage postgresql opensearch microwave-server).each do |unit|
      describe service(unit) do
        it { should be_running }
        it { should be_enabled }
      end
    end

    it 'keeps the database rows' do
      result = mw.psql('127.0.0.1', '/etc/microwave/db.password',
                       sql: 'SELECT count(*) FROM disable_probe')
      expect(result.stdout.strip).to(eq('1'), "psql said: #{result.stderr.to_s.strip}")
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
      expect(status.exit_status).to(eq(0), "garage did not answer: #{status.stderr.to_s.strip}")

      key = mw.read('/etc/microwave/garage.access_key').strip

      info = mw.garage('bucket', 'info', 'chef-cookbooks').stdout
      # The key table pads its permission column, so the flags are followed by
      # spaces rather than the tab of the raw row.
      expect(info).to match(/^RWO /)
      expect(info).to include(key)
      expect(info).to match(/^Objects:\s+[1-9]/)
    end

    it 'serves the uploaded cookbook again' do
      download = mw.knife('cookbook', 'download', 'kitchen_probe', '0.1.0', '--force',
                          '-d', '/tmp/disable-download')
      expect(download.exit_status).to eq 0
      expect(inspec.file('/tmp/disable-download/kitchen_probe-0.1.0/recipes/default.rb')).to exist
    end

    # The server's unit is only just started, so the first request can race the
    # listener; poll bounded instead of probing once.
    it 'answers 200 with every upstream healthy' do
      ready = inspec.command(
        "for i in $(seq 30); do curl -sS -o /dev/null -w '%{http_code}' " \
        "#{mw.base_url}/_status | grep -qx 200 && exit 0; sleep 1; done; exit 1"
      )
      expect(ready.exit_status).to eq(0), "the status endpoint did not come back: #{ready.stderr.to_s.strip}"

      body, code = mw.status
      expect(code).to eq 200
      expect(JSON.parse(body)['upstreams'].values).to all(eq 'pong')
    end
  end
end
