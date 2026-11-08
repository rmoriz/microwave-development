chef_client_systemd_timer 'chef-client' do
  interval '10min'
  splay '1min'
  delay_after_boot '10min'
  chef_binary_path '/opt/cinc/bin/cinc-client'
  config_directory '/etc/cinc'
  daemon_options %w(
    --json-attributes /etc/cinc/node.json
    --log-level info
    --no-color
  )
end
