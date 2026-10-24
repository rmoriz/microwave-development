#!/usr/bin/env ruby
# frozen_string_literal: true

# Seed a fresh Microwave Server instance with the actors that oc-chef-pedant
# expects, then write a pedant_config.rb pointing at that instance.
#
# This intentionally uses only the public HTTP API. The organization creation
# response carries the validator client name and private key; group membership
# is managed through the /organizations/:org/groups API.

require "fileutils"
require "json"
require "net/http"
require "openssl"
require "uri"
require "mixlib/authentication/signedheaderauth"

SERVER = ENV.fetch("PEDANT_SERVER_URL")
SUPERUSER_NAME = ENV.fetch("PEDANT_SUPERUSER_NAME", "pivotal")
SUPERUSER_KEY_PATH = ENV.fetch("PEDANT_SUPERUSER_KEY")
ORG = ENV.fetch("PEDANT_ORG", "pedant_testorg")
ARTIFACTS = ENV.fetch("PEDANT_ARTIFACTS")
# The unversioned pedant API suites target the legacy (v0) surface; specs that
# deliberately exercise server API v1+ switch to the maximum version
# themselves (see versioned_behaviors/server_api_v1_spec.rb).
API_VERSION = ENV.fetch("PEDANT_API_VERSION", "0")

ADMIN_USER = ENV.fetch("PEDANT_ADMIN_USER", "pedant_admin_user")
NON_ADMIN_USER = ENV.fetch("PEDANT_NON_ADMIN_USER", "pedant_user")
BAD_USER = ENV.fetch("PEDANT_BAD_USER", "pedant_nobody")
ADMIN_CLIENT = ENV.fetch("PEDANT_ADMIN_CLIENT", "pedant_admin_client")
NON_ADMIN_CLIENT = ENV.fetch("PEDANT_NON_ADMIN_CLIENT", "pedant_client")
BAD_CLIENT = ENV.fetch("PEDANT_BAD_CLIENT", "pedant_bad_client")
VALIDATOR = "#{ORG}-validator"

SUPERUSER_KEY = OpenSSL::PKey::RSA.new(File.read(SUPERUSER_KEY_PATH))

def request(method, path, payload = nil, key: SUPERUSER_KEY, user: SUPERUSER_NAME)
  uri = URI.join(SERVER, path)
  body = payload.nil? ? "" : JSON.generate(payload)
  signing_object = Mixlib::Authentication::SignedHeaderAuth.signing_object(
    http_method: method,
    path: uri.path,
    body: body,
    timestamp: Time.now.utc.iso8601,
    user_id: user
  )
  headers = {
    "Accept" => "application/json",
    "Content-Type" => "application/json",
    "Host" => "#{uri.host}:#{uri.port}",
    "X-Chef-Version" => "18.10.17",
    "X-Ops-Server-API-Version" => API_VERSION,
  }.merge(signing_object.sign(key))

  http = Net::HTTP.new(uri.host, uri.port)
  http.use_ssl = uri.scheme == "https"
  http.verify_mode = OpenSSL::SSL::VERIFY_NONE
  klass = {
    "GET" => Net::HTTP::Get,
    "POST" => Net::HTTP::Post,
    "PUT" => Net::HTTP::Put,
    "DELETE" => Net::HTTP::Delete,
  }.fetch(method)
  req = klass.new(uri.request_uri, headers)
  req.body = body unless body.empty?
  response = http.request(req)
  [response.code.to_i, response.body.to_s]
end

def expect!(method, path, payload = nil, allowed:, key: SUPERUSER_KEY, user: SUPERUSER_NAME)
  code, body = request(method, path, payload, key: key, user: user)
  unless allowed.include?(code)
    warn "bootstrap: #{method} #{path} -> #{code}"
    warn body
    raise "bootstrap request failed: #{method} #{path} returned #{code}"
  end
  parsed = body.empty? ? {} : JSON.parse(body)
  [code, parsed]
end

def write_private(path, contents)
  File.write(path, contents)
  File.chmod(0o600, path)
end

FileUtils.mkdir_p(ARTIFACTS)

# 1. Organization.  A fresh instance has no orgs, so this normally creates it.
#    The response includes the generated validator client material.
code, org_body = expect!(
  "POST", "/organizations",
  { "name" => ORG, "full_name" => ORG, "org_type" => "Business" },
  allowed: [200, 201, 409]
)
if code == 409
  puts "bootstrap: organization #{ORG} already exists"
else
  puts "bootstrap: created organization #{ORG} (clientname=#{org_body["clientname"].inspect})"
end

# 2. Validator and pedant requestor clients.  Prefer the validator created by
#    organization creation; fall back to explicitly creating it for older
#    servers.
def create_client(name, validator: false)
  code, body = expect!(
    "POST", "/organizations/#{ORG}/clients",
    { "name" => name, "validator" => validator, "create_key" => true },
    allowed: [200, 201, 409]
  )
  if code == 409
    raise "bootstrap: client #{name} already exists; run against a fresh data directory"
  end
  private_key = body["private_key"] || body.dig("chef_key", "private_key")
  raise "bootstrap: client #{name} creation returned no private key: #{body.inspect}" unless private_key

  private_key
end

