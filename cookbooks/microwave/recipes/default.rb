# One node: PostgreSQL and OpenSearch on loopback, the server dialing both.
# The host attributes are set here so they win over a wrapper's split layout
# only by overriding them again; server.rb reads them.

node.default['microwave']['database_host'] = '127.0.0.1'
node.default['microwave']['search_host'] = '127.0.0.1'

include_recipe 'microwave::database'
include_recipe 'microwave::search'
include_recipe 'microwave::server'
