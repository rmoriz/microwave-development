# OpenTelemetry metrics export

Date: 2026-11-03
Status: approved (2026-11-03)
Approach: 1 — OTLP export as a second sink behind the existing registry

## Intent

Operators already scrape `GET /_stats`. What they cannot do is feed the server
into an OpenTelemetry pipeline: there is no OTel SDK, no OTLP endpoint, no
histograms, and no labels. A serious deployment therefore monitors Microwave
blind — total requests and a node count — while the questions that matter
(which route is slow, which organization drives failures, whether converges
succeed, whether MCP tools error, whether the pool is saturated) stay
unanswerable.

This adds OpenTelemetry metrics export over OTLP/HTTP. It is opt-in and off by
default. `/_stats` keeps its current Chef-compatible contract and gains the same
new instruments in Prometheus text form, so scrape users benefit without a
collector.

`[telemetry]` is unrelated: that block is Chef's own compatibility stub. OTel
gets its own `[otel]` block.

## Why now

- The request path already has exactly one funnel (`middleware::observe_request`
  and `middleware::authenticate`), an existing atomic registry
  (`crates/microwave-api/src/metrics.rs`), and an existing exposition renderer.
  A second sink is a small change there instead of a rewrite.
- Every candidate metric source already exists in memory or behind one call:
  `KeygenCache::status()`, `HealthCache`, `pool_stats()`, `count_nodes()`, the
  MCP tool log line, data-collector run ingestion, the search and object-store
  clients.
- `opentelemetry`/`opentelemetry_sdk`/`opentelemetry-otlp` are at a consistent
  0.33.1 across the family, so a pinned workspace dependency is straightforward.
- Operators running Kubernetes or a collector-based stack have asked for OTLP;
  Prometheus-only scraping is not the only deployment shape.

## Goals

- Opt-in OTLP/HTTP (protobuf) metrics export with a configurable endpoint,
  interval, timeout, headers, and resource attributes.
- One instrumentation surface: call sites keep calling `state.metrics.*`; the
  fan-out to local counters and OTel instruments happens inside `Metrics`.
- Request durations as a real histogram with bounded labels: route template,
  method, status code, org — aggregated whole-server or per organization.
- Domain metrics that already have a source: MCP tool calls and durations,
  converge outcomes from the data collector, keygen cache state, dependency
  probe results, store latency, search and object-store latency.
- `/_stats` gains the same instruments in Prometheus text form; its existing
  counters, JSON shape, Basic auth, and maintenance exemption stay unchanged.
- A failing or unreachable collector never slows down or fails a request.
- A clean flush on shutdown, bounded so a dead collector cannot stretch the
  drain window.
- Documentation an operator can follow: collector example, egress note,
  cardinality rules, full metric reference — as a published guide on the docs
  site, not only in the repository.

## Non-goals

- Traces and logs over OTLP. Metrics only; the tracing subscriber stays as is.
- gRPC/tonic transport. HTTP/protobuf only in this design.
- A bundled Grafana, Prometheus, or dashboard JSON.
- OTel in `microwave-ctl`, `microwave-migrate`, `microwave-load`, or the Chef
  client.
- Metrics describing the search cluster or the object store from the outside.
  Only what the server observes through its own clients.
- Changing `/_stats` JSON keys, authentication, or availability rules.

## Anti-goals

- Exporting by default. Default config must stay network-silent.
- A metric whose value costs a database round trip per scrape.
- Unbounded label values (`node`, `client`, `role`, `cookbook` names).
- A second instrumentation API that call sites must remember to call.
- Any change to the `/_stats` byte format that breaks existing assertions.
- Blocking the request path on the exporter, or failing startup because a
  collector is down.

## Constraints

- `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`,
  `opentelemetry-semantic-conventions`, `opentelemetry-proto` (dev only) are
  pinned workspace dependencies at 0.33.1.
- TLS goes through rustls. No OpenSSL.
- No crate names an `opentelemetry` type outside `microwave-otel`. The `Recorder`
  trait and the registry live in a low-level crate (see below) so
  `microwave-store` and `microwave-objectstore` can reach them; `microwave-otel`
  implements the trait; the binary does the wiring.
- Every metric name and attribute also has a Prometheus spelling, so both
  surfaces stay one catalogue. A test pins the mapping table.
- The MCP structured log line keeps its fields; metrics are added next to it.
- `/_stats` continues to be the only scrape surface; no new HTTP surface.
- `serve --check` validates `[otel]` but does not dial the collector.
- Metric values are numbers only; no payload, key, token, or path with IDs
  leaves the process except through the bounded `http.route` template.
