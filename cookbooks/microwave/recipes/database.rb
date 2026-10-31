# PostgreSQL alone, for a wrapper that points the server at this node.

microwave_postgresql 'default' do
  database node['microwave']['database_name']
  user node['microwave']['database_user']
  client_network node['microwave']['client_network']
end
