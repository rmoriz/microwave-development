microwave_server 'default' do
  database_url 'postgres://microwave:generated@db.example.com:5432/microwave?sslmode=require'
  search_url 'https://search.example.com:9200'
  public_url 'https://microwave.example.com'
  ca_certs ['CA-PEM']
end
