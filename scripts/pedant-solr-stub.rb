#!/usr/bin/env ruby
# frozen_string_literal: true

# Minimal stand-in for a Solr commit endpoint.
#
# oc-chef-pedant's search specs call `force_solr_commit` unconditionally in a
# few places (notably word_break_spec.rb) and expect to POST
# `<search_server>/update?commit=true`.  Microwave's search index is updated
# synchronously, so there is nothing to commit; this stub just answers 200 so
# the specs can proceed to exercise the search API itself.
#
# Usage: pedant-solr-stub.rb PORT

require "socket"

port = Integer(ARGV.fetch(0))
server = TCPServer.new("127.0.0.1", port)
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

      body = '{"responseHeader":{"status":0}}'
      socket.write(
        "HTTP/1.1 200 OK\r\n" \
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
