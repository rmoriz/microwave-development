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
default['microwave']['garage_version'] = '2.5.0'
default['microwave']['garage_capacity'] = '1G'
default['microwave']['garage_bucket'] = 'chef-cookbooks'
default['microwave']['garage_digests'] = {
  'x86_64' => '2ff667286217cd73ebcb1c03af993822365000441dfe31a80cb64c5189a9dea8',
  'aarch64' => '69b6040cef428b993500c17dd0d0c1240acf2f3f667b9b05ceba4b9660db4e8c',
}
# `garage_bucket` is the operator-facing knob; the store and the server read the
# shared name, which follows it unless the wrapper sets `object_store_bucket`.
default['microwave']['object_store_backend'] = 'local'
default['microwave']['object_store_bucket'] = lazy { node['microwave']['garage_bucket'] }
default['microwave']['object_store_region'] = 'garage'
default['microwave']['object_store_endpoint'] = nil
default['microwave']['object_store_host'] = nil
default['microwave']['object_store_path_style'] = true
default['microwave']['object_store_access_key_file'] = '/etc/microwave/garage.access_key'
default['microwave']['object_store_secret_key_file'] = '/etc/microwave/garage.secret_key'
default['microwave']['client_network'] = nil
default['microwave']['create_organization'] = false
