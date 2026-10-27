# Experimental MCP server and local run reports

Date: 2026-10-27
Status: draft for approval
Approach: C — HTTP MCP endpoint inside the server

## Intent

Operators currently have to read `journalctl` or open an external Automate
instance to find out why a node failed to converge. Agents have no supported
way to ask the server what happened on a node, which cookbooks exist, or which
resources failed.

This design adds two related pieces:

1. **Local run reports** — the existing data-collector endpoint can optionally
   persist Chef Infra Client reports in the metadata database (SQLite or
   PostgreSQL), either instead of or in addition to forwarding them to
   Automate.
2. **An experimental MCP server** — a Streamable HTTP endpoint per organization
   that exposes the stored runs, converge errors, nodes, and cookbooks as MCP
   tools for agents.

The feature is opt-in and read-only in its first version.

## Why now

- The data-collector endpoint already receives exactly the payload that
  contains run status and resource errors; it currently discards it after
  forwarding.
- The store abstraction already supports SQLite and PostgreSQL behind one
  trait, so report persistence fits the existing backend matrix.
- `rmcp` 3.8 (the official Rust MCP SDK) provides a Streamable HTTP Tower
  service, so an MCP endpoint can be mounted under the existing axum router.

## Goals

- Optional local persistence of run reports in SQLite and PostgreSQL.
- Configurable combination of local storage and Automate forwarding
  (`forward`, `local`, `both`), with today's behavior as the default.
- Bounded growth: a configurable number of runs kept per node.
- An HTTP MCP endpoint under `/organizations/{organization}/mcp` with bearer
  authentication.
- Read-only tools for the questions in the request: search nodes, list
  cookbooks, list/get runs, and list converge errors.
- ACL-aware results for non-superuser principals.
- Tests against both storage backends and end-to-end HTTP tests of the MCP
  endpoint.

## Non-goals

- Write/mutation tools (node edits, cookbook uploads, policy changes). The
  request mentions changes, but this component is report storage. Writes are
  phase 2 and need an audit story first.
- A signed-request (Chef key) MCP client. Agents send a bearer token; machine
  clients can be added later.
- Streaming/long-running MCP tasks, subscriptions, prompts, or resources.
  Only tools are exposed.
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
- Hardening of the token model (rotation UI, per-tool scopes, expiry). The
  token is a static experimental credential.

## Anti-goals

- Changing the default data-collector behavior. An upgrade must not start
  storing reports unless configured.
- Making the MCP endpoint reachable without a token.
- Letting a non-superuser principal read objects its Chef ACLs deny.
- Breaking `POST /organizations/:org/data-collector` for existing chef-client
  configurations that send only `x-data-collector-token`.
- Unbounded database growth from reports.

## Constraints

- One metadata contract: the new repository methods live in
  `microwave-store`'s backend trait; SQLite and PostgreSQL implement them.
- The store must keep working when the report feature is disabled: no new
  required methods on `MetadataStore` if that would break test doubles; a
  separate `ReportRepository` trait is used instead.
- Reports arrive at the existing endpoint and must keep the same
  authentication rules (shared token or Chef signature).
- The MCP route must not weaken the signed-API middleware. It is an explicit
  bypass with its own bearer check.
- MCP protocol handling uses `rmcp` 3.8 with the
  `transport-streamable-http-server` feature; no hand-rolled JSON-RPC.
- The server currently advertises `/organizations/...` routes; MCP follows
  that shape so the organization is unambiguous.

## Approaches considered

### A — stdio MCP with direct store access

A separate `microwave-mcp` binary speaking MCP over stdio, opening SQLite or
PostgreSQL directly. Simplest to build and use with local agent clients, but
only works on the server host and bypasses Chef ACLs entirely. Rejected because
the request is for agents to connect to the server, and because C gives us
organization scoping.

### B — stdio MCP over the signed Chef API

A separate binary with a Chef client key that calls new REST endpoints for
reports. Preserves ACLs and identity, but requires two new surfaces (REST plus
stdio) and is awkward for agent clients that cannot sign Chef requests.
Rejected as more work for less reach.

### C — HTTP MCP endpoint in the main server (chosen)

