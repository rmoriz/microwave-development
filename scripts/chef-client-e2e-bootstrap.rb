#!/usr/bin/env ruby
# frozen_string_literal: true

# Seed a fresh Microwave Server instance for the chef-client end-to-end
# harness, then (in "verify" mode) check the converge result through the API.
#
# This uses only the public HTTP API and mixlib-authentication, which ships
# with every chef-client installation.
#
# Modes:
#   ruby chef-client-e2e-bootstrap.rb          # bootstrap
#   ruby chef-client-e2e-bootstrap.rb verify   # verify node/client

require "digest/md5"
require "fileutils"
require "json"
require "net/http"
require "openssl"
require "uri"
require "mixlib/authentication/signedheaderauth"

SERVER = ENV.fetch("MW_E2E_SERVER_URL")
ARTIFACTS = ENV.fetch("MW_E2E_ARTIFACTS")
SUPERUSER_NAME = ENV.fetch("MW_E2E_SUPERUSER_NAME", "pivotal")
SUPERUSER_KEY_PATH = ENV.fetch("MW_E2E_SUPERUSER_KEY")
ORG = ENV.fetch("MW_E2E_ORG", "mw_e2e")
NODE_NAME = ENV.fetch("MW_E2E_NODE_NAME", "mw-e2e-node")
COOKBOOK = ENV.fetch("MW_E2E_COOKBOOK", "mw_e2e")
COOKBOOK_VERSION = ENV.fetch("MW_E2E_COOKBOOK_VERSION", "0.1.0")
MARKER = ENV.fetch("MW_E2E_MARKER")
COMPAT = ENV.fetch("MW_E2E_COMPAT", "1") == "1"
SYNTH_V2 = ENV.fetch("MW_E2E_SYNTH_V2", "1") == "1"

SUPERUSER_KEY = OpenSSL::PKey::RSA.new(File.read(SUPERUSER_KEY_PATH))
CAPABILITIES = {}

def request(method, path, payload = nil, key: SUPERUSER_KEY, user: SUPERUSER_NAME, api_version: "0")
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
    "X-Chef-Version" => "18.7.10",
    "X-Ops-Server-API-Version" => api_version,
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

def expect!(method, path, payload = nil, allowed: [200, 201], **options)
  code, body = request(method, path, payload, **options)
  unless allowed.include?(code)
    warn "bootstrap: #{method} #{path} -> #{code}"
    warn body
    raise "bootstrap request failed: #{method} #{path} returned #{code}"
  end
  parsed = body.empty? ? {} : JSON.parse(body)
  [code, parsed]
end

def write_private(path, contents)
  FileUtils.mkdir_p(File.dirname(path))
  File.write(path, contents)
  File.chmod(0o600, path)
end

def write_capabilities
  File.write(
    File.join(ARTIFACTS, "capabilities.json"),
    JSON.pretty_generate(CAPABILITIES) + "\n"
  )
end

# ---- verify mode ------------------------------------------------------------
if ARGV.first == "verify"
  node_name = ENV.fetch("MW_E2E_NODE_NAME", "mw-e2e-node")
  code, body = request("GET", "/organizations/#{ORG}/nodes/#{node_name}")
  if code != 200
    warn "verify: GET node #{node_name} -> #{code}"
    warn body
    exit 1
  end
  node = JSON.parse(body)
  failures = []
  failures << "node name #{node["name"].inspect} != #{node_name.inspect}" unless node["name"] == node_name
  unless Array(node["run_list"]).include?("recipe[#{COOKBOOK}]")
    failures << "run_list #{node["run_list"].inspect} does not include recipe[#{COOKBOOK}]"
  end
  version = node.dig("automatic", "cookbooks", COOKBOOK, "version")
  failures << "node has no automatic.cookbooks.#{COOKBOOK}.version" if version.nil?

  client_code, client_body = request("GET", "/organizations/#{ORG}/clients/#{node_name}")
  failures << "client #{node_name} not found (GET -> #{client_code})" unless client_code == 200

  summary = {
    "node" => node_name,
    "node_status" => code,
    "client_status" => client_code,
    "run_list" => node["run_list"],
    "cookbook_version" => version,
    "failures" => failures,
  }
  puts JSON.pretty_generate(summary)
  exit(failures.empty? ? 0 : 1)
end

# ---- bootstrap mode ---------------------------------------------------------
FileUtils.mkdir_p(ARTIFACTS)

# 1. Organization + validator. The create response carries the validator name
#    and private key.
code, org_body = expect!(
  "POST", "/organizations",
  { "name" => ORG, "full_name" => "Microwave E2E" },
  allowed: [200, 201]
)
validator_name = org_body["clientname"]
validator_key = org_body["private_key"]
org_id = org_body["guid"]
raise "bootstrap: organization response missing validator key: #{org_body.inspect}" unless validator_key

