microwave_server 'default' do
  database_url 'postgres://microwave:generated@db.example.com:5432/microwave?sslmode=require'
  search_url 'https://search.example.com:9200'
  bind "#{node['fqdn']}:8000"
  ca_certs %w(CA-PEM-1 CA-PEM-2)
end
