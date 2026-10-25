#!/usr/bin/env ruby
# frozen_string_literal: true

# Minimal stand-in for the external Solr/OpenSearch index that Pedant talks to.
#
# oc-chef-pedant's search specs call `force_solr_commit` unconditionally in a
# few places (notably word_break_spec.rb) and expect to POST
# `<search_server>/update?commit=true`.  They also verify index deletion with
# `direct_solr_query`, which asks `<search_server>/select` whether a document
# is still present.
#
# Microwave's default search backend is embedded: the relational store is the
# source of truth and the search API reads it directly.  There is no external
# index to commit to.  This stub therefore:
#
#   * answers `/update?commit=true` with 200 (nothing to commit), and
#   * translates the narrow `/select` query used by Pedant into a call to the
#     live search API, so the deletion examples verify the embedded index.
#
# It is test harness scaffolding, not a general Solr implementation.
#
# Usage: pedant-solr-stub.rb PORT SERVER_URL SUPERUSER_KEY [ORG]

require "cgi"
require "json"
require "net/http"
require "openssl"
require "socket"
require "time"
require "uri"
require "mixlib/authentication/signedheaderauth"

PORT = Integer(ARGV.fetch(0))
SERVER = ARGV.fetch(1)
SUPERUSER_KEY_PATH = ARGV.fetch(2)
ORG = ARGV.fetch(3, "pedant_testorg")
SUPERUSER_NAME = ENV.fetch("PEDANT_SUPERUSER_NAME", "pivotal")
API_VERSION = ENV.fetch("PEDANT_API_VERSION", "0")

SUPERUSER_KEY = OpenSSL::PKey::RSA.new(File.read(SUPERUSER_KEY_PATH))

# Calls the search API as the superuser.  Direct index checks are not
# authorization tests; the search ACL examples exercise the user-facing API.
def search(index, query)
  uri = URI.join(SERVER, "/organizations/#{ORG}/search/#{index}?q=#{URI.encode_www_form_component(query)}")
  signing = Mixlib::Authentication::SignedHeaderAuth.signing_object(
    http_method: "GET",
    path: uri.path,
    body: "",
    timestamp: Time.now.utc.iso8601,
    user_id: SUPERUSER_NAME
  )
  headers = {
    "Accept" => "application/json",
    "Content-Type" => "application/json",
    "Host" => "#{uri.host}:#{uri.port}",
    "X-Chef-Version" => "18.10.17",
    "X-Ops-Server-API-Version" => API_VERSION,
  }.merge(signing.sign(SUPERUSER_KEY))

  http = Net::HTTP.new(uri.host, uri.port)
  http.use_ssl = uri.scheme == "https"
  http.verify_mode = OpenSSL::SSL::VERIFY_NONE
  response = http.request(Net::HTTP::Get.new(uri.request_uri, headers))
  [response.code.to_i, JSON.parse(response.body)]
end

# Pedant's only direct query is `content:name__=__<exact name>`.  Its filter
# query carries the object type (and the data bag name for items).
def solr_select(params)
  filter = params.fetch("fq", []).first.to_s
  query = params.fetch("q", []).first.to_s

  type = filter[/X_CHEF_type_CHEF_X:([^\s+]+)/, 1]
  index =
    if type == "data_bag_item"
      filter[/data_bag:([^\s+]+)/, 1]
    else
      type
    end
  name = query.split("__=__", 2)[1]
  return [400, { "error" => "unsupported select query" }] unless index && name

  status, body = search(index, "name:#{name}")
  return [status, body] unless status == 200

  rows = body["rows"] || []
  total = body["total"] || rows.length
  [
    200,
    {
      "responseHeader" => { "status" => 0, "QTime" => 0 },
      "response" => { "numFound" => total, "start" => 0, "docs" => rows },
    },
  ]
rescue StandardError => e
  [502, { "error" => "#{e.class}: #{e.message}" }]
end

server = TCPServer.new("127.0.0.1", PORT)
$stdout.puts server.addr[1]
$stdout.flush

loop do
  client = server.accept
  Thread.new(client) do |socket|
    begin
      request_line = socket.gets
      headers = {}
      while (line = socket.gets) && line != "\r\n"
        name, value = line.split(":", 2)
        headers[name.downcase] = value.strip if value
      end
      length = headers.fetch("content-length", "0").to_i
      socket.read(length) if length.positive?

      method, target = request_line.to_s.split(" ", 3)
      uri = URI.parse(target.to_s)
      if method == "POST" && uri.path == "/update"
        status = 200
        body = '{"responseHeader":{"status":0}}'
      elsif method == "GET" && uri.path == "/select"
        status, payload = solr_select(CGI.parse(uri.query.to_s))
        body = JSON.generate(payload)
      else
        status = 404
        body = '{"error":"not found"}'
      end

      reason = status == 200 ? "OK" : (status == 400 ? "Bad Request" : "Error")
      socket.write(
        "HTTP/1.1 #{status} #{reason}\r\n" \
        "Content-Type: application/json\r\n" \
        "Content-Length: #{body.bytesize}\r\n" \
        "Connection: close\r\n\r\n#{body}"
      )
      warn "solr-stub: #{request_line&.strip}" if ENV["PEDANT_SOLR_STUB_VERBOSE"]
    rescue StandardError => e
      warn "solr-stub: #{e.class}: #{e.message}"
    ensure
      socket.close rescue nil
    end
  end
end
