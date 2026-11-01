microwave_server 'default' do
  database_url 'postgres://microwave:gen"erated@db.example.com:5432/microwave'
  search_url 'https://search.example.com:9200'
  bind 'a"b\c:8000'
  public_url "https://microwave.example.com/a\tb\nc\rd"
end
