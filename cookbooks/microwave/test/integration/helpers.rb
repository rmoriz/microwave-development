# Probes shared by both suites. InSpec runs on the workstation and reaches the
# kitchen node through the docker backend, so every probe shells out on the node
# and every control asserts only what these helpers return.
require 'json'
require 'shellwords'

class Microwave
  STATUS_BODY = '/tmp/microwave-status.body'.freeze
  KNIFE_CONFIG = '/tmp/knife.rb'.freeze
  CONFIG = '/etc/microwave/microwave.toml'.freeze
  CTL = 'chef-server-ctl --config /etc/microwave/microwave.toml'.freeze
  PIVOTAL_KEY = '/var/lib/microwave/pivotal.pem'.freeze
  LEGACY_PIVOTAL_KEY = '/tmp/pivotal.pem'.freeze

  def initialize(backend)
    @backend = backend
  end

  # The listener the recipe wrote, for example 127.0.0.1:8000.
  def bind
    @bind ||= @backend.command("cat #{CONFIG}").stdout[/^bind = "(.*)"$/, 1].to_s
  end

  def base_url
    "http://#{bind}"
  end

  # GET /_status and return [body, code]. The body is kept on the node because
  # curl can only print one of the two.
  def status
    code = @backend.command(
      "curl -sS -o #{STATUS_BODY} -w '%{http_code}' #{base_url}/_status"
    ).stdout.to_i
    [@backend.command("cat #{STATUS_BODY}").stdout.strip, code]
  end

  def opensearch_health(host, ca)
    @backend.command("curl -sS --cacert #{ca} https://#{host}:9200/_cluster/health").stdout
  end

  # A remote connection over a freshly trusted CA and a just-installed client can
  # lose one attempt, so the probe retries and returns the last result.
  def psql(host, password_file, sql: 'select 1')
    result = nil
    3.times do
      result = @backend.command(
        "PGPASSWORD=\"$(cat #{password_file})\" psql 'host=#{host} user=microwave " \
        "dbname=microwave sslmode=require' -v ON_ERROR_STOP=1 -A -t " \
        "-c #{Shellwords.escape(sql)}"
      )
      break if result.exit_status.zero?

      sleep 2
    end
    result
  end

  def ctl(*args)
    @backend.command("#{CTL} #{args.join(' ')}")
  end

  # The server package pulls in no psql; the split assertions need one. The check
  # rides along with the install in the same shell: a follow-up exec can still see
  # a stale filesystem view for tens of seconds after apt returned. Three tries
  # are plenty for the apt lock right after the converge.
  def install_psql
    return if psql_works?

    3.times do
      return if @backend.command(
        'apt-get update -q && apt-get install -q -y --no-install-recommends ' \
        'postgresql-client && psql --version'
      ).exit_status.zero?

      sleep 2
    end
    warn 'psql is not usable on the node'
  end

  def psql_works?
    @backend.command('psql --version').exit_status.zero?
  end

  # The provisioner ships chef-client, which normally brings knife along; only
  # install it when it is really missing. Neither binary is part of the chef
  # gem any more, hence the separate knife gem. apt can be briefly locked right
  # after the converge, so the step is retried a few times.
  def install_knife
    install_knife_deps
    @knife_path = wait_for_knife(90)
    return true if @knife_path

    warn 'knife did not become usable within 90s'
    false
  end

  def install_knife_deps
    last = nil
    3.times do
      last = @backend.command(
        'apt-get update -q && apt-get install -q -y --no-install-recommends ' \
        'ruby ruby-dev build-essential libssl-dev && ' \
        'gem install chef knife --no-document'
      )
      return if last.exit_status.zero?

      sleep 2
    end
    warn "knife install failed: #{last.exit_status} #{last.stderr.to_s[0, 1000]}"
  end

  # `gem install` returns before the binstub is resolvable (rubygems writes it
  # afterwards), so poll rather than retrying the install: it took about four
  # seconds on a cold node.
  def wait_for_knife(seconds)
    deadline = Time.now + seconds
    loop do
      path = @backend.command('command -v knife').stdout.to_s.strip
      if !path.empty? && @backend.command("#{path} --version").exit_status.zero?
        return path
      end
      return if Time.now >= deadline

      sleep 1
    end
  end

  def knife_path
    @knife_path ||= wait_for_knife(30)
  end

  # The recipe writes the superuser key into the server's data directory. An
  # earlier converge kept it in /tmp, so fall back to that when it is missing.
  def pivotal_key
    return PIVOTAL_KEY if @backend.command("test -f #{PIVOTAL_KEY}").exit_status.zero?

    PIVOTAL_KEY if @backend.command("test -f #{LEGACY_PIVOTAL_KEY}").exit_status.zero?
  end

  def write_knife_config
    @backend.command(<<~SH)
      cat > #{KNIFE_CONFIG} <<'EOF'
      chef_server_url '#{base_url}/organizations/demo'
      node_name 'pivotal'
      client_key '#{pivotal_key || PIVOTAL_KEY}'
      ssl_verify_mode :verify_none
      EOF
      chmod 0600 #{KNIFE_CONFIG}
    SH
  end

  def knife(*args)
    @backend.command("#{knife_path} #{args.join(' ')} --config #{KNIFE_CONFIG}")
  end

  # Removes the node and client of an earlier verify. Both have to go: a client
  # left behind makes the re-registration a 409, a node left behind makes the
  # final PUT a 403, because the fresh client does not own the old record. Knife
  # reports a missing object with a non-zero status, so the loop polls rather
  # than guessing how long the delete takes to become visible.
  def clear_probe_identity
    last = nil
    5.times do
      # Every status has to be read on its own line: `knife` returns a lazy
      # command resource, and a short-circuited `&&` or an unread delete would
      # never run the command at all.
      node = knife('node', 'show', 'probe').exit_status
      client = knife('client', 'show', 'probe').exit_status
      return true if node != 0 && client != 0

      knife('node', 'delete', 'probe', '-y').exit_status
      last = knife('client', 'delete', 'probe', '-y')
      last.exit_status
      sleep 1
    end
    raise "probe node or client survived five deletions: #{last.stdout.to_s.strip} " \
          "#{last.stderr.to_s.strip[0, 300]}"
  end

  # Registers the node through the demo validator and converges the uploaded
  # cookbook; the saved node record proves the whole client path. The config is
  # passed explicitly: cinc looks in /etc/cinc, where the package default still
  # points at a validation key that does not exist. An earlier record is dropped
  # first so a second verify registers the node again instead of colliding; the
  # key from the first registration has to go with it, or cinc authenticates
  # with a client the server no longer knows.
  def probe
    clear_probe_identity
    @backend.command(<<~SH)
      install -d -m 0755 /etc/chef
      rm -f /etc/cinc/client.pem
      cat > /etc/chef/client.rb <<'EOF'
      chef_server_url "#{base_url}/organizations/demo"
      validation_client_name 'demo-validator'
      validation_key '/tmp/demo-validator.pem'
      ssl_verify_mode :verify_none
      EOF
      echo '{"run_list":["recipe[kitchen_probe]"]}' > /tmp/probe.json
      cinc-client --config /etc/chef/client.rb --node-name probe \
        --json-attributes /tmp/probe.json
    SH
  end
end