# 3. Users.  create_key must be explicit because the server's POST /users does
#    not generate a default key unless asked; real Chef Server does so for
#    pedant's bootstrap request.
def create_user(username)
  code, body = expect!(
    "POST", "/users",
    {
      "username" => username,
      # pedant's authenticate_user expectations derive this from the
      # requestor's configured name.
      "email" => "#{username}@chef.io",
      "first_name" => username,
      "last_name" => username,
      "display_name" => username,
      # oc-chef-pedant hard-codes "foobar" for the local authenticate_user
      # examples, so the seeded requestors must use the same password.
      "password" => "foobar",
      "create_key" => true,
    },
    allowed: [200, 201, 409]
  )
  if code == 409
    raise "bootstrap: user #{username} already exists; run against a fresh data directory"
  end
  private_key = body["private_key"] || body.dig("chef_key", "private_key")
  raise "bootstrap: user #{username} creation returned no private key: #{body.inspect}" unless private_key

  private_key
end

keys = {}
validator_private_key = org_body["private_key"] || org_body.dig("chef_key", "private_key")
keys[:validator] = validator_private_key || create_client(VALIDATOR, validator: true)
keys[:admin_client] = create_client(ADMIN_CLIENT)
keys[:non_admin_client] = create_client(NON_ADMIN_CLIENT)
# The "bad" client is deliberately not created on the server: pedant uses it
# only as a bogus signer and expects it to be absent from GET /clients.
#
# The org admin user is also left to pedant (create_me: true below). Pedant's
# requestor setup drops the `admin`/`associate` flags for pre-existing users,
# so pre-seeding the admin user would make its group-membership expectations
# disagree with the (correct) server state.
keys[:non_admin_user] = create_user(NON_ADMIN_USER)
keys[:bad_user] = create_user(BAD_USER)

# 4. Association is left to pedant's requestor setup (make_owner/make_user),
#    which knows whether each configured user should be associated.
# 5. Populate the default groups through the public groups API.  The admin
#    user does not exist yet; pedant's make_owner adds it to admins,
#    billing-admins and users during requestor setup.
def update_group(group_name, users:, clients:, groups: [])
  expect!(
    "PUT", "/organizations/#{ORG}/groups/#{group_name}",
    {
      "groupname" => group_name,
      "actors" => {
        "users" => users,
        "clients" => clients,
        "groups" => groups,
      },
    },
    allowed: [200]
  )
end

update_group("admins", users: [SUPERUSER_NAME], clients: [])
update_group("billing-admins", users: [], clients: [])
update_group("users", users: [SUPERUSER_NAME, NON_ADMIN_USER], clients: [])
update_group("clients", users: [], clients: [ADMIN_CLIENT, NON_ADMIN_CLIENT, VALIDATOR])
# 6. Write private keys where pedant expects them.
key_paths = {}
keys.each do |name, pem|
  path = File.join(ARTIFACTS, "keys", "#{name}.pem")
  FileUtils.mkdir_p(File.dirname(path))
  write_private(path, pem)
  key_paths[name] = path
end
FileUtils.cp(SUPERUSER_KEY_PATH, File.join(ARTIFACTS, "keys", "pivotal.pem"))
File.chmod(0o600, File.join(ARTIFACTS, "keys", "pivotal.pem"))

# 7. Write the pedant configuration.
config = <<~RUBY
  # Generated by scripts/pedant-bootstrap.rb
  base_resource_url #{SERVER.inspect}
  chef_server #{SERVER.inspect}
  internal_server #{SERVER.inspect}

  superuser_name #{SUPERUSER_NAME.inspect}
  superuser_key #{File.join(ARTIFACTS, "keys", "pivotal.pem").inspect}

  org({ name: #{ORG.inspect}, validator_key: #{key_paths[:validator].inspect} })
  validate_org_creation false
  delete_org false

  server_api_version #{API_VERSION.to_i}
  include_internal false
  old_runlists_and_search true

  log_file #{File.join(ARTIFACTS, "http-traffic.log").inspect}

  requestors({
    clients: {
      admin: {
        name: #{ADMIN_CLIENT.inspect},
        create_me: false,
        key_file: #{key_paths[:admin_client].inspect},
        admin: true,
      },
      non_admin: {
        name: #{NON_ADMIN_CLIENT.inspect},
        create_me: false,
        key_file: #{key_paths[:non_admin_client].inspect},
      },
      bad: {
        name: #{BAD_CLIENT.inspect},
        create_me: false,
        bogus: true,
      },
    },
    users: {
      admin: {
        name: #{ADMIN_USER.inspect},
        create_me: true,
        associate: true,
        admin: true,
      },
      non_admin: {
        name: #{NON_ADMIN_USER.inspect},
        create_me: false,
        associate: true,
        key_file: #{key_paths[:non_admin_user].inspect},
      },
      bad: {
        name: #{BAD_USER.inspect},
        create_me: false,
        associate: false,
        key_file: #{key_paths[:bad_user].inspect},
      },
    },
  })
RUBY

config_path = File.join(ARTIFACTS, "pedant_config.rb")
File.write(config_path, config)
puts "bootstrap: wrote #{config_path}"
puts "bootstrap: org=#{ORG} admin_user=#{ADMIN_USER} validator=#{VALIDATOR}"
