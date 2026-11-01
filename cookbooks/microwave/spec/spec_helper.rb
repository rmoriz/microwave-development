require 'English'
require 'resolv'

require 'chefspec'

# The bundle deliberately stays on Chef 18.3: Chef 19 pins inspec-core 7.2, and
# InSpec 6+ refuses to verify without a licence key, while 5.x only needs the
# EULA (CHEF_LICENSE, which the rake tasks set). The cookbook's floor stays
# 19.4.46 and the kitchen suites run exactly that client; the two things ChefSpec
# would miss on 18.3 are added here instead of lowering the claim.
#
# `apt_repository#signed_by` only arrived in Chef 19; the resource sets it to
# point apt at the keyring instead of a keyserver. The declaration matches
# Chef 19's (a path, or `true`/`false` to keep or drop the default).
unless Chef::Resource::AptRepository.properties.key?(:signed_by)
  Chef::Resource::AptRepository.property :signed_by, [String, true, false, nil], default: true
end

# `apt_package#environment` (the install-time DISABLE_* switches OpenSearch's
# postinst reads) is another 19-only addition.
unless Chef::Resource::AptPackage.properties.key?(:environment)
  Chef::Resource::AptPackage.property :environment, Hash
end
Dir[File.expand_path('../libraries/*.rb', __dir__)].sort.each { |library| load library }
Dir[File.expand_path('support/**/*.rb', __dir__)].sort.each { |support| require support }

RSpec.configure do |c|
  c.cookbook_path = [
    File.expand_path('../..', __dir__),
    File.expand_path('../spec/fixtures/cookbooks', __dir__),
  ]
  c.platform = 'debian'
  c.version = '13'

  # The server recipe resolves the client-facing bind name before writing it, so
  # the rendered config carries a literal address. Answer deterministically here
  # instead of asking the resolver of the build machine.
  c.before do
    # The bundle uses inspec-core 5 so the verifier runs without a Chef license
    # key; Chef 19 pins inspec-core 7.2 and is therefore not the client ChefSpec
    # gets (chef 18.3.0). The cookbook's own floor stays 19.4.46 and the kitchen
    # suites run exactly that client, so only this check is waived.
    allow_any_instance_of(Chef::Cookbook::Metadata).to receive(:validate_chef_version!)

    allow(Resolv).to receive(:getaddress) do |name|
      { 'chef.example.com' => '192.0.2.10', 'other.example.com' => '192.0.2.11' }.fetch(name, name)
    end

    # The server resource refreshes the apt index only while it does not know
    # logrotate; stub the guard so no spec runs apt-cache.
    stub_command('apt-cache show logrotate >/dev/null 2>&1').and_return(true)
  end
end
