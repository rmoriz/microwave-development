require 'chefspec'

RSpec.configure do |c|
  c.cookbook_path = [
    File.expand_path('..', __dir__),
    File.expand_path('../spec/fixtures/cookbooks', __dir__)
  ]
  c.platform = 'debian'
  c.version = '13'
end
