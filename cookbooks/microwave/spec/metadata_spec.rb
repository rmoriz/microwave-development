require 'spec_helper'

describe 'microwave metadata' do
  let(:metadata) do
    meta = Chef::Cookbook::Metadata.new
    meta.from_file('metadata.rb')
    meta
  end

  it 'is named microwave and supports debian 13 only' do
    expect(metadata.name).to eq('microwave')
    expect(metadata.platforms).to eq('debian' => '>= 13.0')
  end

  it 'accepts clients from 19.4.46 up to below 20' do
    expect(metadata.chef_versions.map(&:to_s)).to eq(['chef (>= 19.4.46, < 20)'])
  end

  it 'declares the recipes and the source' do
    expect(metadata.recipes.keys).to contain_exactly(
      'microwave::default', 'microwave::server', 'microwave::database', 'microwave::search'
    )
    expect(metadata.source_url).to eq('https://github.com/rmoriz/microwave-development')
  end
end