- The landing page and its assets are ordinary requests and are measured; the
  nginx-owned paths (`/version`, `/_status`, `/_stats`, `/server_api_version`,
  `/id*`, `/bookshelf/*`, `…/validate`, `…/required_recipe`,
  `…/data-collector`, `…/mcp`, compliance) keep their own route labels and are
  never folded into the landing bucket.

## Approaches considered

**1. OTLP export as a second sink behind the existing registry (chosen).**
`Metrics` stays authoritative and fans out internally to the local atomics and
to OTel instruments. `/_stats` is untouched in phase 1 and gains the new
histograms later. The `opentelemetry*` dependencies live in a new
`microwave-otel` crate behind a trait, so `microwave-api` keeps its dependency
surface.

*Trade-off:* two renderers to keep in step. Mitigated by one catalogue plus a
mapping test, and by the local side being the only surface the existing suite
asserts on.

**2. OTel SDK as the single source, `/_stats` rendered through
`opentelemetry-prometheus`.** One set of instruments and no hand-rolled
exposition. *Trade-off:* rewrites `metrics.rs` and its tests, changes the
exposition order and formatting of a Chef-parity endpoint, adds an exporter
crate whose maintenance is slower than the SDK's, and still needs a special
path for the handler-side JSON gauges (maintenance, nodes, pool).

**3. Semconv names and labels on the scrape endpoint only, no SDK.** Cheapest,
gets the vocabulary right, but no OTLP push — half of what was asked for.

Approach 1 was chosen because it gets OTLP without putting the parity endpoint
or the request hot path at risk.

## Design

### Crate and trait boundary

The registry moves out of `microwave-api` into a new low-level workspace member
`crates/microwave-metrics`. It owns `Metrics`, `MetricsSnapshot`, the `Recorder`
trait, the bucket constants, and the route normalizer. `microwave-store`,
`microwave-objectstore`, and `microwave-api` depend on it (the two backends need
it for phase 3, the API re-exports `Metrics`/`MetricsSnapshot` so call sites keep
their current path). Nothing in it names an `opentelemetry` type.

```rust
pub trait Recorder: Send + Sync {
    fn http_request(&self, route: &str, method: &str, org: &str, status: u16, duration: Duration);
    fn mcp_tool(&self, tool: &str, org: &str, outcome: &str, duration: Duration);
    fn converge(&self, org: &str, status: &str);
    fn store_operation(&self, db_system: &str, operation: &str, outcome: &str, duration: Duration);
    fn search_request(&self, backend: &str, outcome: &str, duration: Duration);
    fn object_store_request(&self, backend: &str, op: &str, outcome: &str, duration: Duration);
}
```

`Metrics` holds `recorder: OnceLock<Arc<dyn Recorder>>`, attached once from
`serve`. `None` means local-only, which is every test and every default
deployment. Attach happens before the listener binds.

The circular-startup problem is solved by building the registry first:
`main` creates `Arc<Metrics>` before the store and passes it to
`AppState::with_metrics(...)`; `AppState::new`/`from_sqlite` keep creating their
own, so existing tests and call sites do not change.

New workspace member `crates/microwave-otel` holds every `opentelemetry*`
dependency:

- `init(config: &OtelConfig, metrics: Arc<Metrics>, scheme: &'static str) -> Result<OtelGuard, OtelError>`
  builds the `SdkMeterProvider` with a `PeriodicReader` over the OTLP/HTTP
  exporter wrapped in an error-tracking exporter, installs a per-instrument cardinality limit `View`, sets the global
  meter provider, and registers the `Recorder` and the observable gauges on one
  meter named `microwave-server`.
- `OtelGuard::shutdown(self).await` wraps `provider.shutdown()` in
  `tokio::time::timeout(config.shutdown_timeout())`.
- Initialization errors (unparseable endpoint, transport setup) are returned so
  `serve` can abort; export errors after startup are stored with
  `Metrics::set_export_error` and logged.

Observable gauges cannot read async state. A refresh task in the binary — it
owns the store and the keygen cache, `microwave-otel` does not — writes keygen
state, dependency states, maintenance state, node count, and pool gauges into
`Metrics` atomics, once immediately at startup and then every
`otel.gauge_refresh_seconds`. `/_stats` writes the same atomics on every scrape.
Uptime is read from the registry's start instant.

### Configuration

