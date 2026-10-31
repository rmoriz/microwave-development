require 'json'

# Resolves a Microwave Server release asset and its published digest from the
# GitHub releases API. Network errors from Chef::HTTP::Simple pass through.
module Microwave
  module Release
    unless const_defined?(:REPOSITORY, false)
      REPOSITORY = 'rmoriz/microwave-development'.freeze
      API = "https://api.github.com/repos/#{REPOSITORY}/releases".freeze
      PACKAGE = 'microwave-server'.freeze
      ARCHES = {
        'x86_64' => 'amd64',
        'aarch64' => 'arm64',
        'arm64' => 'arm64',
      }.freeze
      DIGEST = /\A\h{64}\z/.freeze
    end

    class Missing < StandardError; end
    class UnsupportedArch < StandardError; end

    # Returns { name:, url:, sha256: } for the release's .deb of the given machine.
    def self.asset(version:, arch:)
      release = release_for(version)
      tag = version == 'latest' ? release['tag_name'].to_s : "v#{version}"
      raise Missing, 'release latest has no tag_name' if tag.empty?
      if release['draft']
        raise Missing, "release #{tag} is a draft; pin a published version instead"
      end

      package = "#{PACKAGE}_#{tag.delete_prefix('v')}_#{architecture(arch, tag)}.deb"
      assets = release['assets'] || []

      deb = asset_named(assets, package, tag)
      url = download_url(deb, package, tag)
      digest = asset_named(assets, "#{package}.sha256", tag)
      sha256 = digest_for(digest, tag)

      { name: deb['name'], url: url, sha256: sha256 }
    end

    def self.release_for(version)
      version == 'latest' ? release_json("#{API}/latest") : release_json("#{API}/tags/v#{version}")
    end

    def self.asset_named(assets, name, tag)
      found = assets.find { |asset| asset['name'] == name }
      raise Missing, "release #{tag} has no asset #{name}" if found.nil?

      found
    end

    def self.download_url(asset, name, tag)
      url = asset['browser_download_url'].to_s
      raise Missing, "release #{tag} asset #{name} has no browser_download_url" if url.empty?

      url
    end

    def self.digest_for(asset, tag)
      name = asset['name']
      url = download_url(asset, name, tag)
      sha256 = fetch(url).split.first.to_s
      unless DIGEST.match?(sha256)
        raise Missing, "release #{tag} asset #{name} has an unusable digest"
      end

      sha256
    end

    def self.architecture(machine, tag)
      ARCHES.fetch(machine) do
        raise UnsupportedArch, "release #{tag} has no build for machine #{machine}"
      end
    end

    def self.release_json(url)
      JSON.parse(fetch(url))
    end

    def self.fetch(url)
      Chef::HTTP::Simple.new(url).get(url, headers)
    end

    def self.headers
      headers = {}
      headers['Accept'] = 'application/vnd.github+json'
      token = ENV.fetch('GITHUB_TOKEN', nil)
      headers['Authorization'] = "Bearer #{token}" unless token.nil? || token.empty?
      headers
    end
  end
end
