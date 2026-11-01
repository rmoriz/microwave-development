# The split search node. The `client_network` 172.28.0.0/16 comes from
# /tmp/kitchen/attrs.json; the node publishes its CA to the host so the server
# node can dial it over TLS.

include_recipe 'microwave::search'

file '/mnt/shared/search-ca.crt' do
  content lazy { ::File.read(Microwave::TLS.ca_cert) }
  owner 'root'
  group 'root'
  mode '0644'
end

# The last resource, so the file appears only once this node's converge is done.
# The garage node's healthcheck waits for it.
file '/mnt/shared/search.converged' do
  content 'done'
  owner 'root'
  group 'root'
  mode '0644'
end
