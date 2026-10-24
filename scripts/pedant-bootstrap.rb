#!/usr/bin/env ruby
# frozen_string_literal: true

# Seed a fresh Microwave Server instance with the actors that oc-chef-pedant
# expects, then write a pedant_config.rb pointing at that instance.
#
# This intentionally uses only the public HTTP API (plus a narrowly scoped
# SQLite group-membership seed because the server does not yet expose the
# /organizations/:org/groups API).  It does not depend on the server's
# organization-creation response carrying a validator private key, which is
# itself one of the compatibility gaps this harness measures.

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
DATABASE = ENV.fetch("PEDANT_DATABASE")
API_VERSION = ENV.fetch("PEDANT_API_VERSION", "1")

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
#    Do not rely on the response's validator material: that is a compatibility
#    gap reported by the harness.
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

# 2. Validator and pedant requestor clients.  The server's organization
#    creation does not return a validator key, so create it explicitly as the
#    platform superuser.
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
keys[:validator] = create_client(VALIDATOR, validator: true)
keys[:admin_client] = create_client(ADMIN_CLIENT)
keys[:non_admin_client] = create_client(NON_ADMIN_CLIENT)
# The "bad" client is deliberately not created on the server: pedant uses it
# only as a bogus signer and expects it to be absent from GET /clients.
keys[:admin_user] = create_user(ADMIN_USER)
keys[:non_admin_user] = create_user(NON_ADMIN_USER)
keys[:bad_user] = create_user(BAD_USER)

# 4. Associate the two requestor users with the org.  The "bad" user must stay
#    outside the org.  The server only lets the platform superuser do this.
[ADMIN_USER, NON_ADMIN_USER].each do |username|
  expect!(
    "POST", "/organizations/#{ORG}/users",
    { "username" => username },
    allowed: [201, 409]
  )
end

# 5. Put the org admin into admins/users, the normal user into users, and the
#    two requestor clients into clients.  There is no public /groups endpoint
#    in the current server, so seed the same rows the authz API would write.
#    This is a harness workaround; it is documented in docs/pedant-report.md.
def seed_group(group_name, username: nil, client_name: nil)
  subject = if username
              "SELECT authz_id FROM users WHERE username = '#{username}'"
            else
              "SELECT c.authz_id FROM clients c JOIN orgs o ON o.id = c.org_id " \
                "WHERE o.name = '#{ORG}' AND c.name = '#{client_name}'"
            end
  sql = <<~SQL
    INSERT OR IGNORE INTO authz_group_actors (group_authz_id, actor_authz_id)
    SELECT g.authz_id, (#{subject})
    FROM groups g JOIN orgs o ON o.id = g.org_id
    WHERE o.name = '#{ORG}' AND g.name = '#{group_name}';
  SQL
  system("sqlite3", DATABASE, sql, exception: true)
end

seed_group("admins", username: ADMIN_USER)
seed_group("users", username: ADMIN_USER)
seed_group("billing-admins", username: ADMIN_USER)
seed_group("users", username: NON_ADMIN_USER)
seed_group("clients", client_name: ADMIN_CLIENT)
seed_group("clients", client_name: NON_ADMIN_CLIENT)
seed_group("clients", client_name: VALIDATOR)
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

  server_api_version #{API_VERSION.inspect}
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
        create_me: false,
        associate: false,
        key_file: #{key_paths[:admin_user].inspect},
      },
      non_admin: {
        name: #{NON_ADMIN_USER.inspect},
        create_me: false,
        associate: false,
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
