# Experimental MCP server and local run reports

Date: 2026-10-27
Status: approved (2026-10-27)
Approach: C — HTTP MCP endpoint inside the server

## Intent

Operators currently have to read `journalctl` or open an external Automate
instance to find out why a node failed to converge. Agents have no supported
way to ask the server what happened on a node, which cookbooks exist, or which
resources failed — and no supported way to act on that information.

This design adds two related pieces:

1. **Local run reports** — the existing data-collector endpoint can optionally
   persist Chef Infra Client reports in the metadata database (SQLite or
   PostgreSQL), either instead of or in addition to forwarding them to
   Automate.
2. **An experimental MCP server** — a Streamable HTTP endpoint per organization
   that exposes the stored runs, converge errors, nodes, environments, and
   cookbooks as MCP tools, including write tools for node, environment, and
   cookbook management.

The feature is opt-in. Read and write access are governed by per-organization
bearer tokens mapped to a Chef principal, and every write uses the existing
ACL, normalization, and indexing paths.

## Why now

- The data-collector endpoint already receives exactly the payload that
  contains run status and resource errors; it currently discards it after
  forwarding.
- The store abstraction already supports SQLite and PostgreSQL behind one
  trait, so report persistence fits the existing backend matrix.
- `rmcp` 3.8 (the official Rust MCP SDK) provides a Streamable HTTP Tower
  service, so an MCP endpoint can be mounted under the existing axum router.
- Node, environment, and cookbook mutations already exist as small handler
  functions that can be extracted into shared services, so HTTP and MCP can use
  the same code instead of duplicating ACL and indexing logic.

## Goals

- Optional local persistence of run reports in SQLite and PostgreSQL.
- Configurable combination of local storage and Automate forwarding
  (`forward`, `local`, `both`), with today's behavior as the default.
- Bounded growth: 50 runs per node by default, configurable, `0` disables
  pruning.
- An HTTP MCP endpoint under `/organizations/{organization}/mcp` with
  per-organization bearer authentication and an org-bound principal.
- Read tools: nodes, environments, cookbooks, runs, converge errors.
- Write tools: create/update/delete nodes, create/update/delete environments,
  upload/delete cookbook versions.
- Every write goes through the same ACL checks, normalization, validation, and
  search indexing as the signed HTTP API.
- Tests against both storage backends and end-to-end HTTP tests of the MCP
  endpoint, including write authorization.

## Non-goals

- A signed-request (Chef key) MCP client. Agents send a bearer token; machine
  clients can be added later.
- Streaming/long-running MCP tasks, subscriptions, prompts, or resources.
  Only tools are exposed.
- Per-tool scopes, token expiry, rotation UI, or per-agent identity. Tokens are
  static experimental credentials with the full capability of their principal.
- A persistent audit table. Writes are logged as structured log lines; the
  existing database has no audit-log surface to extend.
- Querying an external Automate/data collector. The local store is the source
  for reports.
- Search-index integration for reports. Reports are queried from the metadata
  store, not the embedded/OpenSearch index. `query` on nodes and cookbooks is a
  case-insensitive substring filter over the metadata rows, not Chef search
  syntax.
- Including `node_runs` in the logical store snapshot / `microwave-migrate`.
  Reports are experimental and can be large; export/import keeps ignoring them
  in v1, and `docs/mcp.md` states this explicitly.
- A UI, retention UI, or per-run annotations.
- Policy, policy-group, role, group, container, client, key, or data-bag write
  tools. The write surface is deliberately limited to nodes, environments, and
  cookbook versions.
- Garbage collection of cookbook blobs orphaned by a failed upload. The
  existing bookshelf behavior has the same property; a follow-up can add
  cleanup.

## Anti-goals

- Changing the default data-collector behavior. An upgrade must not start
  storing reports unless configured.
- Making the MCP endpoint reachable without a valid per-organization token.
- Letting a token for organization A act on organization B.
- Letting a non-superuser principal read or write objects its Chef ACLs deny.
- Breaking `POST /organizations/:org/data-collector` for existing chef-client
  configurations that send only `x-data-collector-token`.
- Unbounded database growth from reports.
- Duplicating node/environment/cookbook mutation logic in the MCP layer.

## Constraints

- One metadata contract: the new report methods live in a `ReportRepository`
  trait; SQLite and PostgreSQL implement it.
- The store must keep working when the report feature is disabled: no new
  required methods on `MetadataStore` that break test doubles.
