# The inverse of microwave::disable. nginx first because both proxies need it
# listening, the server last so it does not probe a backend that is still down.
file Microwave::Disabled.marker('nginx') do
  action :delete
end

service 'nginx' do
  action %i(enable start)
  only_if { Microwave::Disabled.unit?('nginx') }
end

microwave_postgresql 'default' do
  action :enable
end

microwave_opensearch 'default' do
  action :enable
end

microwave_server 'default' do
  action :enable
end
