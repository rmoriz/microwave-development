file '/etc/e2e-marker' do
  content node['e2e_marker'].to_s
  mode '0644'
end
