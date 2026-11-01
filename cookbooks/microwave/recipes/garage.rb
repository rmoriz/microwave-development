# Garage alone, for a wrapper that points the server at this node.

microwave_garage 'default' do
  capacity node['microwave']['garage_capacity']
  bucket node['microwave']['object_store_bucket']
  client_network node['microwave']['client_network']
end
