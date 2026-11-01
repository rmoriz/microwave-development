# `node['fqdn']` is nil on a bare Chef::Node (the attribute spec pins that), so
# the `to_s` is there for the nil case; cookstyle sees an attribute that is
# already a string.
# cookstyle:disable Chef/RedundantCode/OhaiAttributeToString
default['microwave']['bind'] = node['fqdn'].to_s.empty? ? node['hostname'] : node['fqdn']
# cookstyle:enable Chef/RedundantCode/OhaiAttributeToString
default['microwave']['public_url'] = nil
default['microwave']['database_name'] = 'microwave'
default['microwave']['database_user'] = 'microwave'
default['microwave']['database_host'] = nil
default['microwave']['database_port'] = 5432
default['microwave']['database_password'] = nil
default['microwave']['database_password_file'] = '/etc/microwave/db.password'
default['microwave']['ca_certs'] = []
default['microwave']['ca_cert_file'] = nil
default['microwave']['search_host'] = nil
default['microwave']['opensearch_heap'] = '1g'
default['microwave']['client_network'] = nil
default['microwave']['create_organization'] = false
