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
    password = ::File.read('/etc/microwave/db.password').strip if password.to_s.empty?

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
if node['microwave']['create_organization']
  ctl = 'chef-server-ctl --config /etc/microwave/microwave.toml'

  execute 'microwave_organization' do
    command <<~COMMAND
      #{ctl} user-create pivotal --email pivotal@example.com --admin -f /tmp/pivotal.pem &&
      #{ctl} org-create demo --full-name 'Microwave Demo' --association-user pivotal -f /tmp/demo-validator.pem &&
      #{ctl} org-user-add demo pivotal
    COMMAND
    not_if "#{ctl} org-list | grep -q demo"
  end
end