```toml
[otel]
# Off by default: enabling this is the first outbound connection Microwave makes.
enabled = false
endpoint = "http://127.0.0.1:4318"
# Base URL only; the signal path (/v1/metrics) is appended.
# Only "http/protobuf" is supported.
protocol = "http/protobuf"
# Export interval for the periodic reader.
interval_seconds = 60
# Upper bound for one export attempt.
timeout_ms = 10000
# How often the gauge refresh task samples store, keygen, and dependencies.
gauge_refresh_seconds = 60
# Budget for the final flush on shutdown.
shutdown_timeout_ms = 5000
# service.name resource attribute.
service_name = "microwave-server"
# deployment.environment resource attribute; omitted when unset.
environment = "production"
# "Name: value" entries, e.g. an Authorization header for the collector.
headers = []
# https collectors behind a private CA:
# tls_ca_path = "/etc/microwave/collector-ca.pem"
# insecure_skip_tls_verify = false
```

Environment overrides: `MW_OTEL__ENABLED`, `MW_OTEL__ENDPOINT`,
`MW_OTEL__PROTOCOL`, `MW_OTEL__INTERVAL_SECONDS`, `MW_OTEL__TIMEOUT_MS`,
`MW_OTEL__GAUGE_REFRESH_SECONDS`, `MW_OTEL__SHUTDOWN_TIMEOUT_MS`,
`MW_OTEL__SERVICE_NAME`, `MW_OTEL__ENVIRONMENT`, `MW_OTEL__HEADERS`
(comma-separated), `MW_OTEL__TLS_CA_PATH`, `MW_OTEL__INSECURE_SKIP_TLS_VERIFY`.

Validation (in `Config::validate`):

- `enabled` requires a non-empty `endpoint` that parses as `http` or `https`.
- `protocol` must be `http/protobuf`; an unsupported scheme is an error naming
  the key.
- `interval_seconds`, `timeout_ms`, `gauge_refresh_seconds`, and
  `shutdown_timeout_ms` must be non-zero.
- Every header must contain a colon and a non-empty name; the value may be
  empty.
- `tls_ca_path` and `insecure_skip_tls_verify` require an `https` endpoint; a
  configured CA file must exist.

`insecure_skip_tls_verify` means "do not verify the certificate chain", matching
`compliance_proxy`, and is independent of the scheme. Plain `http` endpoints
need neither key.

Secrets: `microwave-ctl set-secret otel headers` stores newline-separated
header lines. `bootstrap::apply_otel_secrets` (next to
`apply_bookshelf_secrets`) fills `otel.headers` only when the config list is
empty, so an environment or TOML value wins. `redact_config_for_output` renders
header values as `***` while keeping names, and every log line that mentions
headers prints names only.

### Resource attributes

`service.name` (config), `service.version` (`CARGO_PKG_VERSION`),
`service.instance.id` (fresh UUID per process), `host.name` (system hostname),
`deployment.environment` (only when set), and
`telemetry.sdk.{name,language,version}` from the SDK.

### Metric catalogue

Naming rule: OTLP uses dotted OTel names and semconv attribute names where a
semconv metric exists; the Prometheus exposition uses the snake-case
`microwave_server_*` spelling in the right-hand column. Both come from the same
instrument. Prometheus labels use the short spelling (`org`); OTLP keeps the
dotted attribute name (`microwave.organization`).

The existing counters stay exactly as they are on `/_stats` — `requests_total`
unlabelled and `responses_total{class}` — because tests and dashboards pin them.
Per-route and per-status counts come from the histogram's `_count`, so there is
no second labelled counter and no collision.

