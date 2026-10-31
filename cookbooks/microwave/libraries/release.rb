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
    end

    class Missing < StandardError; end
    class UnsupportedArch < StandardError; end

    # Returns { url:, sha256: } for the release's .deb of the given machine.
    def self.asset(version:, arch:)
      tag = version == 'latest' ? latest_tag : "v#{version}"
      package = "#{PACKAGE}_#{tag.delete_prefix('v')}_#{architecture(arch, tag)}.deb"
      assets = release(tag)['assets'] || []

      deb = asset_named(assets, package, tag)
      digest = asset_named(assets, "#{package}.sha256", tag)
      sha256 = fetch(digest['browser_download_url']).split.first
      raise Missing, "release #{tag} has no digest for #{package}" if sha256.nil?

      { url: deb['browser_download_url'], sha256: sha256 }
    end

    def self.latest_tag
      tag = release_json("#{API}/latest")['tag_name'].to_s
      raise Missing, 'release latest has no tag_name' if tag.empty?

      tag
    end

    def self.release(tag)
      release_json("#{API}/tags/#{tag}")
    end

    def self.asset_named(assets, name, tag)
      found = assets.find { |asset| asset['name'] == name }
      raise Missing, "release #{tag} has no asset #{name}" if found.nil?

      found
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
      headers = { 'Accept' => 'application/vnd.github+json' }
      token = ENV.fetch('GITHUB_TOKEN', nil)
      headers['Authorization'] = "Bearer #{token}" unless token.nil? || token.empty?
      headers
    end
  end
end