- Reports arrive at the existing endpoint and must keep the same
  authentication rules (shared token or Chef signature).
- The MCP route must not weaken the signed-API middleware. It is an explicit
  bypass with its own bearer check.
- MCP protocol handling uses `rmcp` 3.8 with the
  `transport-streamable-http-server` feature; no hand-rolled JSON-RPC.
- The MCP handler lives inside `microwave-api`, not in a separate crate. A
  separate `microwave-mcp` crate would need the API's `AppState` and mutation
  services, creating a dependency cycle; a lower-level service crate is not
  worth the churn for an experiment.
- The server currently advertises `/organizations/...` routes; MCP follows
  that shape so the organization is unambiguous.
- Write handlers are extracted into `pub(crate)` service functions in their
  existing modules and called by both the axum handlers and the MCP tools.

## Approaches considered

### A — stdio MCP with direct store access

A separate `microwave-mcp` binary speaking MCP over stdio, opening SQLite or
PostgreSQL directly. Simplest to build and use with local agent clients, but
only works on the server host and bypasses Chef ACLs entirely. Rejected because
the request is for agents to connect to the server, and because C gives us
organization scoping and a shared write path.

### B — stdio MCP over the signed Chef API

A separate binary with a Chef client key that calls new REST endpoints for
reports. Preserves ACLs and identity, but requires two new surfaces (REST plus
stdio) and is awkward for agent clients that cannot sign Chef requests.
Rejected as more work for less reach.

### C — HTTP MCP endpoint in the main server (chosen)

Mount an rmcp Streamable HTTP service per organization in the existing axum
router, authenticate with a per-organization bearer token, and call shared
read/mutation services. Remote agents can connect with a normal HTTP client;
there is one new surface; reports, reads, and writes ship together. Cost: a new
auth path and a new endpoint in the daemon, plus a refactor that extracts the
existing mutation handlers into services.

## Design

### 1. Configuration

Extend `[data_collector]`:

```toml
[data_collector]
# forward (default): forward only, no local storage
# local: store locally, do not forward
# both: store locally and forward
store = "forward"
# Keep this many runs per node; 0 keeps everything. Old runs are pruned after
# each successful insert.
keep_runs = 50
root_url = "https://automate.example.com/data-collector/v0/"  # required for forward/both
```

`DataCollectorConfig::is_configured()` keeps its meaning for forwarding. A new
`is_enabled()` returns true when `root_url` is set or `store != forward`; the
route and the middleware bypass use `is_enabled()`. `root_url` is only required
when forwarding is active. Validation rejects unknown `store` values and
negative `keep_runs` values.

Add a new `[mcp]` section with one entry per organization:

```toml
[mcp]
enabled = false
# Upper bound for any list tool result and for per-object ACL checks.
max_results = 100
# Upper bound for one tool result in bytes. Large reports and node documents
# are rejected with an isError result instead of being truncated silently.
max_result_bytes = 1048576
# Decoded size cap for one upload_cookbook_version call. The HTTP body limit
# is derived from this (base64 inflation plus JSON overhead).
max_upload_bytes = 10485760
# Host headers the MCP endpoint accepts. rmcp defaults to loopback only, which
# would reject remote agents. When empty, the host from server.public_url is
# used; if that is unset too, the endpoint stays loopback-only.
allowed_hosts = ["chef.example.com", "chef.example.com:443"]
# Browser origins; empty disables Origin validation. Agents use non-browser
# clients, so this stays empty unless a browser client is added.
allowed_origins = []

[mcp.organizations.acme]
# Bearer token agents send as `Authorization: Bearer <token>`. Alternatively
# set the `mcp/acme` secret via
#   microwave-ctl set-secret mcp acme <token>
# Config wins over the secret.
token = "replace-with-a-long-random-token"
# Chef user the token acts as. Defaults to auth.superuser_name (pivotal).
# A superuser bypasses ACL checks; any other user is checked per object.
principal = "mcp-agent-acme"
# When false, the write tools are not registered for this organization.
writes = true

[mcp.organizations.beta]
principal = "mcp-agent-beta"
writes = false
```

Rules:

- The map key is the organization name. A token is only valid for that
  organization; the endpoint path must match the entry.
- `principal` defaults to `auth.superuser_name`. It must resolve to an existing
  user, and that user must be an organization member unless it is the platform
  superuser; otherwise requests for that org fail with 500 and a log line.
- `writes` defaults to `true` when an organization entry exists. A read-only
  deployment sets it to `false`; the write tools are then absent from
  `tools/list` and calls to them fail as unknown tools.
