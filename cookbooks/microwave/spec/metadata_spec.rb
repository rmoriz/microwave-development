require_relative 'spec_helper'

describe 'microwave metadata' do
  let(:metadata) do
    meta = Chef::Cookbook::Metadata.new
    meta.from_file(File.expand_path('../metadata.rb', __dir__))
    meta
  end

  it 'is named microwave and supports debian 13 only' do
    expect(metadata.name).to eq('microwave')
    expect(metadata.version).to eq('0.1.0')
    expect(metadata.platforms).to eq('debian' => '>= 13.0')
  end

  it 'accepts clients from 19.4.46 up to below 20' do
    requirement = metadata.chef_versions.first.requirement
    expect(requirement).to be_satisfied_by(Gem::Version.new('19.4.46'))
    expect(requirement).to be_satisfied_by(Gem::Version.new('19.9.0'))
    expect(requirement).not_to be_satisfied_by(Gem::Version.new('19.4.45'))
    expect(requirement).not_to be_satisfied_by(Gem::Version.new('20.0.0'))
  end

  it 'declares the recipes and the source' do
    expect(metadata.recipes.keys).to contain_exactly(
      'microwave::default', 'microwave::server', 'microwave::database', 'microwave::search'
    )
    expect(metadata.source_url).to eq('https://github.com/rmoriz/microwave-development')
    expect(metadata.issues_url).to eq('https://github.com/rmoriz/microwave-development/issues')
  end
end