write_private(File.join(ARTIFACTS, "keys", "validator.pem"), validator_key)

# 2. A normal client for the converge.
code, client_body = expect!(
  "POST", "/organizations/#{ORG}/clients",
  { "name" => NODE_NAME, "validator" => false, "create_key" => true },
  allowed: [200, 201]
)
client_key = client_body["private_key"] || client_body.dig("chef_key", "private_key")
raise "bootstrap: client response missing private key: #{client_body.inspect}" unless client_key

client_key_path = File.join(ARTIFACTS, "keys", "client.pem")
write_private(client_key_path, client_key)

# 3. Strict capability probe: can a normal client read the default
#    environment? A stock Chef Server's default org policy grants the
#    `clients` group read access to environments.
env_path = "/organizations/#{ORG}/environments/_default"
env_status, env_body = request(
  "GET", env_path, nil,
  key: OpenSSL::PKey::RSA.new(client_key), user: NODE_NAME, api_version: "2"
)
CAPABILITIES["environment_read_status"] = env_status
CAPABILITIES["environment_acl_granted"] = false
File.write(
  File.join(ARTIFACTS, "environment-probe-before.json"),
  JSON.pretty_generate({ "status" => env_status, "body" => env_body }) + "\n"
)

if env_status == 403 && COMPAT
  # Grant the default environment's read ACL to the clients group, preserving
  # the existing actors/groups. This mirrors the stock org policy and is
  # recorded as a server gap in the report.
  acl_code, acl = expect!(
    "GET", "#{env_path}/_acl",
    nil,
    allowed: [200]
  )
  read = acl["read"] || {}
  actors = Array(read["actors"])
  groups = Array(read["groups"])
  groups << "clients" unless groups.include?("clients")
  expect!(
    "PUT", "#{env_path}/_acl/read",
    { "actors" => actors, "groups" => groups },
    allowed: [200]
  )
  CAPABILITIES["environment_acl_granted"] = true
  env_status, env_body = request(
    "GET", env_path, nil,
    key: OpenSSL::PKey::RSA.new(client_key), user: NODE_NAME, api_version: "2"
  )
  CAPABILITIES["environment_read_status_after_grant"] = env_status
  File.write(
    File.join(ARTIFACTS, "environment-probe-after.json"),
    JSON.pretty_generate({ "status" => env_status, "body" => env_body }) + "\n"
  )
  warn "bootstrap: server gap: default environment ACL did not allow clients; granted read to the clients group"
elsif env_status == 403
  warn "bootstrap: default environment read is 403 and MW_E2E_COMPAT=0; the chef-client run is expected to fail"
end

# 4. Build the cookbook on disk.
cookbook_dir = File.join(ARTIFACTS, "cookbook")
FileUtils.mkdir_p(File.join(cookbook_dir, "recipes"))
metadata = <<~RUBY
  name "#{COOKBOOK}"
  version "#{COOKBOOK_VERSION}"
  description "Microwave chef-client e2e cookbook"
  maintainer "Microwave E2E"
  maintainer_email "e2e@example.invalid"
  license "Apache-2.0"
RUBY
recipe = <<~RUBY
  file #{MARKER.inspect} do
    content "converged by chef-client e2e\\n"
    mode "0644"
  end
RUBY
File.write(File.join(cookbook_dir, "metadata.rb"), metadata)
File.write(File.join(cookbook_dir, "recipes", "default.rb"), recipe)

files = {
  "root_files" => ["metadata.rb"],
  "recipes" => ["default.rb"],
}
entries = files.flat_map do |segment, names|
  names.map do |name|
    path = segment == "root_files" ? name : File.join(segment, name)
    data = File.binread(File.join(cookbook_dir, path))
    {
      "segment" => segment,
      "name" => name,
      "path" => path,
      "checksum" => Digest::MD5.hexdigest(data),
      "data" => data,
    }
  end
end

# 5. Upload through the sandbox API.
checksums = entries.to_h { |entry| [entry["checksum"], nil] }
code, sandbox = expect!(
  "POST", "/organizations/#{ORG}/sandboxes",
  { "checksums" => checksums },
  allowed: [200, 201]
)
sandbox_id = sandbox["sandbox_id"]
raise "bootstrap: sandbox response missing id: #{sandbox.inspect}" unless sandbox_id

