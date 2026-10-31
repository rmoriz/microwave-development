# The server alone. A wrapper sets node['microwave']['database_host'] and
# node['microwave']['search_host']; the backend material is either handed in as
# attributes or read from the files the database resource wrote locally.

database_host = node['microwave']['database_host']
search_host = node['microwave']['search_host']

raise "microwave::server needs node['microwave']['database_host']" if database_host.to_s.empty?
raise "microwave::server needs node['microwave']['search_host']" if search_host.to_s.empty?

microwave_server 'default' do
  bind "#{node['microwave']['bind']}:8000"
  public_url node['microwave']['public_url'] unless node['microwave']['public_url'].to_s.empty?

  database_url lazy {
    password = node['microwave']['database_password']
    if password.to_s.empty?
      password = begin
                   ::File.read('/etc/microwave/db.password').strip
                 rescue Errno::ENOENT, Errno::EACCES
                   raise "microwave::server needs node['microwave']['database_password'] " \
                         'or a readable /etc/microwave/db.password'
                 end
    end

    if password.to_s.empty?
      raise "microwave::server needs node['microwave']['database_password'] " \
            'or a non-empty /etc/microwave/db.password'
    end

    "postgres://#{node['microwave']['database_user']}:#{password}@#{node['microwave']['database_host']}:" \
      "#{node['microwave']['database_port']}/#{node['microwave']['database_name']}?sslmode=require"
  }

  search_url "https://#{node['microwave']['search_host']}:9200"

  ca_certs lazy {
    if node['microwave']['ca_certs'].any?
      node['microwave']['ca_certs']
    elsif ::File.exist?(Microwave::TLS.ca_cert)
      [::File.read(Microwave::TLS.ca_cert)]
    else
      []
    end
  }
end

# The deployment proof: one admin, one organization, one membership. The CLI
# is microwave-ctl behind a chef-server-ctl symlink; the runbook name is kept.
# The command waits for the status endpoint itself, bounded, so a service that
# never comes up fails the converge loudly instead of skipping the step. Every
# step after that is guarded on its own, so a re-run repairs a half finished run
# instead of tripping over the user that already exists.
if node['microwave']['create_organization']
  ctl = 'chef-server-ctl --config /etc/microwave/microwave.toml'
  status_url = "http://#{node['microwave']['bind']}:8000/_status"

  apt_package 'curl' # the readiness probe below needs it, the package does not ship it

  execute 'microwave_organization' do
    command <<~COMMAND
      for i in $(seq 1 20); do curl -fsS #{status_url} && break; [ "$i" -lt 20 ] && sleep 3; done &&
      { #{ctl} user-list | grep -qx pivotal || #{ctl} user-create pivotal --email pivotal@example.com --admin -f /tmp/pivotal.pem ; } &&
      #{ctl} org-create demo --full-name 'Microwave Demo' --association-user pivotal -f /tmp/demo-validator.pem &&
      #{ctl} org-user-add demo pivotal
    COMMAND
    retries 3
    retry_delay 3
    not_if "#{ctl} org-list | grep -qx demo"
  end
end
