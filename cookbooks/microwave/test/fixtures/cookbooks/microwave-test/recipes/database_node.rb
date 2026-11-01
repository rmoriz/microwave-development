# The split database node. The `client_network` 172.28.0.0/16 comes from
# /tmp/kitchen/attrs.json; the node publishes its password and CA to the host
# so the server node can dial it over TLS. attrs.json also halves the OpenSearch
# heap (512m instead of the 1g default); the docker VM is too small for the
# default next to a running database.

include_recipe 'microwave::database'

file '/mnt/shared/db.password' do
  content lazy { ::File.read('/etc/microwave/db.password') }
  owner 'root'
  group 'root'
  mode '0644'
end

file '/mnt/shared/db-ca.crt' do
  content lazy { ::File.read(Microwave::TLS.ca_cert) }
  owner 'root'
  group 'root'
  mode '0644'
end

# The last resource, so the file appears only once this node's converge is done.
# The search node's healthcheck waits for it: the docker VM here is small and
# overlapping the two converges OOM-kills the search node.
file '/mnt/shared/db.converged' do
  content 'done'
  owner 'root'
  group 'root'
  mode '0644'
end
