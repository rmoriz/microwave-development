require_relative '../helpers'

# The single-node layout: both backends and the server on one machine. The
# node is registered as kitchen-all-in-one, so the knife assertions use that
# name.
control 'microwave all-in-one services' do
  mw = Microwave.new(inspec)

  describe service('postgresql') do
    it { should be_running }
    it { should be_enabled }
  end

  describe service('opensearch') do
    it { should be_running }
    it { should be_enabled }
  end

  describe service('microwave-server') do
    it { should be_running }
    it { should be_enabled }
  end

  describe 'the status endpoint' do
    body, code = mw.status

    it('answers 200') { expect(code).to eq 200 }
    it('reports every dependency healthy') do
      # The upstream map is checked as a whole: the released package names the
      # search upstream chef_opensearch, the workspace tree chef_index.
      report = JSON.parse(body)
      search_key = (report['upstreams'].keys & %w(chef_index chef_opensearch)).first
      expect(report['status']).to eq 'pong'
      expect(report['upstreams'].keys).to contain_exactly('chef_sql', 'bookshelf', search_key)
      expect(report['upstreams'].values).to all(eq 'pong')
    end
  end
end

control 'microwave all-in-one wiring' do
  mw = Microwave.new(inspec)
  mw.install_psql

  describe file('/etc/microwave/microwave.toml') do
    it { should exist }
    its('mode') { should cmp 0o640 }
    its('owner') { should eq 'root' }
    its('group') { should eq 'microwave' }
    its('content') { should include 'sslmode=require' }
    its('content') { should include 'https://127.0.0.1:9200' }
  end

  describe 'postgres over TLS' do
    it 'accepts an sslmode=require connection' do
      result = mw.psql('127.0.0.1', '/etc/microwave/db.password')

      expect(result.stdout.strip).to eq('1'), "psql said: #{result.stderr.to_s.strip}"
    end
  end

  describe 'opensearch over TLS' do
    health = mw.opensearch_health('127.0.0.1', '/etc/microwave/tls/ca.crt')

    it('is green or yellow') { expect(health).to match(/"status":"(green|yellow)"/) }
  end
end

control 'microwave all-in-one organizations and clients' do
  mw = Microwave.new(inspec)

  describe 'the shipped organization' do
    it('is listed') { expect(mw.ctl('org-list').stdout).to include('demo') }
    it('has a validator key') { expect(inspec.file('/tmp/demo-validator.pem')).to exist }
  end

  describe 'org-create' do
    it 'creates a second organization with a validator key' do
      # A fresh name keeps the control repeatable on a node that was not
      # recreated between two verify runs.
      name = "kitchen-#{Time.now.to_i}"
      result = mw.ctl('org-create', name, '--association-user', 'pivotal',
                      '-f', '/tmp/kitchen-validator.pem')
      expect(result.exit_status).to eq 0
      expect(result.stdout).to include("Created organization #{name}")
      expect(mw.ctl('org-show', name).exit_status).to eq 0
      expect(inspec.file('/tmp/kitchen-validator.pem')).to exist
    end
  end

  describe 'a registered node' do
    it 'uploads the probe cookbook and converges it' do
      expect(mw.install_knife).to be true
      expect(mw.write_knife_config.exit_status).to eq 0

      upload = mw.knife('cookbook', 'upload', 'kitchen_probe', '--force',
                        '--cookbook-path', '/opt/fixtures')
      expect(upload.exit_status).to eq 0

      result = mw.probe
      said = (result.stdout.to_s + result.stderr.to_s).strip[0, 2000]
      expect(result.exit_status).to eq(0), "cinc-client said: #{said}"
      expect(mw.knife('node', 'show', 'probe').stdout).to include('kitchen_probe')
    end
  end
end
