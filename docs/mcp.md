# MCP endpoint (experimental)

Microwave Server can expose a per-organization
[MCP](https://modelcontextprotocol.io/) endpoint over the Streamable HTTP
transport. It lets an MCP-capable agent list and, optionally, mutate Chef
objects and read locally stored converge reports through tool calls instead of
the signed Chef HTTP API.

The surface is **experimental**. It has no persistent audit table, no token
expiry, no token scopes, and no per-tool permissions. Read
[Limitations](#limitations) before enabling it on an internet-facing
deployment.

## Enablement

The endpoint is disabled by default. It is mounted only when `mcp.enabled` is
true:

```toml
[mcp]
enabled = true
```

```
POST /organizations/<org>/mcp
```

The route is registered for all methods and the Streamable HTTP transport
handles them; clients normally send JSON-RPC over `POST`. Requests are
stateless (`with_legacy_session_mode(false)`) and answered as JSON
(`with_json_response(true)`), so there is no session to keep alive between
calls and no `Mcp-Session-Id` affinity.

This path bypasses the Chef signed-request middleware. The bearer token
described below is the only credential accepted on it; `X-Ops-Sign`
signatures are not used.

The endpoint does not exist while `mcp.enabled = false` (the path returns
`404`), and it does not exist for an organization without an
`[mcp.organizations.<org>]` entry.

## Authentication and principals

Authentication is a static bearer token per organization:

```
Authorization: Bearer <token>
```

Missing or incorrect credentials return `401` with
`WWW-Authenticate: Bearer`. An unknown organization, or an organization
without an MCP entry, returns `404`. A configured principal that does not
exist or is not a member of the organization returns `500`; the server logs
the reason, and the client only sees the generic error.

A token is only valid for its organization's path. The token for organization
A cannot read organization B.

### Token sources

The server uses the first configured source:

1. `token` in `[mcp.organizations.<org>]` in the configuration file.
2. The `mcp` secret group, one secret per organization:

   ```sh
   microwave-ctl --config /etc/microwave/microwave.toml \
     set-secret mcp acme <token>
   ```

A configuration-file token is read at startup, so changing or removing it
requires a server restart. The secret is read per request, so rotating it with
`set-secret` (or `remove-secret`) takes effect immediately. Prefer the secret;
if a token is kept in the file, keep the file `0640`, owned by the service
user.

An organization entry with neither an inline token nor a stored secret still
answers `401` and logs a warning; it never falls through to unauthenticated
access.

The CLI accepts an organization name as the secret name (`set-secret mcp
<org> <token>`). `show-secret mcp <org>` prints the stored value.

### Principal and ACLs

Each organization entry names the user the token acts as:

```toml
[mcp.organizations.acme]
principal = "mcp-agent"   # optional
```

`principal` defaults to `auth.superuser_name` (by default `pivotal`). That
principal is treated as a platform superuser and **bypasses ACL checks** for
every tool. This is convenient for a first test and wrong for a shared or
production deployment.

For a scoped identity, set `principal` to a non-superuser user who is a member
of the organization and grant that user only the project ACLs the agent needs.
Non-superuser tool behavior:

- `list_*` tools hide objects the principal cannot read.
- `get_*` tools return a tool error (`isError: true`) with `forbidden` or
  `not found`.
- Runs and converge errors inherit the ACL of their node. Runs of nodes that no
  longer exist are visible to superusers only.

The token has no expiry and no scopes. Rotate it by replacing the secret, or
by editing the config and restarting.

## Write access

`writes` defaults to `true`, which registers the eight write tools. They call
the same services as the signed HTTP API and still enforce the principal's
ACLs; `writes` is a capability switch, not an extra authorization layer.

```toml
[mcp.organizations.acme]
principal = "mcp-agent"
writes = false
```

With `writes = false`, the write tools are absent from `tools/list` and calls
to them fail as unknown tools. Read tools are always registered.

> **Destructive surface.** When `writes = true`, `delete_node`,
> `delete_environment`, and `delete_cookbook_version` are available to the
> agent. There is no dry-run and no confirmation prompt. Point experimental
> agents at a non-production organization and give them a dedicated,
> non-superuser principal.

## Host and origin checks

The Streamable HTTP transport rejects non-loopback `Host` headers by default
(DNS-rebinding protection). A remote agent therefore needs an allowlist:

```toml
[mcp]
enabled = true
allowed_hosts = ["chef.example.com", "chef.example.com:8443"]
```

When `allowed_hosts` is empty, the server derives the host and, when the URL
has an explicit port, the `host:port` variant from `server.public_url`.
Configure `allowed_hosts` directly when the agent reaches the server under a
different name than `public_url`, or when `public_url` is unset. Without
either setting the endpoint is reachable only with a loopback `Host`.

`allowed_origins` is optional and only relevant to browser-based clients. An
empty list disables Origin validation, which is the intended setting for
non-browser agents. Set it explicitly when a browser client calls the
endpoint:

```toml
[mcp]
allowed_origins = ["https://app.example.com"]
```

A reverse proxy should also restrict this path if the server is
internet-facing.

## Configuration reference

All keys are optional. Defaults are the built-in values.

| TOML key | Environment | Default | Notes |
|---|---|---|---|
| `mcp.enabled` | `MW_MCP__ENABLED` | `false` | Mounts `POST /organizations/<org>/mcp`. |
| `mcp.max_results` | `MW_MCP__MAX_RESULTS` | `100` | Clamped to `1`-`1000`. Caps every list tool's `limit`. |
| `mcp.max_result_bytes` | `MW_MCP__MAX_RESULT_BYTES` | `1048576` (1 MiB) | Clamped to 1 KiB-16 MiB. A larger serialized result is an error, never truncated. |
| `mcp.max_upload_bytes` | `MW_MCP__MAX_UPLOAD_BYTES` | `10485760` (10 MiB) | Clamped to 1 byte-250 MiB. Caps decoded cookbook upload bytes. |
| `mcp.allowed_hosts` | `MW_MCP__ALLOWED_HOSTS` (comma-separated) | empty | Empty derives host variants from `server.public_url`. |
| `mcp.allowed_origins` | `MW_MCP__ALLOWED_ORIGINS` (comma-separated) | empty | Empty disables Origin checks. |
| `mcp.organizations` | none | empty | Map keyed by organization name. |
| `mcp.organizations.<org>.token` | none | unset | Inline bearer token; wins over the secret. Requires a restart to change. |
| `mcp.organizations.<org>.principal` | none | `auth.superuser_name` | User the token acts as. |
| `mcp.organizations.<org>.writes` | none | `true` | `false` hides and rejects the write tools. |

Per-organization entries have no environment override. Bare TOML keys accept
`-` and `_`, so `[mcp.organizations.team-1]` is a valid table key for the
organization `team-1`.

The transport caps request bodies at `max_upload_bytes * 4 / 3 + 1 MiB` and
answers an oversized body with `413`. The body must fit the base64 encoding of
the tool arguments, not just the decoded files.

Validate a changed file before restarting:

```sh
microwave-ctl --config /etc/microwave/microwave.toml check-config
```

## Client configuration

Most MCP clients accept a JSON server entry with a URL and static headers.
Substitute the organization name and token:

```json
{
  "mcpServers": {
    "microwave-acme": {
      "type": "http",
      "url": "https://chef.example.com/organizations/acme/mcp",
      "headers": {
        "Authorization": "Bearer <token>"
      }
    }
  }
}
```

The bearer header must be sent on **every** request; the endpoint keeps no
server-side session that could remember it. A client that insists on the
stdio transport cannot use this endpoint.

The same call by hand, first `initialize` and then `tools/list`:

```sh
curl -sS https://chef.example.com/organizations/acme/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Authorization: Bearer <token>' \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2025-03-26",
      "capabilities": {},
      "clientInfo": { "name": "curl", "version": "0" }
    }
  }'
```

```sh
curl -sS https://chef.example.com/organizations/acme/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Authorization: Bearer <token>' \
  -d '{ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }'
```

Tool calls use the regular JSON-RPC shape:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "tools/call",
  "params": {
    "name": "list_nodes",
    "arguments": { "query": "web", "limit": 20 }
  }
}
```

## Tool reference

The endpoint exposes 11 read tools and, when `writes = true`, 8 write tools
(19 total). Tool results are JSON text content. Expected failures set
`isError: true`.

### Read tools

Always registered.

| Tool | Arguments | Result |
|---|---|---|
| `list_nodes` | `query?`, `environment?`, `limit?` | Name, environment, policy name/group, `updated_at`. `query` is a case-insensitive substring match on the node name. |
| `get_node` | `name` | Full node object. |
| `list_environments` | `limit?` | Name and `updated_at`. |
| `get_environment` | `name` | Full environment object. |
| `list_cookbooks` | `query?`, `limit?` | Name, versions, newest version. `query` is a case-insensitive substring match on the cookbook name. |
| `list_cookbook_versions` | `name` | Version, frozen flag, `updated_at`. |
| `get_cookbook_version` | `name`, `version` | Rendered manifest, dependencies, frozen flag. |
| `list_runs` | `node_name?`, `status?`, `failures_only?`, `since?`, `limit?` | Run id, node, status, start/end time, resource and error counts, newest first. |
| `latest_run` | `node_name` | Newest run summary plus its top-level error and resource errors. |
| `get_run` | `node_name`, `run_id` | Full stored report (resources and errors). |
| `list_converge_errors` | `node_name?`, `since?`, `limit?` | Run id, node, time, resource type/name (`null` for a top-level error), error class/message. |

`node_name` on `list_runs` and `list_converge_errors` is optional; without it
the tools scan every node the principal can read. `since` must be an RFC 3339
timestamp.

### Write tools

Registered only when the organization has `writes = true`.

| Tool | Arguments | Semantics |
|---|---|---|
| `create_node` | `name`, `environment?`, `run_list?`, `normal?`, `policy_name?`, `policy_group?` | Builds a node document and runs the create path (container `nodes` Create ACL). |
| `update_node` | `name`, `node` (full document) | Same normalization and validation as `PUT /nodes/:name`. Node Update ACL. This is a replacement, not JSON patch. |
| `delete_node` | `name` | Node Delete ACL. |
| `create_environment` | `name`, `attributes?`, `cookbook_versions?` | Container `environments` Create ACL. |
| `update_environment` | `name`, `environment` (full document), `new_name?` | Environment Update ACL. Replacement, not JSON patch; `new_name` renames. |
| `delete_environment` | `name` | Environment Delete ACL. `_default` is rejected like the HTTP API. |
| `upload_cookbook_version` | `name`, `version`, `metadata`, `files`, `frozen?`, `force?` | Uploads inline or pre-existing checksum files, then runs the normal create/update path. See [Cookbook upload](#cookbook-upload). |
| `delete_cookbook_version` | `name`, `version` | Cookbook version Delete ACL. |

Write tools return a compact summary rather than the full object:

- node: `{ "name", "environment", "policy_name", "policy_group" }`
- environment update: `{ "name", "updated_at", "renamed" }` (create omits
  `renamed`)
- delete node or environment: `{ "name", "deleted": true }`
- cookbook upload: `{ "name", "version", "frozen" }`
- cookbook delete: `{ "name", "version", "deleted": true }`

Fetch the full object with the corresponding read tool.

## Result limits and errors

Every list tool clamps `limit` to `mcp.max_results` (default 100). A missing
`limit` uses `max_results`; values below 1 are raised to 1.

Every tool serializes its result and checks `mcp.max_result_bytes`. An
oversized result returns a tool error with `result exceeds
max_result_bytes`; results are never silently truncated. Raise the limit or
ask for fewer rows. `get_run` on a large report is the usual case.

Error text is stable:

| Condition | Tool error text |
|---|---|
| `400`, `409`, or `422` from the shared service | The HTTP message is kept. |
| `403` | `forbidden` |
| `404` | `not found` |
| `5xx` | Logged with tool context; the caller sees `internal error`. |
| Invalid argument, including a non-RFC 3339 `since` | The validation message. |

The transport handles protocol errors (malformed JSON-RPC, unsupported
method, oversized body with `413`). Tool failures are normal JSON-RPC results
with `isError: true`, so a client must inspect that flag.

## Cookbook upload

`upload_cookbook_version` collapses the HTTP sandbox/bookshelf flow into one
call. Files are either inline base64 or references to blobs that already
exist in the organization:

```json
{
  "name": "nginx",
  "version": "1.2.0",
  "metadata": { "dependencies": { "apt": ">= 7.0.0" } },
  "frozen": false,
  "files": [
    { "path": "recipes/default.rb", "content_base64": "..." },
    {
      "path": "templates/default/nginx.conf.erb",
      "content_base64": "...",
      "specificity": "default"
    }
  ]
}
```

Rules:

- At most 100 files per call.
- The decoded total must not exceed `mcp.max_upload_bytes`; the HTTP body cap
  is derived from the same value.
- Invalid base64 fails the call.
- Each file supplies either `content_base64` (the server computes the MD5 and
  stores the blob) or `checksum`: a lowercase 32-character hex MD5 that must
  already exist in the organization. The checksum form supports an
  out-of-band upload, for example the sandbox/bookshelf flow.
- `name` defaults to `path`, and `specificity` defaults to `default`.
- `metadata.version` is injected from the top-level `version`.
- The manifest's `frozen?` comes from `frozen`.
- Updating an existing frozen version requires `force: true`. A forced update
  keeps `frozen` sticky: frozen stays frozen.

Blobs are written before the database row. A store or ACL failure can
therefore leave content-addressed orphan blobs behind; a later upload of the
same checksum reuses them. This matches the existing sandbox/bookshelf
behavior and is a v1 limitation.

## Limitations

- **Experimental.** The tool names, arguments, and behavior may change.
- **No audit table and no per-call audit log.** Authentication failures and
  internal tool errors are logged, but there is no durable, queryable record
  of successful calls, the tool that ran, or a write target.
- **No token expiry, scopes, or per-tool permissions.** Authorization is the
  organization-level `writes` switch plus the principal's ACLs.
- **No stdio transport.** Only Streamable HTTP is implemented.
- **No server-side session state.** Clients must send the bearer header on
  every request.
- **Run reports are local-only.** `node_runs` is excluded from snapshots and
  from `microwave-migrate`. The run tools only see reports stored by this
  server, which requires `data_collector.store = "local"` or `"both"`. With
  the default `forward` mode the run tools return no data.
- **Orphaned blobs.** A failed or denied upload can leave blobs that no
  cookbook version references (see above); there is no garbage collector.
- **No signature fallback.** `/organizations/<org>/mcp` bypasses the Chef
  signed-request middleware. The bearer token is the only credential on that
  path, so protect it like any other long-lived credential and restrict the
  route at the reverse proxy where possible.

The `data_collector.store` setting itself is documented in
[`docs/deployment.md`](deployment.md#keys-and-environment-overrides) and the
annotated example configuration.
