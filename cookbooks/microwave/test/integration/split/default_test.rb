require_relative '../helpers'

# The split layout: only the server runs on this node, the database and the
# search engine are the two containers of the fixture stack. The node is
# registered as kitchen-split, so the knife assertions use that name.
control 'microwave split services' do
  mw = Microwave.new(inspec)

  describe service('microwave-server') do
    it { should be_running }
    it { should be_enabled }
  end

  # The backends are remote containers; the server node carries neither.
  describe service('postgresql') do
    it { should_not be_running }
  end

  describe service('opensearch') do
    it { should_not be_running }
  end

  describe 'the status endpoint' do
    body, code = mw.status

    it('answers 200') { expect(code).to eq 200 }
    it('reports both remote backends healthy') do
      # See the all-in-one suite: the search upstream key depends on the build.
      report = JSON.parse(body)
      search_key = (report['upstreams'].keys & %w(chef_index chef_opensearch)).first
      expect(report['status']).to eq 'pong'
      expect(report['upstreams'].keys).to contain_exactly('chef_sql', 'bookshelf', search_key)
      expect(report['upstreams'].values).to all(eq 'pong')
    end
  end
end

control 'microwave split wiring' do
  mw = Microwave.new(inspec)
  mw.install_psql

  describe file('/etc/microwave/microwave.toml') do
    it { should exist }
    its('mode') { should cmp 0o640 }
    its('owner') { should eq 'root' }
    its('group') { should eq 'microwave' }
    its('content') { should include '@db.microwave.test:5432/microwave?sslmode=require' }
    its('content') { should include 'https://search.microwave.test:9200' }
  end

  # The split password and the CA bundle came in over the fixture mount. The
  # mount copy is a plain host file (0644); the mode the package sets on the
  # database node is published separately and asserted here.
  describe file('/mnt/backend/db.password') do
    it { should exist }
    its('mode') { should cmp 0o644 }
  end

  describe file('/mnt/backend/db-password.mode') do
    its('content') { should match(/\A640\n?\z/) }
  end

  describe file('/mnt/backend/ca.crt') do
    it { should exist }
    its('content') { should match(/BEGIN CERTIFICATE/) }
  end

  describe 'the trusted backend CAs' do
    it 'installs both certificates' do
      files = inspec
              .command('cat /usr/local/share/ca-certificates/microwave-backend-*.crt 2>/dev/null')
              .stdout
      expect(files.scan('BEGIN CERTIFICATE').size).to eq 2
    end
  end

  describe 'postgres over TLS' do
    it 'reaches the remote database with sslmode=require' do
      result = mw.psql('db.microwave.test', '/mnt/backend/db.password')

      expect(result.stdout.strip).to eq('1'), "psql said: #{result.stderr.to_s.strip}"
    end
  end

  describe 'opensearch over TLS' do
    health = mw.opensearch_health('search.microwave.test', '/mnt/backend/search-ca.crt')

    it('is green or yellow') { expect(health).to match(/"status":"(green|yellow)"/) }
  end

  # Remote clients reach the search proxy on the search node; its vhost has to
  # listen on that host and admit the fixture network. The file itself cannot be
  # read from here, so pre_converge publishes a copy into the mount; the HTTPS
  # probe above already proved this node gets through it.
  describe file('/mnt/backend/search-vhost.conf') do
    its('content') { should include 'listen search.microwave.test:9200 ssl;' }
    its('content') { should include 'allow 172.28.0.0/16;' }
  end
end

control 'microwave split organizations and clients' do
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
