require_relative 'spec_helper'

describe 'Microwave::TLS' do
  it 'names the CA and per-host files under /etc/microwave/tls' do
    expect(Microwave::TLS.ca_dir).to eq('/etc/microwave/tls')
    expect(Microwave::TLS.ca_cert).to eq('/etc/microwave/tls/ca.crt')
    expect(Microwave::TLS.ca_key).to eq('/etc/microwave/tls/ca.key')
    expect(Microwave::TLS.cert('db')).to eq('/etc/microwave/tls/db.crt')
    expect(Microwave::TLS.key('db')).to eq('/etc/microwave/tls/db.key')
  end

  it 'names the search files the same way' do
    expect(Microwave::TLS.cert('search')).to eq('/etc/microwave/tls/search.crt')
    expect(Microwave::TLS.key('search')).to eq('/etc/microwave/tls/search.key')
  end
end
