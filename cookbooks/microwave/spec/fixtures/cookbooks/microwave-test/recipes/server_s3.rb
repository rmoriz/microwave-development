microwave_server 'default' do
  database_url 'postgres://microwave:generated@db.example.com:5432/microwave?sslmode=require'
  search_url 'https://search.example.com:9200'
  bind "#{node['fqdn']}:8000"
  object_store_backend 's3'
  object_store_bucket 'bucket'
  object_store_endpoint 'https://s3.example.com'
  object_store_region 'eu-central-1'
  object_store_path_style false
  object_store_access_key_file '/etc/microwave/garage.access_key'
  object_store_secret_key_file '/etc/microwave/garage.secret_key'
end