- An organization without an `[mcp.organizations.*]` entry has no MCP endpoint
  (404), even when `mcp.enabled = true`.
- `max_results` is clamped to `1..=1000`; `max_upload_bytes` to
  `1..=250 MiB` (the bookshelf limit); `max_result_bytes` to `1 KiB..=16 MiB`.

Environment overrides cover the scalar settings only:
`MW_MCP__ENABLED`, `MW_MCP__MAX_RESULTS`, `MW_MCP__MAX_RESULT_BYTES`,
`MW_MCP__MAX_UPLOAD_BYTES`, `MW_MCP__ALLOWED_HOSTS` and
`MW_MCP__ALLOWED_ORIGINS` (comma-separated), `MW_DATA_COLLECTOR__STORE`,
`MW_DATA_COLLECTOR__KEEP_RUNS`. The
per-organization map is configured in the file or via the `mcp/<org>` secret;
hand-written env parsing does not support nested maps.

Secret support: extend the `microwave-ctl` credential validator so the `mcp`
group accepts arbitrary names (one per organization). `set-secret mcp <org>
<token>` and `show-secret mcp <org>` then work without adding a new command.
The store layer already accepts arbitrary validated `(group, name)` pairs.

### 2. Ingestion

A Chef Infra Client sends **two** messages per run: `message_type: "run_start"`
(with only `node_name`, `run_id`, `start_time`) and `message_type:
"run_converge"` (with status, resources, counts, and errors). Action messages
use `message_type: "action"`.

v1 stores **only `run_converge`**:

- `run_start` and action messages are handled exactly as today (forwarded when
  forwarding is enabled, otherwise acknowledged with 200) and are never stored.
  This avoids an empty "in progress" row shadowing the final report and avoids
  an out-of-order `run_start` overwriting a converged row.
- A stored row therefore always has a real status and resource data. There is
  no `running` state in v1.

`POST /organizations/:org/data-collector` keeps its current authentication and
forwarding behavior, and gains a local branch for `run_converge`:

1. Authenticate exactly as today (shared token or Chef signature).
2. Parse the JSON body if local storage is active. A malformed body, a missing
   `node_name`, or a missing `run_id` on a `run_converge` message returns 400.
   Non-`run_converge` messages skip parsing and storage.
3. Store locally first, then forward. A failed store returns 500 without
   forwarding; a failed forward still returns 502/504 but keeps the local row.
4. Respond with the upstream response if forwarding happened; otherwise 200
   with `{}` (accepted).

Payload handling is defensive: unknown fields are preserved in the stored raw
document. Extracted fields:

| Field | Source | Fallback |
| --- | --- | --- |
| `node_name` | `node_name` | required |
| `run_id` | `run_id` | required |
| `status` | `status` (`success`/`failure`) | `"unknown"` |
| `start_time` | `start_time`, normalized | server receive time |
| `end_time` | `end_time`, normalized | `null` |
| `resource_count` | `total_resource_count` | `resources.len()` |
| `updated_resource_count` | `updated_resource_count` | count of resources with status `updated` |
| `error_count` | top-level `error` (0 or 1) plus resources with `error` | computed |

Timestamp normalization: `start_time` and `end_time` are parsed as RFC 3339,
converted to UTC, and stored with fixed millisecond precision
(`YYYY-MM-DDTHH:MM:SS.sssZ`). Fixed-width UTC text sorts chronologically, so
pruning, `latest_run`, and `since` can compare and order the column directly.
If `start_time` is missing or unparseable, the server receive time is used, so
`start_time` is always present. `NodeRunFilter.since` is an RFC 3339 instant
and is compared against the normalized `start_time`.

`failures_only` means `status != 'success' OR error_count > 0`.

### 3. Storage schema

Both migration files get the same table (SQLite and PostgreSQL syntax
differences only in types):

```sql
CREATE TABLE IF NOT EXISTS node_runs (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    node_name TEXT NOT NULL,
    run_id TEXT NOT NULL,
    status TEXT NOT NULL,
    start_time TEXT NOT NULL,      -- normalized UTC, millisecond precision
    end_time TEXT,
    resource_count INTEGER NOT NULL DEFAULT 0,
    updated_resource_count INTEGER NOT NULL DEFAULT 0,
    error_count INTEGER NOT NULL DEFAULT 0,
    report TEXT NOT NULL,          -- compressed with encode_serialized (mwz1: + base64 zlib)
    created_at TEXT NOT NULL,
    UNIQUE (org_id, node_name, run_id),
    FOREIGN KEY (org_id) REFERENCES orgs (id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS node_runs_org_node_time_index
    ON node_runs (org_id, node_name, start_time DESC);
CREATE INDEX IF NOT EXISTS node_runs_org_time_index
    ON node_runs (org_id, start_time DESC);
```