Mount an rmcp Streamable HTTP service per organization in the existing axum
router, authenticate with a bearer token, and query the store directly through
an ACL-aware handler. Remote agents can connect with a normal HTTP client;
there is one new surface; reports and MCP ship together. Cost: a new auth
path and a new endpoint in the daemon.

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
keep_runs = 10
root_url = "https://automate.example.com/data-collector/v0/"  # required for forward/both
```

`DataCollectorConfig::is_configured()` keeps its meaning for forwarding. A new
`is_enabled()` returns true when `root_url` is set or `store != forward`; the
route and the middleware bypass use `is_enabled()`. `root_url` is only required
when forwarding is active. Validation rejects `store` values other than the
three names and a `keep_runs` value that is not a non-negative integer (TOML
already enforces the type).

Add a new `[mcp]` section:

```toml
[mcp]
enabled = false
# Bearer token agents send as `Authorization: Bearer <token>`. May also be
# stored as the `mcp.token` secret via
#   microwave-ctl set-secret mcp token <token>
token = "replace-with-a-long-random-token"
# Chef user the token acts as. Defaults to auth.superuser_name (pivotal).
# A superuser bypasses ACL checks; any other user is checked per object.
principal = "pivotal"
# Upper bound for any tool result; also caps per-object ACL checks.
max_results = 100
```

Environment overrides follow the existing `MW_*` convention:
`MW_MCP__ENABLED`, `MW_MCP__TOKEN`, `MW_MCP__PRINCIPAL`, `MW_MCP__MAX_RESULTS`,
and `MW_DATA_COLLECTOR__STORE`, `MW_DATA_COLLECTOR__KEEP_RUNS`.

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

### 5. MCP endpoint

New crate `crates/microwave-mcp`:

- `McpHandler` implements `rmcp::ServerHandler` and holds a small context:
  `Arc<dyn MetadataStore>`, `Org`, `Principal`, `is_superuser`, `max_results`.
- Tools are declared with `#[tool]`/`#[tool_router]` from `rmcp-macros` (or
  hand-built `Tool` definitions if the macro API differs in 3.8).
- Tool handlers return JSON text content; expected failures (not found,
  forbidden, invalid argument) set `isError: true` with a stable message.

Mounting and authentication:

- Route: `/organizations/{organization}/mcp` in the main router, any method
  forwarded to the Streamable HTTP service.
- The signed-API middleware bypasses this path (like `/id`), so the route is
  wrapped in its own layer:
  1. Resolve the bearer token from `Authorization`.
  2. Compare constant-time against `mcp.token` or the `mcp.token` secret.
  3. Resolve the organization from the path (404 if absent).
  4. Resolve the configured principal user; require organization membership
     unless the principal is the platform superuser.
  5. On success, set a task-local `McpContext` and run the rmcp service.
- The rmcp service factory runs per request (stateless mode) and reads the
  task-local context to build the handler. This keeps the standard
  `nest_service` mounting while giving each request its own org and principal.
- `StreamableHttpServerConfig` uses `with_json_response(true)` for simple
  request/response clients. The endpoint accepts `POST`; `GET`/`DELETE`
  behavior follows rmcp defaults.
- Missing/invalid token → 401 with `WWW-Authenticate: Bearer`. Disabled MCP →
  the route is absent and returns the normal 404.

Tool set (read-only):

| Tool | Arguments | Result |
| --- | --- | --- |
| `list_nodes` | `query?`, `environment?`, `limit?` | name, environment, policy name/group, updated_at; `query` is a case-insensitive substring match on the node name |
| `get_node` | `name` | full node object |
| `list_cookbooks` | `query?`, `limit?` | name, versions, newest version; `query` is a case-insensitive substring match on the cookbook name |
| `get_cookbook_version` | `name`, `version` | manifest summary, dependencies, frozen |
| `list_runs` | `node_name?`, `status?`, `failures_only?`, `limit?` | run id, node, status, start/end, resource/error counts |
| `latest_run` | `node_name` | the newest run summary plus its top-level error and resource errors |
| `get_run` | `node_name`, `run_id` | full stored report (resources, errors) |
| `list_converge_errors` | `node_name?`, `since?`, `limit?` | run id, node, time, resource type/name (null for top-level errors), error class/message |

ACL: for a non-superuser principal, `list_*` filters objects whose node or
cookbook authz the principal cannot read; `get_*` returns `isError` with a
forbidden message. Reports use the node's authz id, resolved by
`(org_id, node_name)`. Reports whose node no longer exists are visible to
superusers only. The superuser principal skips these checks, matching the rest
of the server.

Limits: every tool clamps `limit` to `mcp.max_results` (default 100). Reports
are loaded only for the selected rows; the raw report is decoded lazily for
`get_run` and `list_converge_errors`.

No mutation tools in v1.

### 6. Crate and router layout

- `crates/microwave-mcp/`: handler, tool definitions, context, error mapping,
  and unit tests for tool argument parsing.
- `crates/microwave-api/src/mcp.rs`: route, auth layer, task-local context,
  service construction.
- `crates/microwave-api/src/lib.rs`: merge the route only when
  `config.mcp.enabled`.
- `crates/microwave-api/src/middleware.rs`: bypass branch for
  `/organizations/{org}/mcp` (exact segment match, no trailing-slash surprise).