| Instrument (OTLP) | Type | Attributes | Exposition name |
| --- | --- | --- | --- |
| `http.server.request.duration` | histogram (s) | `http.request.method`, `http.route`, `http.response.status_code`, `url.scheme`, `microwave.organization` | `microwave_server_request_duration_seconds` |
| `microwave.server.requests` | observable counter | — | `microwave_server_requests_total` |
| `microwave.server.responses` | observable counter | `class` = `2xx`/`4xx`/`5xx` | `microwave_server_responses_total` |
| `microwave.server.authentication.failures` | observable counter | — | `microwave_server_authentication_failures_total` |
| `microwave.server.uptime` | observable gauge | — | `microwave_server_uptime_seconds` |
| `microwave.server.maintenance` | observable gauge | — | `microwave_server_maintenance` |
| `microwave.server.nodes` | observable gauge | — | `microwave_server_nodes` |
| `microwave.server.db.pool.connections` | observable gauge | `state` | `microwave_server_db_pool_connections` |
| `microwave.server.db.pool.connections.max` | observable gauge | — | `microwave_server_db_pool_connections_max` |
| `microwave.server.build.info` | observable gauge (1) | `service.version`, `indexing.mode` | `microwave_server_build_info` |
| `microwave.server.dependencies` | observable gauge | `dependency`, `state` = `up`/`down` | `microwave_server_dependency_up` |
| `microwave.keygen.*` | observable gauges | — | `microwave_server_keygen_*` |
| `microwave.mcp.tool.calls` | counter | `tool`, `outcome`, `microwave.organization` | `microwave_server_mcp_tool_calls_total` |
| `microwave.mcp.tool.duration` | histogram (s) | `tool`, `outcome`, `microwave.organization` | `microwave_server_mcp_tool_duration_seconds` |
| `microwave.converge.runs` | counter | `status`, `microwave.organization` | `microwave_server_converge_runs_total` |
| `microwave.store.operation.duration` | histogram (s) | `db.system`, `db.operation`, `outcome` | `microwave_server_store_operation_duration_seconds` |
| `microwave.search.request.duration` | histogram (s) | `backend`, `outcome` | `microwave_server_search_request_duration_seconds` |
| `microwave.object_store.request.duration` | histogram (s) | `backend`, `operation`, `outcome` | `microwave_server_object_store_request_duration_seconds` |

The `microwave.keygen.*` family covers `capacity`, `cached`, `workers`,
`worker_failures`, `warm_up_failures`, `generated_on_demand`, and
`worker_healthy`, mirroring the `/_status` keygen document.

`http.response.status_code` is the exact code, not a class, so 401, 403, 404 and
503 stay distinguishable. Latency histograms use the OTel default buckets in
seconds, capped at 10 s, plus an explicit `+Inf`; the boundaries are one `const`
array shared by both sinks and a test asserts they are strictly increasing.

`microwave.organization` stays on the request histogram. The server is
multi-org, so both views have to work: whole-server numbers come from an
aggregation without the org label (`sum without (microwave_organization)` in
PromQL), per-org numbers from grouping by it. The guide shows both queries. The
same applies to the MCP and converge metrics, which keep the label for the same
reason.

### Cardinality

`route × method × status × org` is the whole series set of the request
histogram. Method and status are naturally bounded, the route is a template, and
the org set is bounded by the organization count. Object names are never labels.
Because the org label multiplies this set, the request histogram gets the higher
limit below; every other instrument stays at the default.

Both sinks enforce a hard per-instrument limit:

- default 2000 series, 10 000 for `http.server.request.duration`;
- local: an insert-only `RwLock<HashMap<SeriesKey, Buckets>>`; once the map is at
  the limit, unknown keys collapse onto `route="_other"` (or the equivalent
  `tool`/`operation` value), counted once per status and org;
- OTLP: an `opentelemetry_sdk` cardinality-limit `View` at the same numbers.

The overflow series name is documented, and so is the symptom: once `_other`
grows, the deployment is over budget. The docs state the rule plainly: adding a
label means proving it is bounded.

The local store is always on, because `/_stats` needs the histogram with OTel
disabled. The hot path is one read lock plus one bucket increment; a
double-check under the write lock handles a first insert.

### Route labels

`observe_request` is the outermost layer, so it measures the whole request —
queue, auth, handler, database — and sees every response, including the 401s,
403s, maintenance 503s, and landing pages that never reach a handler. It does
the recording. That answers both "which route is slow" and "which organization
drives failures".

`MatchedPath` still has to come from route matching. A second, thin middleware
added with `Router::layer` on the inner router runs after matching, extracts
`MatchedPath`, and inserts it into the *response* extensions (`http.route`).
The outer layer reads it after `next.run()`.

Requests without a template (`/version` on the outer router, unknown paths, the
landing assets) fall back to `metrics_route(path)`: strip the query, replace
UUID segments with `:uuid`, replace the segment after a known id position with
`:id`, and collapse everything else to `unknown`. A table-driven test pins the
cases. MCP is merged into the inner router, so MCP requests do get templates.

`microwave.organization` is parsed from the URI path before authentication, so
it is also available for unsigned traffic; `/organizations/{org}/validate/...`
and `/organizations/{org}/mcp` use the path segment, and requests without one
use `_default`.

### Data sources

- **HTTP**: the middleware above.
- **MCP**: `record_mcp_tool` in `mcp/tools.rs`, next to the existing structured
  log line, using the same `outcome` and `elapsed` values so log line and metric
  cannot disagree.
