require_relative 'spec_helper'

describe Microwave::Disabled do
  it 'maps a unit to its marker' do
    expect(described_class.marker('postgresql')).to eq('/etc/microwave/disabled/postgresql')
  end

  it 'reports a marker as set only while the file exists' do
    allow(::File).to receive(:exist?).with('/etc/microwave/disabled/garage').and_return(true)

    expect(described_class.marked?('garage')).to be(true)
  end

  it 'finds a unit in any of the three unit directories' do
    allow(::File).to receive(:exist?).and_call_original
    allow(::File).to receive(:exist?).with('/lib/systemd/system/nginx.service').and_return(true)

    expect(described_class.unit?('nginx')).to be(true)
    expect(described_class.unit?('absent')).to be(false)
  end
end