There is deliberately **no foreign key to `nodes`**. A client may report before
the node object exists, and a node can be deleted while its history is still
useful. Instead, ACL checks resolve the node by `(org_id, node_name)` at query
time. Reports whose node no longer exists are visible to superusers only; org
isolation still applies. Cascading report cleanup on node deletion is a
follow-up, not part of v1.

Re-inserting the same `(org_id, node_name, run_id)` updates the row (retries are
idempotent), preserving the original `id` when possible.

Converge errors are not duplicated into a second table in v1. `error_count`
makes them cheap to find, and the MCP tool parses the compressed report for the
matching runs. The parser covers both error locations: the top-level `error`
object (run failed before resource execution, e.g. compile or run-list
expansion) and `resources[].error`. A top-level error is returned as a row with
`resource_type` and `resource_name` set to `null`. If this proves too slow, a
follow-up can add `node_run_errors` without changing the public tool schema.

Pruning runs after each insert: delete rows for that node older than the newest
`keep_runs` (ordered by `start_time DESC`). `0` disables pruning.

### 4. Store contract

New trait `ReportRepository` in `crates/microwave-store/src/backend.rs`:

```rust
pub struct NewNodeRun<'a> {
    pub node_name: &'a str,
    pub run_id: &'a str,
    pub status: &'a str,
    pub start_time: Option<&'a str>,
    pub end_time: Option<&'a str>,
    pub resource_count: i64,
    pub updated_resource_count: i64,
    pub error_count: i64,
    pub report: &'a str,
}

pub struct NodeRunFilter {
    pub node_name: Option<String>,
    pub status: Option<String>,
    pub since: Option<String>,
    pub failures_only: bool,
    pub limit: usize,
}

#[async_trait]
pub trait ReportRepository: Send + Sync {
    async fn store_node_run(&self, org_id: &str, run: NewNodeRun<'_>, keep_runs: usize)
        -> Result<StoredNodeRun, StoreError>;
    async fn list_node_runs(&self, org_id: &str, filter: &NodeRunFilter)
        -> Result<Vec<StoredNodeRun>, StoreError>;
    async fn find_node_run(&self, org_id: &str, node_name: &str, run_id: &str)
        -> Result<Option<StoredNodeRun>, StoreError>;
    async fn latest_node_run(&self, org_id: &str, node_name: &str)
        -> Result<Option<StoredNodeRun>, StoreError>;
}
```

`StoredNodeRun` carries the extracted columns and the decoded report JSON.
Implementations:

- SQLite: `crates/microwave-store/src/reports.rs` plus delegation in
  `backend_sqlite.rs`.
- PostgreSQL: `crates/microwave-store-postgres/src/reports.rs`.

`MetadataStore` gains `ReportRepository` as a supertrait only if every existing
implementation can satisfy it; otherwise handlers use a small enum/capability
check. Preferred: add the supertrait and implement it for both backends, since
both are in-tree and tests already require both. (Builder may choose the
capability approach if a test double breaks.)

Cookbook uploads need one small addition: an idempotent
`ensure_checksums(org_id, &[String])` method on `SandboxRepository` (next to
`checksum_exists` and `existing_checksums`) that inserts missing rows into
`checksums`. It is implemented by both backends.

### 5. MCP endpoint and authentication

Module `crates/microwave-api/src/mcp/`:

- `mod.rs` — route construction, per-request service wiring.
- `auth.rs` — bearer extraction, per-org token lookup, principal resolution.
- `context.rs` — `McpContext { state, org, principal, is_superuser, writes,
  max_results, max_upload_bytes }`.
- `tools.rs` — read and write tool implementations.

`McpHandler` (or the equivalent rmcp handler) holds the context and implements
`rmcp::ServerHandler`. Tools are declared with `#[tool]`/`#[tool_router]` from
`rmcp-macros` (or hand-built `Tool` definitions if the macro API differs in
3.8). Tool handlers return JSON text content; expected failures (not found,
forbidden, invalid argument, conflict) set `isError: true` with a stable
message.

Request flow:

1. Route `/organizations/{organization}/mcp` is merged into the router only
   when `config.mcp.enabled`.
2. The signed-API middleware bypasses this path (like `/id`), so the route has
   its own auth layer.
3. Resolve the organization from the path; unknown org → 404.
4. Look up `mcp.organizations[org.name]`; no entry → 404 (the endpoint does not
   exist for that org).
5. Resolve the token: config value, else the `mcp/<org>` secret. Missing token
   configuration → 401 and a warning log (never fall through to open access).
   Compare with `Authorization: Bearer` in constant time; mismatch → 401 with
   `WWW-Authenticate: Bearer`.
6. Resolve the configured principal user. Missing user → 500 plus a log line.
   Require org membership unless the principal is the platform superuser.
7. Build the `McpContext` (holding `Arc<AppState>`, the `Org`, the resolved
   principal, `is_superuser`, `writes`, `max_results`, `max_result_bytes`, and
   `max_upload_bytes`) and run the rmcp service for this request.
8. `StreamableHttpServerConfig`:
   - `with_json_response(true)` for simple request/response clients.
   - `with_legacy_session_mode(false)` so requests are stateless and no
     session affinity is needed.
   - `with_max_request_body_bytes(max_upload_bytes * 4 / 3 + 1 MiB)`, which
     rmcp enforces while streaming and answers with 413.
   - `with_allowed_hosts(...)` from `mcp.allowed_hosts`, falling back to the
     host of `server.public_url`. rmcp rejects non-loopback `Host` headers by
     default (DNS-rebinding protection), so a remote deployment must configure
     this or the endpoint is only reachable locally.
   - `with_allowed_origins(...)` only when `mcp.allowed_origins` is non-empty;
     empty keeps Origin validation disabled for non-browser agents.

The rmcp service factory does not see the request directly. Preferred wiring is
to construct a service per request with the context captured in its closure.
If rmcp needs a stable service instance for sessions, fall back to a
task-local `McpContext` set by the auth layer and read by the factory; this is
a builder detail because the exact 3.8 signature decides it.

Because the endpoint is stateless and per-request, `tools/list` reflects the
org's `writes` flag and the request's principal. No server-side session state
is retained beyond what `rmcp` itself keeps for a streamable request.

### 6. Read tools

| Tool | Arguments | Result |
| --- | --- | --- |
| `list_nodes` | `query?`, `environment?`, `limit?` | name, environment, policy name/group, updated_at; `query` is a case-insensitive substring match on the node name |
| `get_node` | `name` | full node object |
| `list_environments` | `limit?` | name, updated_at |
| `get_environment` | `name` | full environment object |
| `list_cookbooks` | `query?`, `limit?` | name, versions, newest version; `query` is a case-insensitive substring match on the cookbook name |
| `list_cookbook_versions` | `name` | version, frozen, updated_at |
| `get_cookbook_version` | `name`, `version` | manifest summary, dependencies, frozen |
| `list_runs` | `node_name?`, `status?`, `failures_only?`, `since?`, `limit?` | run id, node, status, start/end, resource/error counts |
| `latest_run` | `node_name` | the newest run summary plus its top-level error and resource errors |
| `get_run` | `node_name`, `run_id` | full stored report (resources, errors) |
| `list_converge_errors` | `node_name?`, `since?`, `limit?` | run id, node, time, resource type/name (null for top-level errors), error class/message |

ACL: for a non-superuser principal, `list_*` filters objects the principal
cannot read; `get_*` returns `isError` with a forbidden message. Reports use
the node's authz id, resolved by `(org_id, node_name)`. Reports whose node no
longer exists are visible to superusers only. The superuser principal skips
these checks, matching the rest of the server.

Limits: every list tool clamps `limit` to `mcp.max_results` (default 100).
Reports are loaded only for the selected rows; the raw report is decoded lazily
for `get_run` and `list_converge_errors`. Every tool serializes its result and
rejects it with an `isError` message when it exceeds `mcp.max_result_bytes`;
results are never silently truncated. `since` must parse as RFC 3339, otherwise
the tool returns an invalid-argument error.

### 7. Write tools

All writes are registered only when the org's `writes = true`. They call shared
service functions, not a parallel implementation.

