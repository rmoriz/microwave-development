# The server alone. A wrapper sets node['microwave']['database_host'] and
# node['microwave']['search_host']; the backend material is either handed in as
# attributes or read from files. The password comes from
# node['microwave']['database_password_file'], which defaults to the file the
# database resource writes on a single node. The CA comes from
# node['microwave']['ca_certs'] or node['microwave']['ca_cert_file']; without
# either, the local CA the database wrote is trusted.

database_host = node['microwave']['database_host']
search_host = node['microwave']['search_host']
password_file = node['microwave']['database_password_file']
ca_cert_file = node['microwave']['ca_cert_file']

raise "microwave::server needs node['microwave']['database_host']" if database_host.to_s.empty?
raise "microwave::server needs node['microwave']['search_host']" if search_host.to_s.empty?

# The bind doubles as the fallback base the served and the presigned URLs are
# built from, so a name is resolved to a literal address before the resource
# runs and never reaches those URLs. This is the recipe-side half of the pair:
# `microwave-ctl check-config` resolves a name itself, for hand-written configs,
# so both paths end at an address. `Resolv.getaddress` returns the first address
# the resolver hands back, in its order, and that one is used as is.
#
# A literal address is kept as configured, loopback included: `127.0.0.1` is a
# deliberate choice in front of a local TLS proxy, and swapping it for the
# primary address would silently ignore it.
#
# A *resolved* loopback result is not good enough for the served URLs: stock
# Debian maps the hostname to 127.0.1.1 in /etc/hosts, so resolving the default
# bind would leave the listener loopback-only. Prefer the node's primary address
# whenever the name resolves to loopback or does not resolve at all. A host that
# is reachable from outside wants an explicit name that resolves to its public
# address, that address itself, or `public_url`. `0.0.0.0` listens on every
# interface, loopback included, but publishes no name of its own.
require 'ipaddr'
require 'resolv'
primary_address = node['ipaddress'].to_s
primary_address = '0.0.0.0' if primary_address.empty?
configured_bind = node['microwave']['bind'].to_s
listen_address = begin
                   IPAddr.new(configured_bind).to_s
                 rescue ArgumentError, TypeError
                   begin
                     resolved = Resolv.getaddress(configured_bind)
                     IPAddr.new(resolved).loopback? ? primary_address : resolved
                   rescue StandardError
                     primary_address
                   end
                 end
listen = "#{listen_address.include?(':') ? "[#{listen_address}]" : listen_address}:8000"

microwave_server 'default' do
  bind listen
  public_url node['microwave']['public_url'] unless node['microwave']['public_url'].to_s.empty?

  object_store_backend node['microwave']['object_store_backend']
  object_store_bucket node['microwave']['object_store_bucket']
  object_store_endpoint node['microwave']['object_store_endpoint']
  object_store_region node['microwave']['object_store_region']
  object_store_path_style node['microwave']['object_store_path_style']
  object_store_access_key_file node['microwave']['object_store_access_key_file']
  object_store_secret_key_file node['microwave']['object_store_secret_key_file']

  database_url lazy {
    password = node['microwave']['database_password']
    if password.to_s.empty?
      password = begin
                   ::File.read(password_file).strip
                 rescue Errno::ENOENT, Errno::EACCES
                   raise "microwave::server needs node['microwave']['database_password'] " \
                         "or a readable #{password_file}"
                 end
    end

    if password.to_s.empty?
      raise "microwave::server needs node['microwave']['database_password'] " \
            "or a non-empty #{password_file}"
    end

    "postgres://#{node['microwave']['database_user']}:#{password}@#{node['microwave']['database_host']}:" \
      "#{node['microwave']['database_port']}/#{node['microwave']['database_name']}?sslmode=require"
  }

  search_url "https://#{node['microwave']['search_host']}:9200"

  ca_certs lazy {
    if node['microwave']['ca_certs'].any?
      node['microwave']['ca_certs']
    elsif ca_cert_file && ::File.exist?(ca_cert_file)
      [::File.read(ca_cert_file)]
    elsif ::File.exist?(Microwave::TLS.ca_cert)
      [::File.read(Microwave::TLS.ca_cert)]
    else
      []
    end
  }
end

# The package creates /etc/microwave as 0750 root:microwave. PostgreSQL and
# OpenSearch run as their own users and read the TLS material below it, so the
# directory is widened for every topology. Managing it here keeps the backend
# resources from flipping it back on the next run.
directory '/etc/microwave' do
  owner 'root'
  group 'root'
  mode '0755'
end

# The deployment proof: one admin, one organization, one membership. The CLI
# is microwave-ctl behind a chef-server-ctl symlink; the runbook name is kept.
# The command waits for the status endpoint itself, bounded, so a service that
# never comes up fails the converge loudly instead of skipping the step. Every
# step after that is guarded on its own, so a re-run repairs a half finished run
# instead of tripping over the user that already exists.
if node['microwave']['create_organization']
  ctl = 'chef-server-ctl --config /etc/microwave/microwave.toml'
  status_url = "http://#{listen}/_status"

  apt_package 'curl' # the readiness probe below needs it, the package does not ship it

  execute 'microwave_organization' do
    command <<~COMMAND
      ready=0
      for i in $(seq 1 20); do
        curl -fsS --max-time 5 #{status_url} >/dev/null && { ready=1; break; }
        sleep 3
      done
      # The last body names the failing upstream; the retry budget alone would
      # only say that the endpoint answered 500.
      if [ "$ready" -ne 1 ]; then curl -sS --max-time 5 #{status_url}; exit 1; fi
      { #{ctl} user-list | grep -qx pivotal || #{ctl} user-create pivotal --email pivotal@example.com --admin -f /var/lib/microwave/pivotal.pem ; } &&
      #{ctl} org-create demo --full-name 'Microwave Demo' --association-user pivotal -f /tmp/demo-validator.pem &&
      #{ctl} org-user-add demo pivotal
    COMMAND
    retries 3
    retry_delay 3
    # The block guard is first so Chef short-circuits the list: a node disabled
    # by microwave::disable never waits for the status endpoint.
    not_if { Microwave::Disabled.marked?('microwave-server') }
    not_if "#{ctl} org-list | grep -qx demo"
  end
end
