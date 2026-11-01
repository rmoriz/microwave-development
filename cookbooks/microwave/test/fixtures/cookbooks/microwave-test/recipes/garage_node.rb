# The split object store node. The `client_network` 172.28.0.0/16 comes from
# /tmp/kitchen/attrs.json, so the rendered vhost admits the server node. The
# node publishes both key files and its CA to the host so the server can sign
# against it.

include_recipe 'microwave::garage'

file '/mnt/shared/garage.access_key' do
  content lazy { ::File.read('/etc/microwave/garage.access_key') }
  owner 'root'
  group 'root'
  mode '0644'
end

file '/mnt/shared/garage.secret_key' do
  content lazy { ::File.read('/etc/microwave/garage.secret_key') }
  owner 'root'
  group 'root'
  mode '0644'
end

file '/mnt/shared/garage-ca.crt' do
  content lazy { ::File.read(Microwave::TLS.ca_cert) }
  owner 'root'
  group 'root'
  mode '0644'
end

# Proves the imported key really owns the bucket; the verifier reads this file.
execute 'garage bucket info' do
  command lazy { "garage -c /etc/garage.toml bucket info #{node['microwave']['object_store_bucket']} > /mnt/shared/garage-bucket.txt" }
  sensitive true
end

# The last resource, so the file appears only once this node's converge is done.
# pre_converge waits for it before the server node starts.
file '/mnt/shared/garage.s3' do
  content 'done'
  owner 'root'
  group 'root'
  mode '0644'
end