entries.each do |entry|
  info = sandbox.fetch("checksums").fetch(entry["checksum"])
  next unless info["needs_upload"]

  uri = URI.parse(info.fetch("url"))
  http = Net::HTTP.new(uri.host, uri.port)
  http.use_ssl = uri.scheme == "https"
  http.verify_mode = OpenSSL::SSL::VERIFY_NONE
  response = http.send_request("PUT", uri.request_uri, entry["data"], { "Content-Type" => "application/octet-stream" })
  unless response.code.to_i == 200
    warn "bootstrap: upload #{entry["path"]} -> #{response.code}"
    warn response.body
    raise "bootstrap: cookbook file upload failed"
  end
end

expect!("PUT", "/organizations/#{ORG}/sandboxes/#{sandbox_id}", { "is_completed" => true })

# 6. Register the cookbook version manifest (API v0 shape: one array per
#    segment). The server adds presigned download URLs on read/solve.
def file_records(entries, segment)
  entries.select { |entry| entry["segment"] == segment }.map do |entry|
    {
      "name" => entry["name"],
      "path" => entry["path"],
      "checksum" => entry["checksum"],
      "specificity" => "default",
    }
  end
end

manifest = {
  "name" => "#{COOKBOOK}-#{COOKBOOK_VERSION}",
  "version" => COOKBOOK_VERSION,
  "cookbook_name" => COOKBOOK,
  "metadata" => {
    "name" => COOKBOOK,
    "version" => COOKBOOK_VERSION,
    "description" => "Microwave chef-client e2e cookbook",
    "maintainer" => "Microwave E2E",
    "maintainer_email" => "e2e@example.invalid",
    "license" => "Apache-2.0",
    "dependencies" => {},
    "recipes" => { "default" => "" },
  },
  "recipes" => file_records(entries, "recipes"),
  "root_files" => file_records(entries, "root_files"),
  "definitions" => [],
  "libraries" => [],
  "attributes" => [],
  "files" => [],
  "templates" => [],
  "resources" => [],
  "providers" => [],
}
expect!(
  "PUT", "/organizations/#{ORG}/cookbooks/#{COOKBOOK}/#{COOKBOOK_VERSION}",
  manifest,
  allowed: [200, 201]
)

# 7. Ask the server for the run's cookbook set as the client would (API v2).
#    Stock chef-client 18/19 requests API v2 and expects `all_files` entries
#    with download URLs. If the server still returns the v0 segment shape,
#    synthesize a v2 manifest from the server's own v0 response (which carries
#    the server-generated GET URLs) and upload it as a new patch version.
def solve_as_client(client_key)
  request(
    "POST", "/organizations/#{ORG}/environments/_default/cookbook_versions",
    { "run_list" => ["recipe[#{COOKBOOK}]"] },
    key: OpenSSL::PKey::RSA.new(client_key), user: NODE_NAME, api_version: "2"
  )
end

def v2_ready?(solution)
  entry = solution[COOKBOOK]
  files = entry && entry["all_files"]
  files.is_a?(Array) && !files.empty? && files.all? { |file| file["url"].is_a?(String) && !file["url"].empty? }
end

solve_status, solve_body = solve_as_client(client_key)
solution = solve_body.empty? ? {} : JSON.parse(solve_body)
CAPABILITIES["v2_solve_status"] = solve_status
CAPABILITIES["v2_all_files"] = v2_ready?(solution)
CAPABILITIES["v2_synthesized"] = false
CAPABILITIES["cookbook_version_used"] = solution.dig(COOKBOOK, "version")
File.write(
  File.join(ARTIFACTS, "v2-solve-before.json"),
  JSON.pretty_generate({ "status" => solve_status, "body" => solution }) + "\n"
)