- **Converge**: in `report_ingest`/`data_collector`, once the run status is
  known, before persistence or forwarding.
- **Store**: `microwave-store` gains a `Metrics`-free `Timed` wrapper used by
  the PostgreSQL and SQLite backends for the prepared-statement operations on
  the hot path (node save, object fetch, search-adjacent reads), recording
  `db.operation` per repository method.
- **Search / object store**: in the opensearch client and the S3/local object
  store paths, recording backend, operation, and outcome.
- **Keygen / dependencies / pool / nodes / maintenance**: the gauge refresh task
  and the `/_stats` handler.

### Runtime behaviour

- Disabled: `recorder` stays `None`, no provider, no gauge task, no sockets.
- Enabled and unreachable: background export failures log at `warn` with the
  endpoint and error class, deduplicated to one line per interval, and set
  `Metrics`'s last-export-error slot. Requests are unaffected.
- Shutdown: after the graceful drain, `OtelGuard::shutdown` runs under the
  configured budget, then the process exits. A hung collector therefore costs
  at most `shutdown_timeout_ms`.
- `serve --check` validates and prints the redacted `[otel]` state, then returns
  without dialing.

### `/_stats`

Phase 1 keeps the document and the text body byte-identical. Phase 2 appends the
histograms (`_bucket`/`_sum`/`_count` lines with the label set above) and the
new counters before the pool block. JSON gains an `otel` object with `enabled`,
`endpoint`, and `last_export_error` in phase 2; the endpoint is printed without
credentials. The keygen and dependency gauges follow the existing JSON structure
and stay additive.

`last_export_error` crosses the crate boundary as plain state on `Metrics`
(`set_export_error`, `clear_export_error`, `export_error`), set and cleared by
the exporter wrapper, so the API still names no OTel type.

### Documentation and claims

The guide is a published page, `docs/opentelemetry.md`, not a section buried in
the deployment runbook. The deployment block stays the place for the keys and
validation errors an operator needs while editing the file; the guide carries
the how-to.

- `docs/opentelemetry.md`: what OTel export is for, the off-by-default promise,
  a collector config snippet, enabling step by step, the egress note (one
  outbound connection to `host:port`, and that a NetworkPolicy or egress
  allowlist needs it), the appended `/v1/metrics` path, header secrets, the TLS
  keys, the full metric table with types and attributes, the cardinality rules,
  how to read the exported data in Prometheus/Grafana, and troubleshooting
  (silent collector, `last_export_error`, dropped series).
- `docs/index.md`: entry under Start here.
- `website/docs/docusaurus.config.ts`: `opentelemetry.md` in `docs.include`.
- `website/docs/sidebars.ts`: `{type: 'doc', id: 'opentelemetry', label:
  'OpenTelemetry metrics'}` in the Operator guides category, so the page is in
  the nav and reachable from every other page.
- `packaging/microwave.toml.example`: commented `[otel]` block, disabled, with
  the egress note and a pointer to the guide.
- `docs/deployment.md`: `#### OpenTelemetry metrics` under Configuration (all
  keys, secret handling, validation errors, the appended `/v1/metrics` path)
  plus a Monitoring subsection with a collector config snippet, the cardinality
  rules, an example scrape of the collector's Prometheus exporter, and the
  metric table, linking the guide.
- `README.md`: one line in the monitoring area linking the guide.
- `docs/claims.toml`: claims for "off by default", "the guide is published on
  the docs site" (surfaces: the guide file, `include`, `sidebars.ts`), the
  secret path, the redacted config output, the metric catalogue, and the
  `/_stats` additions, each with a `grep` surface.
- `docs/mcp.md` only if the MCP tool metrics change operator-visible behaviour.
- The homepage gets a link only if it grows or already has a monitoring
  section; it does not get a new section just for this.

`docs/opentelemetry.md` must keep the site build green: page-relative links use
`./guide.md`, links out of `docs/` use the `../` form the plugin rewrites, and
no raw HTML anchors.

## Verification

- `cargo test -p microwave-config`: parsing, defaults, every validation error,
  env overrides, header redaction, `https`-only rules for the TLS keys.
- `cargo test -p microwave-metrics`: catalogue mapping table, bucket
  monotonicity, route normalization table, series-cap overflow, `Recorder`
  never called when unattached.
- `cargo test -p microwave-api`: `/_stats` regression plus new histogram
  assertions, MCP and converge counters, org parsing for signed, unsigned, and
  `/organizations/{org}/...` paths.
