# One node: PostgreSQL and OpenSearch on loopback, the server dialing both.
# The hosts are only defaulted here, so a wrapper that already points them
# somewhere else (its own split layout) keeps its values. server.rb reads them.

node.default['microwave']['database_host'] ||= '127.0.0.1'
node.default['microwave']['search_host'] ||= '127.0.0.1'

include_recipe 'microwave::database'
include_recipe 'microwave::search'
include_recipe 'microwave::server'