| Tool | Arguments | Semantics |
| --- | --- | --- |
| `create_node` | `name`, `environment?`, `run_list?`, `normal?`, `policy_name?`, `policy_group?` | builds a node document and runs the create path (container `nodes` Create ACL) |
| `update_node` | `name`, `node` (full document) | same normalization and validation as `PUT /nodes/:name`; node Update ACL |
| `delete_node` | `name` | node Delete ACL |
| `create_environment` | `name`, `attributes?`, `cookbook_versions?` | container `environments` Create ACL |
| `update_environment` | `name`, `environment` (full document), `new_name?` | environment Update ACL; rename keeps the HTTP API's index behavior |
| `delete_environment` | `name` | environment Delete ACL; `_default` is rejected like the HTTP API |
| `upload_cookbook_version` | `name`, `version`, `metadata`, `files`, `frozen?`, `force?` | uploads inline files, records checksums, then runs the normal create/update path |
| `delete_cookbook_version` | `name`, `version` | cookbook version Delete ACL |

Write tools return a compact summary, not the full object: node name,
environment, policy name/group; environment name and timestamp; cookbook name,
version, and frozen flag. This keeps results below `max_result_bytes` and avoids
the confusing case where a write succeeded but the response is rejected for
being too large. Read tools remain the way to fetch full objects.

`update_node` and `update_environment` take a full document, matching the HTTP
API's PUT semantics. They are not JSON-patch tools. Agent-facing descriptions
say so explicitly.

Refactor to share code:

- `crates/microwave-api/src/nodes.rs`: extract
  `pub(crate) async fn create_node_object`, `update_node_object`,
  `delete_node_object`. Each takes `&AppState`, `&AuthContext`, `&Org`, and
  validated input, performs ACL checks, store writes, and search indexing, and
  returns the stored node.
- `crates/microwave-api/src/environments.rs`: equivalent
  `create_environment_object`, `update_environment_object`,
  `delete_environment_object`.
- `crates/microwave-api/src/cookbooks.rs`: `put_cookbook_version_object` and
  `delete_cookbook_version_object`. The put service takes a manifest `Value`
  and an explicitly selected API version (MCP uses v2 semantics).
- The axum handlers become thin wrappers that add `Location` headers, status
  codes, and the negotiated API version. Existing HTTP behavior and tests must
  stay unchanged.
- MCP maps `ApiError` to tool errors: 400/409/422 → message text, 403 →
  "forbidden", 404 → "not found", 5xx → generic "internal error" plus a log
  line.

### 8. Cookbook upload

The HTTP API expects a JSON cookbook manifest whose files were uploaded to
`/bookshelf/organizations/:org/checksums/:md5` first. MCP collapses this into
one tool:

```json
{
  "name": "nginx",
  "version": "1.2.0",
  "metadata": { "dependencies": { "apt": ">= 7.0.0" } },
  "frozen": false,
  "files": [
    { "path": "recipes/default.rb", "content_base64": "..." },
    { "path": "templates/default/nginx.conf.erb", "content_base64": "...", "specificity": "default" }
  ]
}
```

Server-side steps:

1. Validate the cookbook name and version. Decode each `content_base64`,
   enforcing `mcp.max_upload_bytes` for the decoded total and a file-count cap
   (100). Compute the MD5 in the same format as bookshelf.
2. Build a v2 manifest: `name = "<name>-<version>"`, `cookbook_name`, `version`,
   `metadata` (with `version` injected), and `all_files` entries
   `{name, path, checksum, specificity}`. If an entry provides `checksum`
   instead of `content_base64`, it is accepted only when that checksum already
   exists (the blob was uploaded out of band).
3. Record missing checksums with `ensure_checksums` and put each blob into the
   object store using the same namespace and call path as bookshelf.
4. Call `put_cookbook_version_object`. It re-checks ACLs, the frozen flag,
   checksum existence, and performs the store write. `frozen?` is merged with
   the existing version exactly as over HTTP.

Failure ordering: blobs are written before the database row. A failed store
leaves orphaned blobs, which a later upload of the same checksum reuses. This
matches the existing sandbox/bookshelf behavior and is documented as a v1
limitation.

The tool result is a compact summary: cookbook name, version, and the merged
`frozen?` flag. `get_cookbook_version` returns the rendered manifest. An update
of an already-existing version follows the HTTP rules: if it is frozen, the
call fails unless `force: true` is passed; a new upload keeps `frozen` sticky.

### 9. Crate and router layout

- `crates/microwave-api/src/mcp/`: route, auth layer, context, read/write tools,
  error mapping, unit tests for argument parsing.
- `crates/microwave-api/src/lib.rs`: merge the route only when
  `config.mcp.enabled`.
