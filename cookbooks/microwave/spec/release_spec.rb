require_relative 'spec_helper'

describe 'Microwave::Release' do
  include_context 'release api'

  it 'maps x86_64 to the amd64 asset and its digest' do
    expect(Microwave::Release.asset(version: '0.1.0', arch: 'x86_64')).to eq(
      url: 'https://example.invalid/microwave-server_0.1.0_amd64.deb',
      sha256: '6d1f0c2e9a7b4c3d8e5f6a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f'
    )
  end

  it 'maps aarch64 and arm64 to the arm64 asset' do
    %w(aarch64 arm64).each do |machine|
      expect(Microwave::Release.asset(version: '0.1.0', arch: machine))
        .to include(url: 'https://example.invalid/microwave-server_0.1.0_arm64.deb')
    end
  end

  it 'rejects an unknown machine with the tag and the machine' do
    expect { Microwave::Release.asset(version: '0.1.0', arch: 'riscv64') }
      .to raise_error(Microwave::Release::UnsupportedArch, /v0\.1\.0.*riscv64/)
  end

  it 'rejects a release without the digest with the tag and the asset' do
    allow_any_instance_of(Chef::HTTP::Simple).to receive(:get) do |_instance, _url, _headers|
      '{"tag_name":"v0.1.0","assets":[' \
        '{"name":"microwave-server_0.1.0_amd64.deb",' \
        '"browser_download_url":"https://example.invalid/microwave-server_0.1.0_amd64.deb"}]}'
    end

    expect { Microwave::Release.asset(version: '0.1.0', arch: 'x86_64') }
      .to raise_error(Microwave::Release::Missing, /v0\.1\.0.*microwave-server_0\.1\.0_amd64\.deb\.sha256/)
  end

  it 'rejects a blank digest body with the tag and the asset' do
    allow_any_instance_of(Chef::HTTP::Simple).to receive(:get) do |_instance, url, _headers|
      url.end_with?('.sha256') ? "\n" : release_json
    end

    expect { Microwave::Release.asset(version: '0.1.0', arch: 'x86_64') }
      .to raise_error(Microwave::Release::Missing, /v0\.1\.0.*microwave-server_0\.1\.0_amd64\.deb/)
  end
end
