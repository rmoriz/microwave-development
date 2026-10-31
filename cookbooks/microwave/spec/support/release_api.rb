# Stubs the GitHub releases API so the suite never touches the network.
RSpec.shared_context 'release api' do
  let(:release_json) do
    '{"tag_name":"v0.1.0","assets":[' \
      '{"name":"microwave-server_0.1.0_amd64.deb",' \
      '"browser_download_url":"https://example.invalid/microwave-server_0.1.0_amd64.deb?token=abc"},' \
      '{"name":"microwave-server_0.1.0_amd64.deb.sha256",' \
      '"browser_download_url":"https://example.invalid/microwave-server_0.1.0_amd64.deb.sha256?token=abc"},' \
      '{"name":"microwave-server_0.1.0_arm64.deb",' \
      '"browser_download_url":"https://example.invalid/microwave-server_0.1.0_arm64.deb"},' \
      '{"name":"microwave-server_0.1.0_arm64.deb.sha256",' \
      '"browser_download_url":"https://example.invalid/microwave-server_0.1.0_arm64.deb.sha256"}]}'
  end

  let(:digest_body) do
    '6d1f0c2e9a7b4c3d8e5f6a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f  ' \
      "microwave-server_0.1.0_amd64.deb\n"
  end

  let(:requested_urls) { [] }
  let(:requested_headers) { [] }

  before do
    allow_any_instance_of(Chef::HTTP::Simple).to receive(:get) do |_instance, url, headers|
      requested_urls << url
      requested_headers << headers
      url.include?('.sha256') ? digest_body : release_json
    end
  end

  # Every example starts without a token so a developer's shell cannot leak in.
  around do |example|
    token = ENV.fetch('GITHUB_TOKEN', nil)
    ENV.delete('GITHUB_TOKEN')
    begin
      example.run
    ensure
      ENV['GITHUB_TOKEN'] = token unless token.nil?
    end
  end
end
