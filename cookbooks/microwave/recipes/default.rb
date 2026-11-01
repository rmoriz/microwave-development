# One node: PostgreSQL and OpenSearch on loopback, the server dialing both.
# The hosts are only defaulted here, so a wrapper that already points them
# somewhere else (its own split layout) keeps its values. server.rb reads them.

node.default['microwave']['database_host'] ||= '127.0.0.1'
node.default['microwave']['search_host'] ||= '127.0.0.1'

include_recipe 'microwave::database'
include_recipe 'microwave::search'

# The garage include stays after the backends and before the server, which
# reads the endpoint fill below.
if node['microwave']['object_store_backend'] == 'garage'
  # The endpoint lands in presigned URLs, so it has to match both the
  # certificate SAN and the vhost listener. Both use the resource's hostname
  # (FQDN, else hostname) and 127.0.0.1, not the server bind. A client network
  # adds the store's name; a split wrapper names the store node in
  # object_store_host or sets object_store_endpoint itself.
  if node['microwave']['object_store_endpoint'].to_s.empty?
    store_host = node['microwave']['object_store_host'].to_s
    if store_host.empty?
      store_host = '127.0.0.1'
      # cookstyle:disable Chef/RedundantCode/OhaiAttributeToString
      store_host = node['fqdn'].to_s.empty? ? node['hostname'] : node['fqdn'] if node['microwave']['client_network']
      # cookstyle:enable Chef/RedundantCode/OhaiAttributeToString
    end
    node.default['microwave']['object_store_endpoint'] = "https://#{store_host}:3900"
  end
  include_recipe 'microwave::garage'
end

include_recipe 'microwave::server'
