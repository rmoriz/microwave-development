microwave_garage 'default' do
  hostname 'garage.example.com'
  client_network node['microwave']['client_network']
end
