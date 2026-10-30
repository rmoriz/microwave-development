# MCP endpoint (experimental)

Microwave Server can expose a per-organization
[MCP](https://modelcontextprotocol.io/) endpoint over the Streamable HTTP
transport. It lets an MCP-capable agent list and, optionally, mutate Chef
objects and read locally stored converge reports through tool calls instead of
the signed Chef HTTP API.

The surface is **experimental**. It has no persistent audit table
([gap-mcp-audit](remaining-work.md#gap-mcp-audit)), no token expiry, no token
scopes, and no per-tool permissions. Read
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
exist returns `500`; a non-superuser principal that is not a member of the
organization also returns `500`. The configured superuser skips the
membership check. The server logs the reason, and the client only sees the
generic error.

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

- `list_*` tools hide objects the principal cannot read. An explicit
  `node_name` on `list_runs` or `list_converge_errors` that the principal
  cannot read is a `forbidden` tool error, not an empty list.
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

When `mcp.enabled` is true, the server logs a startup warning for every
organization whose `principal` is unset (or names `auth.superuser_name`) while
`writes = true`. The defaults are unchanged; the warning exists because that
combination gives a bearer token full destructive superuser capability.

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

When `allowed_hosts` is empty, the server derives the host from
`server.public_url`. It also derives the `host:port` variant only when the URL
has a non-default explicit port (for example `https://host:8443`), because
`url::Url::port()` is `None` for the scheme's default port (`:443`).
Configure `allowed_hosts` directly when the agent reaches the server under a
different name than `public_url`, or when `public_url` is unset. Without
either setting the endpoint is reachable only with a loopback `Host`.

`allowed_origins` is optional and only validates the `Origin` header. The
transport does not emit `Access-Control-Allow-*` response headers, so a
cross-origin browser client still needs CORS handling from the reverse proxy.
Same-origin browser clients work without it. An empty list disables Origin
validation, which is the intended setting for non-browser agents. Set it
explicitly when a browser client calls the endpoint:

```toml
[mcp]
allowed_origins = ["https://app.example.com"]
```

A reverse proxy should also restrict this path if the server is
internet-facing.

## Configuration reference

All keys are optional. Defaults are the built-in values. Each row shows the
TOML key and, underneath, the `MW_*` variable that overrides it: `MW_` plus the
key with every level separated by `__` and the segments upper-cased. Rows
without a variable are TOML-only.

#### Enablement and principals

Mounting the endpoint and deciding which user a tool call runs as. See
[Enablement](#enablement) and [Principal and ACLs](#principal-and-acls).

| Setting | Details |
|---|---|
| `mcp.enabled`<br/>`MW_MCP__ENABLED` | Default `false`. Mounts `POST /organizations/<org>/mcp`. |
| `mcp.organizations.<org>.token` | Unset by default. Inline bearer token; wins over the secret. Requires a restart to change. |
| `mcp.organizations.<org>.principal` | Default `auth.superuser_name`. User the token acts as. |
| `mcp.organizations.<org>.writes` | Default `true`. `false` hides and rejects the write tools; see [Write access](#write-access). |

#### Limits

Bounds are clamped by the server and enforced before a response is written;
[Result limits and errors](#result-limits-and-errors) has the failure
behaviour.

| Setting | Details |
|---|---|
| `mcp.max_results`<br/>`MW_MCP__MAX_RESULTS` | Default `100`. Clamped to `1`-`1000`. Caps the `limit` of every list tool that accepts one. |
| `mcp.max_result_bytes`<br/>`MW_MCP__MAX_RESULT_BYTES` | Default `1048576` (1 MiB). Clamped to 1 KiB-16 MiB. A larger serialized result is an error, never truncated. |
| `mcp.max_upload_bytes`<br/>`MW_MCP__MAX_UPLOAD_BYTES` | Default `10485760` (10 MiB). Clamped to 1 byte-250 MiB. Caps decoded cookbook upload bytes; see [Cookbook upload](#cookbook-upload). |

#### Host and origin checks

Both lists are comma-separated and drive the checks described in
[Host and origin checks](#host-and-origin-checks).

| Setting | Details |
|---|---|
| `mcp.allowed_hosts`<br/>`MW_MCP__ALLOWED_HOSTS` | Empty by default. Empty derives host variants from `server.public_url`. |
| `mcp.allowed_origins`<br/>`MW_MCP__ALLOWED_ORIGINS` | Empty by default. Empty disables Origin checks. |

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

### Claude Desktop

`claude_desktop_config.json` starts stdio servers only, so this endpoint needs
the `mcp-remote` bridge. Add the entry to the file (macOS:
`~/Library/Application Support/Claude/claude_desktop_config.json`) and restart
Claude Desktop:

```json
{
  "mcpServers": {
    "microwave-acme": {
      "command": "npx",
      "args": [
        "-y",
        "mcp-remote",
        "https://chef.example.com/organizations/acme/mcp",
        "--header",
        "Authorization:${MICROWAVE_MCP_TOKEN}"
      ],
      "env": { "MICROWAVE_MCP_TOKEN": "Bearer <token>" }
    }
  }
}
```

The spaces around the colon in `--header` are omitted on purpose; several
clients mangle arguments that contain spaces. If your Claude Desktop version
supports remote connectors, you can add the URL there instead and let it hold
the header.

### Cursor

Add the entry to the project `.cursor/mcp.json` or the user-level
`~/.cursor/mcp.json`, then reload Cursor:

```json
{
  "mcpServers": {
    "microwave-acme": {
      "url": "https://chef.example.com/organizations/acme/mcp",
      "headers": { "Authorization": "Bearer <token>" }
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
|---|---|
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

`status` matches the stored Chef status exactly (`success` or `failure`;
ingestion stores `unknown` when the payload omits it). `failures_only` keeps
runs whose stored status is not `success` or whose error count is non-zero.
`limit` on `list_converge_errors` bounds the number of runs that have errors,
not the number of emitted error rows, so older failures are not hidden behind
newer clean runs.

### Write tools

Registered only when the organization has `writes = true`.

| Tool | Arguments | Semantics |
|---|---|
| `create_node` | `name`, `environment?`, `run_list?`, `normal?`, `policy_name?`, `policy_group?` | Builds a node document and runs the create path (container `nodes` Create ACL). |
| `update_node` | `name`, `node` (full document) | Same normalization and validation as `PUT /nodes/:name`. Node Update ACL. This is a replacement, not JSON patch. |
| `delete_node` | `name` | Node Delete ACL. |
| `create_environment` | `name`, `attributes?`, `cookbook_versions?` | Container `environments` Create ACL. |
| `update_environment` | `name`, `environment` (full document), `new_name?` | Environment Update ACL. Replacement, not JSON patch; `new_name` renames. |
| `delete_environment` | `name` | Environment Delete ACL. `_default` is rejected like the HTTP API. |
| `upload_cookbook_version` | `name`, `version`, `metadata`, `files`, `frozen?`, `force?` | Uploads inline or pre-existing checksum files, then runs the normal create/update path. See [Cookbook upload](#cookbook-upload). |
| `delete_cookbook_version` | `name`, `version` | Cookbook version Delete ACL. |

`create_environment` accepts only an `attributes` object with optional
`default` and `override` maps; any other key is rejected as an invalid
argument:

```json
{
  "name": "prod",
  "attributes": {
    "default": { "app": { "port": 80 } },
    "override": { "app": { "port": 8080 } }
  },
  "cookbook_versions": { "apache": "= 1.0.0" }
}
```

Write tools return a compact summary rather than the full object:

- node: `{ "name", "environment", "policy_name", "policy_group" }`
- environment update: `{ "name", "updated_at", "renamed" }` (create omits
  `renamed`)
- delete node or environment: `{ "name", "deleted": true }`
- cookbook upload: `{ "name", "version", "frozen" }`
- cookbook delete: `{ "name", "version", "deleted": true }`

Fetch the full object with the corresponding read tool.

## Result limits and errors

Every list tool that accepts `limit` clamps it to `mcp.max_results` (default
100). A missing `limit` uses `max_results`; values below 1 are raised to 1.
`list_cookbook_versions` has no `limit` and returns every readable version.

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
| `5xx` | The error is logged; the caller sees `internal error`. |
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
- `name` and `path` in the manifest are both set from `path`; `specificity`
  defaults to `default`.
- `metadata.version` is injected from the top-level `version`.
- The manifest's `frozen?` comes from `frozen`.
- Updating an existing frozen version requires `force: true`. A forced update
  keeps `frozen` sticky: frozen stays frozen.

Blobs are written before the database row. A store or ACL failure can
therefore leave content-addressed orphan blobs behind; a later upload of the
same checksum reuses them. This matches the existing sandbox/bookshelf
behavior and is a v1 limitation.

## Verify it works

1. Run `microwave-ctl --config /etc/microwave/microwave.toml check-config`.
2. Restart the server, connect the client, and call `tools/list`. With
   `writes = false`, only the 11 read tools appear; with `writes = true`, all 19
   tools appear.
3. Call `list_nodes` as the configured principal. A scoped non-superuser sees
   only nodes its ACLs allow.
4. If the run tools return nothing, check that `data_collector.store` is
   `"local"` or `"both"` and that reports have been received since enablement.
   Reports are pruned to `data_collector.keep_runs` per organization and node
   (default 50); `0` keeps everything at the cost of database growth. Run tools
   never see reports older than the retained window.
5. Tail the server log and confirm one structured log line per call, including the
   organization, principal, tool, duration, result count, and outcome; write
   calls also log the target. Server failures add a separate error line.

## Limitations

- **Experimental.** The tool names, arguments, and behavior may change.
- **No durable audit table.** Tool calls log one structured log line per call with
  the organization, principal, tool, duration, result count, and outcome;
  writes also log the target, and server failures log a separate error line.
  Logs go to the server log (journald or the container log), are not stored in
  the database, and are not a queryable audit record.
- **No token expiry, scopes, or per-tool permissions.** Authorization is the
  organization-level `writes` switch plus the principal's ACLs.
- **No stdio transport.** Only Streamable HTTP is implemented.
- **No server-side session state.** Clients must send the bearer header on
  every request.
- **Run reports are local-only.** `node_runs` is excluded from snapshots and
  from `microwave-migrate`. The run tools only see reports stored by this
  server, which requires `data_collector.store = "local"` or `"both"`. With
  the default `forward` mode the run tools return no data. Retained reports are
  pruned to `keep_runs` per organization and node (default 50).
- **Orphaned blobs.** A failed or denied upload can leave blobs that no
  cookbook version references (see above); there is no garbage collector.
- **No signature fallback.** `/organizations/<org>/mcp` bypasses the Chef
  signed-request middleware. The bearer token is the only credential on that
  path, so protect it like any other long-lived credential and restrict the
  route at the reverse proxy where possible.

The `data_collector.store` setting itself is documented in
[`docs/deployment.md`](deployment.md#keys-and-environment-overrides) and the
annotated example configuration.
