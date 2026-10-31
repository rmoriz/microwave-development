require_relative 'spec_helper'

describe 'Microwave::Release' do
  include_context 'release api'

  it 'maps x86_64 to the amd64 asset, its name and its digest' do
    expect(Microwave::Release.asset(version: '0.1.0', arch: 'x86_64')).to eq(
      name: 'microwave-server_0.1.0_amd64.deb',
      url: 'https://example.invalid/microwave-server_0.1.0_amd64.deb?token=abc',
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

  it 'rejects a release without the digest with the tag and the digest asset' do
    allow_any_instance_of(Chef::HTTP::Simple).to receive(:get) do |_instance, _url, _headers|
      '{"tag_name":"v0.1.0","assets":[' \
        '{"name":"microwave-server_0.1.0_amd64.deb",' \
        '"browser_download_url":"https://example.invalid/microwave-server_0.1.0_amd64.deb"}]}'
    end

    expect { Microwave::Release.asset(version: '0.1.0', arch: 'x86_64') }.to raise_error(
      Microwave::Release::Missing,
      /\Arelease v0\.1\.0 has no asset microwave-server_0\.1\.0_amd64\.deb\.sha256\z/
    )
  end

  it 'rejects a release without the .deb with the tag and the .deb asset' do
    allow_any_instance_of(Chef::HTTP::Simple).to receive(:get) do |_instance, _url, _headers|
      '{"tag_name":"v0.1.0","assets":[' \
        '{"name":"microwave-server_0.1.0_amd64.deb.sha256",' \
        '"browser_download_url":"https://example.invalid/microwave-server_0.1.0_amd64.deb.sha256"}]}'
    end

    expect { Microwave::Release.asset(version: '0.1.0', arch: 'x86_64') }.to raise_error(
      Microwave::Release::Missing,
      /\Arelease v0\.1\.0 has no asset microwave-server_0\.1\.0_amd64\.deb\z/
    )
  end

  it 'rejects a draft tag with the tag instead of fetching its assets' do
    allow_any_instance_of(Chef::HTTP::Simple).to receive(:get) do |_instance, _url, _headers|
      '{"tag_name":"v0.2.0","draft":true,"assets":[' \
        '{"name":"microwave-server_0.2.0_amd64.deb",' \
        '"browser_download_url":"https://example.invalid/microwave-server_0.2.0_amd64.deb"}]}'
    end

    expect { Microwave::Release.asset(version: '0.2.0', arch: 'x86_64') }.to raise_error(
      Microwave::Release::Missing,
      /v0\.2\.0.*draft.*pin a published version/
    )
  end

  it 'rejects a digest asset without a download URL with the asset' do
    allow_any_instance_of(Chef::HTTP::Simple).to receive(:get) do |_instance, _url, _headers|
      '{"tag_name":"v0.1.0","assets":[' \
        '{"name":"microwave-server_0.1.0_amd64.deb",' \
        '"browser_download_url":"https://example.invalid/microwave-server_0.1.0_amd64.deb"},' \
        '{"name":"microwave-server_0.1.0_amd64.deb.sha256"}]}'
    end

    expect { Microwave::Release.asset(version: '0.1.0', arch: 'x86_64') }.to raise_error(
      Microwave::Release::Missing,
      /microwave-server_0\.1\.0_amd64\.deb\.sha256.*no browser_download_url/
    )
  end

  it 'rejects a blank digest body with the asset and the unusable digest' do
    allow_any_instance_of(Chef::HTTP::Simple).to receive(:get) do |_instance, url, _headers|
      url.include?('.sha256') ? "\n" : release_json
    end

    expect { Microwave::Release.asset(version: '0.1.0', arch: 'x86_64') }.to raise_error(
      Microwave::Release::Missing,
      /\Arelease v0\.1\.0 asset microwave-server_0\.1\.0_amd64\.deb\.sha256 has an unusable digest\z/
    )
  end

  it 'rejects a truncated digest body with the asset and the unusable digest' do
    allow_any_instance_of(Chef::HTTP::Simple).to receive(:get) do |_instance, url, _headers|
      url.include?('.sha256') ? "6d1f0c2e  microwave-server_0.1.0_amd64.deb\n" : release_json
    end

    expect { Microwave::Release.asset(version: '0.1.0', arch: 'x86_64') }.to raise_error(
      Microwave::Release::Missing, /microwave-server_0\.1\.0_amd64\.deb\.sha256.*unusable digest/
    )
  end

  it 'takes the assets of the /latest response without a second tags call' do
    Microwave::Release.asset(version: 'latest', arch: 'x86_64')

    expect(requested_urls).to eq(
      [
        'https://api.github.com/repos/rmoriz/microwave-development/releases/latest',
        'https://example.invalid/microwave-server_0.1.0_amd64.deb.sha256?token=abc',
      ]
    )
  end

  it 'requests the GitHub media type' do
    Microwave::Release.asset(version: '0.1.0', arch: 'x86_64')

    expect(requested_headers).to all(include('Accept' => 'application/vnd.github+json'))
  end

  it 'requests without Authorization when GITHUB_TOKEN is unset' do
    Microwave::Release.asset(version: '0.1.0', arch: 'x86_64')

    expect(requested_headers).to all(satisfy { |headers| !headers.key?('Authorization') })
  end

  context 'with GITHUB_TOKEN' do
    around do |example|
      ENV['GITHUB_TOKEN'] = 'secret-token'
      begin
        example.run
      ensure
        ENV.delete('GITHUB_TOKEN')
      end
    end

    it 'authorizes the request with the token' do
      Microwave::Release.asset(version: '0.1.0', arch: 'x86_64')

      expect(requested_headers).to all(include('Authorization' => 'Bearer secret-token'))
    end
  end
end