- `cargo test -p microwave-otel`: instruments record into an in-memory
  `SdkMeterProvider`; a shutdown flushes a pending batch; the cardinality `View`
  collapses at the limit.
- Integration with a stub OTLP receiver (axum test server, `POST /v1/metrics`,
  returns 200) decoding the body with `opentelemetry-proto` (dev-dependency) and
  asserting resource `service.name`, the request histogram, and that a request
  still succeeds while the receiver answers 500.
- A refresh-task test: keygen, maintenance, node and pool gauges are populated
  before the first interval elapses.
- `cd website/docs && npm run build`: the new guide is included, in the sidebar,
  and every link resolves (`onBrokenLinks: 'throw'`).
- `serve --check` with `[otel] enabled = true` succeeds with no collector.
- Manual end-to-end look through the `observability` profile: `docker compose
  --profile observability up -d --build`, check the collector receives the
  server's series and Prometheus scrapes them, then `docker compose --profile
  observability down -v --rmi local` so no container, volume, or locally built
  image is left behind.
- `cargo fmt --all`, `cargo clippy --workspace --all-targets`,
  `scripts/run-pedant.sh <tag>` for the touched suites.

## Implementation phases

All three phases ship in this change; they are sequenced so each one is green on
its own.

1. **Plumbing**: `microwave-metrics` registry move plus `Recorder` trait and
   fan-out, `AppState::with_metrics`, `microwave-otel` crate, `[otel]` config
   plus validation, provider lifecycle and shutdown, route-label middleware and
   normalizer, existing counters/gauges mirrored, resource attributes, secret
   path, docs skeleton.
2. **Request depth**: histograms in `/_stats` and OTLP, MCP tool metrics,
   converge counter, keygen and dependency gauges, gauge refresh task, `otel`
   JSON object.
3. **Backend depth**: store, search, and object-store histograms; the
   `observability` profile in the root `docker-compose.yml` with collector and
   Prometheus; the finished guide and all claims.

The guide, its `include`/sidebar entry, and the claims land with phase 1 so the
page exists before the metrics it describes are complete; phase 2 and 3 fill the
table in.

`website/**` plus `docs/opentelemetry.md` means both the docs build and
`cargo test` are the verification for a docs commit (see `AGENTS.md`).

## Observability profile

`docker-compose.yml` gains an `otel-collector` and a `prometheus` service behind
`profiles: [observability]`, so a plain `docker compose up -d` stays unchanged.
The collector receives OTLP/HTTP from the `microwave` service by DNS name
(`http://otel-collector:4318`), and Prometheus scrapes the collector's
Prometheus exporter — the reference topology the guide describes. The profile
builds nothing from the repository, and `down -v --rmi local` removes it fully.

A short "Watching it locally" section in the guide walks through it.

## Left to the builder

- The exact `metrics_route` fallback table and its test cases.
- Whether `db.operation` values are derived from the repository method name or a
  hand-maintained enum.
- The collector and Prometheus image tags and the profile's Prometheus config.
- Whether `microwave_server_build_info` is worth a series at all.
- Log wording for export failures.

## Decisions taken here

- Approach 1, not the SDK-as-single-source rewrite.
- `[otel]`, not `[telemetry]`.
- HTTP/protobuf only; no gRPC.
- One catalogue with a mapping test; legacy counters stay unlabelled.
- `microwave.organization` on the request histogram, so whole-server and
  per-org queries both work.
- Header values via the secret store; config and environment win.
- `insecure_skip_tls_verify` plus `tls_ca_path`, mirroring `compliance_proxy`.
- Startup fails closed on a bad `[otel]` block, export fails open afterwards.
- Flush budget on shutdown, not an unbounded flush.
- No dial during `serve --check`.
- Route templates for labels, never object names.
- Registry in its own crate so the store and object store can reach it.
- Series cap with `_other` overflow on both sinks, higher for the request
  histogram.
- Full-request durations, recorded outside authentication.
- All three phases in this change.
- The reference stack lives in the root `docker-compose.yml` as a profile.
- The guide is a published docs page with a sidebar entry.

## Open questions

None outstanding.



## Rollout

Ship disabled. Operators enable `[otel] enabled = true` plus an endpoint, then
watch the collector's self-metrics for dropped series before widening the
attribute set. `/_stats` keeps working throughout, so a deployment can be
monitored before the collector exists.

For a local look, `docker compose --profile observability up -d --build` starts
the reference collector and Prometheus; the guide links the queries to start
with.
