# One node: PostgreSQL and OpenSearch on loopback, the server dialing both.
# The hosts are only defaulted here, so a wrapper that already points them
# somewhere else (its own split layout) keeps its values. server.rb reads them.

node.default['microwave']['database_host'] ||= '127.0.0.1'
node.default['microwave']['search_host'] ||= '127.0.0.1'

if node['microwave']['object_store_backend'] == 'garage'
  # The endpoint lands in presigned URLs and has to match the certificate SAN.
  # A single node reaches the store on loopback; a split wrapper names the
  # store node in object_store_host or sets object_store_endpoint itself.
  if node['microwave']['object_store_endpoint'].to_s.empty?
    store_host = node['microwave']['object_store_host'].to_s
    store_host = node['microwave']['client_network'] ? node['microwave']['bind'] : '127.0.0.1' if store_host.empty?
    node.default['microwave']['object_store_endpoint'] = "https://#{store_host}:3900"
  end
  include_recipe 'microwave::garage'
end

include_recipe 'microwave::database'
include_recipe 'microwave::search'
include_recipe 'microwave::server'