if solve_status == 200 && !v2_ready?(solution) && COMPAT && SYNTH_V2
  # Fetch the v0 manifest (the server adds presigned GET URLs to each segment).
  code, v0 = expect!(
    "GET", "/organizations/#{ORG}/cookbooks/#{COOKBOOK}/#{COOKBOOK_VERSION}",
    nil,
    allowed: [200]
  )
  File.write(
    File.join(ARTIFACTS, "v0-manifest.json"),
    JSON.pretty_generate(v0) + "\n"
  )
  segments = %w[recipes definitions libraries attributes files templates resources providers root_files]
  all_files = segments.flat_map do |segment|
    Array(v0[segment]).map do |file|
      {
        "name" => file["path"] || "#{segment}/#{file["name"]}",
        "path" => file["path"] || "#{segment}/#{file["name"]}",
        "checksum" => file["checksum"],
        "specificity" => file["specificity"] || "default",
        "url" => file["url"],
      }
    end
  end
  missing_urls = all_files.select { |file| file["url"].nil? || file["url"].empty? }
  unless missing_urls.empty?
    warn "bootstrap: server did not add download URLs to the v0 manifest: #{missing_urls.inspect}"
    raise "bootstrap: cannot synthesize a v2 manifest without server-generated URLs"
  end

  major, minor, patch = COOKBOOK_VERSION.split(".").map(&:to_i)
  v2_version = format("%d.%d.%d", major, minor, patch + 1)
  v2_metadata = (v0["metadata"] || {}).merge("name" => COOKBOOK, "version" => v2_version)
  v2_manifest = {
    "name" => "#{COOKBOOK}-#{v2_version}",
    "cookbook_name" => COOKBOOK,
    "version" => v2_version,
    "metadata" => v2_metadata,
    "all_files" => all_files,
  }
  File.write(
    File.join(ARTIFACTS, "v2-manifest.json"),
    JSON.pretty_generate(v2_manifest) + "\n"
  )
  expect!(
    "PUT", "/organizations/#{ORG}/cookbooks/#{COOKBOOK}/#{v2_version}",
    v2_manifest,
    allowed: [200, 201]
  )

  solve_status, solve_body = solve_as_client(client_key)
  solution = solve_body.empty? ? {} : JSON.parse(solve_body)
  CAPABILITIES["v2_solve_status_after_synthesis"] = solve_status
  CAPABILITIES["v2_all_files_after_synthesis"] = v2_ready?(solution)
  CAPABILITIES["v2_synthesized"] = true
  CAPABILITIES["cookbook_version_used"] = solution.dig(COOKBOOK, "version")
  File.write(
    File.join(ARTIFACTS, "v2-solve-after.json"),
    JSON.pretty_generate({ "status" => solve_status, "body" => solution }) + "\n"
  )
  warn "bootstrap: server gap: API v2 cookbook_versions response had no all_files; uploaded a synthesized v2 manifest #{COOKBOOK}-#{v2_version}"
elsif solve_status == 200 && !v2_ready?(solution) && COMPAT
  warn "bootstrap: server gap: API v2 cookbook_versions response had no all_files; v2 synthesis is disabled (MW_E2E_SYNTH_V2=0)"
end

if solve_status != 200
  warn "bootstrap: cookbook solve as client returned #{solve_status}; chef-client is expected to fail"
  warn solve_body
end

# 8. Chef client configuration and JSON attributes (run list). The run list
#    must come from -j/attributes rather than --override-runlist: chef-client
#    skips the final node save for a temporary (override) policy.
config = <<~RUBY
  chef_server_url #{(SERVER + "/organizations/#{ORG}").inspect}
  node_name #{NODE_NAME.inspect}
  client_key #{client_key_path.inspect}
  validation_client_name #{validator_name.inspect}
  validation_key #{File.join(ARTIFACTS, "keys", "validator.pem").inspect}
  ssl_verify_mode :verify_none
  file_cache_path #{File.join(ARTIFACTS, "cache").inspect}
  file_backup_path #{File.join(ARTIFACTS, "backup").inspect}
  checksum_path #{File.join(ARTIFACTS, "checksums").inspect}
  http_retry_count 1
  log_level :info
RUBY
config_path = File.join(ARTIFACTS, "client.rb")
File.write(config_path, config)

# A second config exercises the validator registration path: client_key does
# not exist yet, so chef-client POSTs /clients signed by the validator.
validator_config = <<~RUBY
  chef_server_url #{(SERVER + "/organizations/#{ORG}").inspect}
  node_name #{(NODE_NAME + "-validator").inspect}
  client_key #{File.join(ARTIFACTS, "keys", "registered-client.pem").inspect}
  validation_client_name #{validator_name.inspect}
  validation_key #{File.join(ARTIFACTS, "keys", "validator.pem").inspect}
  ssl_verify_mode :verify_none
  file_cache_path #{File.join(ARTIFACTS, "cache-validator").inspect}
  file_backup_path #{File.join(ARTIFACTS, "backup-validator").inspect}
  checksum_path #{File.join(ARTIFACTS, "checksums-validator").inspect}
  http_retry_count 1
  log_level :info
RUBY
File.write(File.join(ARTIFACTS, "client-validator.rb"), validator_config)

node_json = { "run_list" => ["recipe[#{COOKBOOK}]"] }
node_json_path = File.join(ARTIFACTS, "node.json")
File.write(node_json_path, JSON.pretty_generate(node_json))

write_capabilities
puts "bootstrap: org=#{ORG} org_id=#{org_id} validator=#{validator_name}"
puts "bootstrap: client=#{NODE_NAME} marker=#{MARKER}"
puts "bootstrap: capabilities=#{JSON.generate(CAPABILITIES)}"
puts "bootstrap: config=#{config_path} node_json=#{node_json_path}"