- `crates/microwave-api/src/middleware.rs`: bypass branch for
  `/organizations/{org}/mcp`, active only when `mcp.enabled` is true and the
  path has exactly the MCP segment shape (no trailing-slash surprise).
- `crates/microwave-api/src/data_collector.rs`: local store branch.
- `crates/microwave-api/src/nodes.rs`,
  `crates/microwave-api/src/environments.rs`,
  `crates/microwave-api/src/cookbooks.rs`: shared mutation services.
- `crates/microwave-config/src/config.rs`: `McpConfig`, `McpOrgConfig`,
  `DataCollectorStore`.
- `crates/microwave-store/src/reports.rs`,
  `crates/microwave-store-postgres/src/reports.rs`: report implementations.
- `crates/microwave-store/src/sandboxes.rs` (and Postgres equivalent):
  `ensure_checksums`.
- `crates/microwave-ctl/src/secrets.rs`: allow arbitrary names in the `mcp`
  credential group.
- Both `0001_initial.sql` files: `node_runs` table and indexes.

### 10. Error handling and observability

- Ingestion: invalid JSON/missing fields → 400 with the existing error shape;
  store failure → 500; forward failure → 502/504 (existing mapping).
- MCP: protocol errors are handled by rmcp; tool errors are JSON text with
  `isError: true`; internal store failures are logged with the request org and
  tool and returned as a generic "internal error" text.
- Log one line per tool call: org, principal, tool, duration, result count.
  Writes additionally log the target (node/environment name, cookbook
  name/version) and the outcome. Do not log report bodies, cookbook contents,
  or raw error messages at info level.
- No new metrics in v1; the existing request metrics cover the HTTP status.
- No persistent audit table. This is a documented limitation of the experiment.

### 11. Security notes

- Each organization has its own static bearer token. A token is only valid for
  its organization's path; the token for org A cannot read org B.
- The token acts as the configured principal. Use a dedicated non-superuser
  user with exactly the needed project ACLs. With the default principal
  (`superuser_name`), the token has full superuser capability for that org and
  every other org it is configured for.
- `writes = false` removes all write tools from `tools/list`. It is a
  capability switch, not an additional authorization layer; the principal's
  ACLs are still enforced for every write.
- Destructive tools (`delete_node`, `delete_environment`,
  `delete_cookbook_version`) are available whenever `writes = true`. There is
  no dry-run and no confirmation prompt; agents should be pointed at
  non-production orgs for the experiment.
- Prefer the `mcp/<org>` secret over a token in the config file. The conffile
  should be `0640`, owned by the server user.
- Reports may contain resource names, error messages, and backtraces from
  nodes; they are as sensitive as the node object. Same ACL, same database.
- Cookbook uploads accept base64 content up to `max_upload_bytes` per call.
  Keep the default small; the endpoint is not a general-purpose artifact
  store.
- The endpoint is experimental and should be restricted at the reverse proxy
  if the server is internet-facing. Configure `mcp.allowed_hosts` for the
  public hostname; the rmcp default only accepts loopback hosts.
- Tokens are not scoped per tool and have no expiry. This is acceptable only
  for the experiment.

### 12. Testing

- Store parity (`crates/microwave-store/tests/dual_backend.rs` new `reports`
  suite, run on SQLite and PostgreSQL): insert/list/filter, idempotent
  re-insert, `latest`, `failures_only`, `since`, pruning with `keep_runs`,
  compressed report round-trip, org isolation, `ensure_checksums` idempotency.
- Config tests: `store` values, forwarding requirements, per-org parsing,
  `writes` default, token/principal defaults, `max_results`,
  `max_result_bytes`, and `max_upload_bytes` bounds, `allowed_hosts` fallback
  to `server.public_url`, scalar environment overrides.
- Ingestion tests (`crates/microwave-api/tests/data_collector_test.rs`):
  forward-only unchanged, local-only, both, `run_start` and action messages not
  stored, invalid JSON/missing run id, timestamp normalization and fallback,
  token auth, prune, signing still accepted.
- MCP read tests (`crates/microwave-api/tests/mcp_test.rs`): 401 without token,
  401 for a token from another org, 404 for unknown/unconfigured org,
  initialize + tools/list, each read tool happy path, top-level error rows in
  `list_converge_errors`, ACL filtering with a non-superuser principal, reports
  of deleted nodes hidden from non-superusers, `max_results` clamp, disabled
  endpoint absent, org isolation, non-loopback `Host` rejected unless
  configured, oversized request body rejected with 413, oversized tool result
  rejected with an `isError` message, invalid `since` rejected.
