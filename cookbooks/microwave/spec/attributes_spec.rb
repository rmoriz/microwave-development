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
end
