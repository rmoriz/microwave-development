microwave_server 'default' do
  database_url 'postgres://microwave:generated@db.example.com:5432/microwave?sslmode=require'
  search_url 'https://search.example.com:9200'
  bind "#{node['fqdn']}:8000"
  object_store_backend 'garage'
  object_store_bucket 'chef-cookbooks'
  object_store_region 'garage'
  object_store_path_style true
  object_store_access_key_file '/etc/microwave/garage.access_key'
  object_store_secret_key_file '/etc/microwave/garage.secret_key'
end
