# Stops and disables every service, in reverse dependency order. The server
# first so it stops answering, nginx last because both proxies share it.
microwave_server 'default' do
  action :disable
end

microwave_opensearch 'default' do
  action :disable
end

microwave_postgresql 'default' do
  action :disable
end

microwave_garage 'default' do
  action :disable
end

file Microwave::Disabled.marker('nginx') do
  content "disabled by microwave::disable\n"
  owner 'root'
  group 'root'
  mode '0644'
end

service 'nginx' do
  action %i(stop disable)
  only_if { Microwave::Disabled.unit?('nginx') }
end
