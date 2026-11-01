require_relative 'spec_helper'

describe 'microwave attributes' do
  def node_for(fqdn)
    node = Chef::Node.new
    node.normal['hostname'] = 'kitchen-node'
    node.normal['fqdn'] = fqdn
    node.instance_eval(File.read(File.expand_path('../attributes/default.rb', __dir__)), __FILE__, 1)
    node
  end

  it 'binds to the fqdn when the node has one' do
    expect(node_for('microwave.example').dig('microwave', 'bind')).to eq('microwave.example')
  end

  it 'falls back to the hostname when the fqdn is empty' do
    expect(node_for('').dig('microwave', 'bind')).to eq('kitchen-node')
  end

  it 'falls back to the hostname when the fqdn is unset' do
    expect(node_for(nil).dig('microwave', 'bind')).to eq('kitchen-node')
  end

  it 'reads the database password from the local file by default' do
    expect(node_for('microwave.example').dig('microwave', 'database_password_file'))
      .to eq('/etc/microwave/db.password')
  end

  it 'defaults the CA file to nil so the local CA is used' do
    node = node_for('microwave.example')

    expect(node['microwave']).to have_key('ca_cert_file')
    expect(node.dig('microwave', 'ca_cert_file')).to be_nil
  end

  it 'defaults the object store to local so existing installs keep working' do
    expect(node_for('microwave.example').dig('microwave', 'object_store_backend')).to eq('local')
  end

  it 'defaults the garage key files next to the password file' do
    node = node_for('microwave.example')
    expect(node.dig('microwave', 'object_store_access_key_file')).to eq('/etc/microwave/garage.access_key')
    expect(node.dig('microwave', 'object_store_secret_key_file')).to eq('/etc/microwave/garage.secret_key')
  end

  it 'leaves the endpoint and the store host unset so the default fill decides' do
    node = node_for('microwave.example')

    expect(node['microwave']).to have_key('object_store_endpoint')
    expect(node.dig('microwave', 'object_store_endpoint')).to be_nil
    expect(node.dig('microwave', 'object_store_host')).to be_nil
  end

  it 'shares one bucket name between the store and the server' do
    node = node_for('microwave.example')

    expect(node.dig('microwave', 'object_store_bucket')).to eq('chef-cookbooks')
    expect(node.dig('microwave', 'object_store_bucket')).to eq(node.dig('microwave', 'garage_bucket'))
  end
end
