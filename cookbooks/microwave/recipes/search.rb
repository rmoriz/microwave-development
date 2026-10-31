# OpenSearch alone, for a wrapper that points the server at this node.

microwave_opensearch 'default' do
  heap node['microwave']['opensearch_heap']
  client_network node['microwave']['client_network']
end