- MCP write tests: `writes = false` hides and rejects write tools; create,
  update, and delete each object type; ACL denial for a non-superuser;
  node/environment search index updated or removed; cookbook upload with inline
  files round-trips through `get_cookbook_version` and bookshelf download;
  frozen-version conflict and `force`; upload size and file-count caps;
  environment `_default` delete rejection; rename behavior.
- HTTP regression: the existing node, environment, and cookbook tests must
  stay green after the service extraction, including status codes and
  `Location` headers.
- Snapshot tests: assert `node_runs` is not part of the logical snapshot, so
  the exclusion is intentional and stays documented.
- PostgreSQL HTTP coverage gets the MCP happy path if the existing
  `postgres_http_coverage_test` structure allows it; otherwise the store
  parity suite plus SQLite HTTP tests are the gate for v1.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets`,
  `cargo fmt --check`.

### 13. Documentation and packaging

- `docs/mcp.md`: enablement, per-org token setup (config and
  `set-secret mcp <org> <token>`), `allowed_hosts` requirements for remote
  agents, agent configuration examples (Claude Desktop/Cursor HTTP MCP with the
  Authorization header), complete tool reference with read/write markers,
  ACL/superuser warning, destructive-tool warning, limitations (no audit table,
  no token expiry, no snapshot support, orphaned blobs), and the upload payload
  format.
- `packaging/microwave.toml.example`: `[data_collector] store`/`keep_runs` and a
  commented `[mcp]` block with two org examples.
- `README.md`: status line and deployment pointer.
- `docs/deployment.md`: short section linking to `docs/mcp.md`; note the
  data-collector mode change and the new write surface.
- The `.deb` package already ships the example config, so no packaging changes
  beyond the config template.

## Implementation phases

The work is sequenced so each phase is independently testable:

1. **Report storage and ingestion** — config, both migrations, `ReportRepository`,
   data-collector local branch, pruning, store parity and ingestion tests.
2. **MCP read surface** — `rmcp` integration, per-org auth, read tools, HTTP
   tests, docs.
3. **Shared mutation services** — extract node/environment/cookbook handlers
   without behavior change; existing HTTP tests stay green.
4. **MCP write surface** — write tools, cookbook inline upload, ACL and
   regression tests, docs update.

## What is left to the builder

- Exact rmcp 3.8 API details (`ServerHandler` method signatures, tool router
  macro shape, `StreamableHttpServerConfig` builder names) and whether a
  per-request service or the task-local fallback is the right wiring.
- Whether `MetadataStore` can gain `ReportRepository` as a supertrait without
  breaking test doubles; otherwise use a capability method.
- SQL upsert syntax (`ON CONFLICT ... DO UPDATE`) and whether the supported
  SQLite version range covers the chosen form.
- Whether to store the report as compressed text or a BLOB; use the existing
  `encode_serialized` helper either way.
- The exact shape of the extracted mutation services (return types, how much
  response metadata stays in the HTTP handler) as long as HTTP behavior is
  unchanged.
- The object-store namespace used by bookshelf versus sandboxes; the existing
  code mixes the org name and org id, and the upload path must match whichever
  is correct. Verify with an end-to-end upload/download test before relying on
  it.
- Tool description wording and JSON schema details.
- Whether the cookbook tools use the existing `list_cookbook_versions` query or
  a summary query; reuse existing store methods rather than adding new ones
  unless the cost is measurable.

## Rollout

- Default off (`mcp.enabled = false`, `data_collector.store = "forward"`).
- Existing chef-client configurations keep working unchanged; `run_start` and
  action messages are never stored.
- Enabling local storage creates the table on the next connect via the
  existing migration file; no separate migration command.
- Logical snapshot and `microwave-migrate` keep ignoring `node_runs` in v1;
  this is documented so an operator migration does not silently lose reports
  without warning.
- Removing the feature later is safe: the table stays, nothing reads it.
- Start with `writes = false` for an org, verify the read tools, then enable
  writes and point the token at a principal with the smallest sufficient ACL
  set.

## Decisions taken

These were open in revision 1 and are now fixed by the requester:

1. **Write tools are in v1.** The set is create/update/delete node,
   create/update/delete environment, and upload/delete cookbook version.
2. **One bearer token per organization**, mapped to a configured principal.
   Tokens are org-bound and cannot cross organization paths.
3. **`keep_runs = 50`** per node, `0` disables pruning.