- `crates/microwave-api/src/data_collector.rs`: local store branch.
- `crates/microwave-config/src/config.rs`: `McpConfig`, `DataCollectorStore`.
- `crates/microwave-store/src/reports.rs`,
  `crates/microwave-store-postgres/src/reports.rs`: implementations.
- Both `0001_initial.sql` files: `node_runs` table and indexes.

### 7. Error handling and observability

- Ingestion: invalid JSON/missing fields → 400 with the existing error shape;
  store failure → 500; forward failure → 502/504 (existing mapping).
- MCP: protocol errors are handled by rmcp; tool errors are JSON text with
  `isError: true`; internal store failures are logged with the request org and
  tool and returned as a generic "internal error" text.
- Log one line per tool call: org, principal, tool, duration, result count.
  Do not log report bodies or error messages at info level.
- No new metrics in v1; the existing request metrics cover the HTTP status.

### 8. Security notes

- The MCP token is a static bearer credential. Anyone with it acts as the
  configured principal for the endpoint's organization. Use a long random
  value, keep the conffile at 0640, and prefer the `mcp.token` secret.
- Superuser principals bypass ACLs. The default `pivotal` principal therefore
  sees everything; set `mcp.principal` to a dedicated user with read-only
  project ACLs for production experiments.
- Reports may contain resource names, error messages, and backtraces from
  nodes; they are as sensitive as the node object. Same ACL, same database.
- The endpoint is experimental and should be restricted at the reverse proxy
  if the server is internet-facing.
- The token is not scoped per tool and has no expiry. This is acceptable only
  for the experiment.

### 9. Testing

- Store parity (`crates/microwave-store/tests/dual_backend.rs` new `reports`
  suite, run on SQLite and PostgreSQL): insert/list/filter, idempotent
  re-insert, `latest`, `failures_only`, `since`, pruning with `keep_runs`,
  compressed report round-trip, org isolation.
- Config tests: `store` values, forwarding requirements, token/principal
  defaults, environment overrides.
- Ingestion tests (`crates/microwave-api/tests/data_collector_test.rs`):
  forward-only unchanged, local-only, both, `run_start` and action messages not
  stored, invalid JSON/missing run id, timestamp normalization and fallback,
  token auth, prune, signing still accepted.
- MCP tests (`crates/microwave-api/tests/mcp_test.rs`): 401 without token,
  404 for unknown org, initialize + tools/list, each tool happy path, top-level
  error rows in `list_converge_errors`, ACL filtering with a non-superuser
  principal, reports of deleted nodes hidden from non-superusers, `max_results`
  clamp, disabled endpoint absent, org isolation.
- Snapshot tests: assert `node_runs` is not part of the logical snapshot, so
  the exclusion is intentional and stays documented.
- PostgreSQL HTTP coverage gets the MCP happy path if the existing
  `postgres_http_coverage_test` structure allows it; otherwise the store
  parity suite plus SQLite HTTP tests are the gate for v1.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets`,
  `cargo fmt --check`.

### 10. Documentation and packaging

- `docs/mcp.md`: enablement, token, agent configuration examples
  (Claude Desktop/Cursor HTTP MCP with the Authorization header), tool
  reference, ACL/superuser warning, limitations, and the explicit note that
  `node_runs` is not part of logical snapshots or `microwave-migrate`.
- `packaging/microwave.toml.example`: `[data_collector] store`/`keep_runs` and
  a commented `[mcp]` block.
- `README.md`: status line and deployment pointer.
- `docs/deployment.md`: short section linking to `docs/mcp.md`; note the
  data-collector mode change.
- The `.deb` package already ships the example config, so no packaging changes
  beyond the config template.

## What is left to the builder

- Exact rmcp 3.8 API details (`ServerHandler` method signatures, tool router
  macro shape, `StreamableHttpServerConfig` builder names) and whether the
  task-local mounting works as described; fall back to constructing the
  service per request if not.
- Whether `MetadataStore` can gain `ReportRepository` as a supertrait without
  breaking test doubles; otherwise use a capability method.
- SQL upsert syntax (`ON CONFLICT ... DO UPDATE`) and whether the supported
  SQLite version range covers the chosen form.
- Whether to store the report as compressed text or a BLOB; use the existing
  `encode_serialized` helper either way.
- Tool description wording and JSON schema details.
- Whether the cookbook tools use `list_cookbook_versions` (all versions) or a
  summary query; reuse existing store methods rather than adding new ones
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

## Open questions for the requester

1. Should v1 stay read-only, or is one write tool (for example updating a
   node's run list) part of the experiment? The design currently says
   read-only.
2. Is a single bearer token per server enough, or does the experiment need one
   token per organization/principal? The design uses one token that maps to one
   configured principal.
3. Is `keep_runs = 10` per node a sensible default, or should reports be kept
   forever by default?
