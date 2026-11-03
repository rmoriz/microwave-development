# OpenTelemetry metrics

Microwave Server can push its own metrics to an OpenTelemetry collector over
OTLP/HTTP. Without it, an operator on `GET /_stats` sees total requests, a
response-class split, authentication failures, uptime, and a handful of gauges
— and has to guess which route is slow, which organization drives failures, and
whether converges succeed. The exported series answer those questions: request
durations per route template and status code, MCP tool calls and durations,
converge outcomes, store, search and object-store latency, and the keygen,
dependency and pool gauges.

The export is a second sink. `/_stats` keeps rendering *some* of the same
numbers — the request histogram, the legacy counters, uptime, maintenance,
build info, dependencies, keygen and pool — in Prometheus text form, and stays
the scrape surface when no collector exists; MCP, converge, store, search and
object store go out over OTLP only. Both surfaces come from one instrument
catalogue ([`crates/microwave-otel/src/lib.rs`](../crates/microwave-otel/src/lib.rs)).

Metrics are opt-in and **off by default**. A default deployment makes no
outbound connection.

## What enabling costs

Enabling `[otel]` makes the first outbound connection the server opens that is
not a Chef request: one periodic OTLP/HTTP request from the server to the
configured collector address, every `interval_seconds`, plus one connection for
the final flush on shutdown. Nothing else changes; requests never wait on the
exporter.

Budget for that in the network policy:

- Allow egress from the server to the collector's `host:port` (the default port
  for OTLP/HTTP is `4318`). A default-deny `NetworkPolicy` or an egress
  allowlist blocks the export until the rule exists.
- An unreachable collector does not fail startup or a request. The export fails
  open: requests keep being served, `/` and the API keep answering, and the
  failure lands in `last_export_error` (see
  [Troubleshooting](#troubleshooting)).
- A malformed `[otel]` block is different: `serve` and `check-config` fail
  closed before the listener binds, so a typo is caught at deploy time.

The server dials nothing at startup. The first export attempt happens one
interval after the start.

## Enable it

1. Start from the collector side. The reference configuration below receives
   OTLP/HTTP protobuf and re-exposes it for Prometheus; adapt it to your own
   pipeline:

   ```yaml
   receivers:
     otlp:
       protocols:
         http:
           endpoint: 0.0.0.0:4318

   processors:
     batch:

   exporters:
     prometheus:
       endpoint: 0.0.0.0:8889
       send_timestamps: true
       translation_strategy: UnderscoreEscapingWithSuffixes

   extensions:
     health_check:
       endpoint: 0.0.0.0:13133

   service:
     extensions: [health_check]
     pipelines:
       metrics:
         receivers: [otlp]
         processors: [batch]
         exporters: [prometheus]
     telemetry:
       metrics:
         readers:
           - pull:
               exporter:
                 prometheus:
                   host: 0.0.0.0
                   port: 8888
   ```

   The `batch` processor only batches. Neither a `memory_limiter` processor nor
   an exporter `sending_queue` is part of this reference config, and the
   Prometheus exporter keeps no retention of its own.

2. Point the server at the collector's base URL and switch the export on:

   ```toml
   [otel]
   enabled = true
   endpoint = "http://otel-collector:4318"
   ```

   The endpoint is a base URL. The signal path `/v1/metrics` is appended, so
   `http://otel-collector:4318` becomes
   `http://otel-collector:4318/v1/metrics`. An endpoint that already ends in
   `/v1/metrics` is not appended twice. `http` and `https` are accepted;
   anything else is a validation error. Credentials in the URL (`user:pass@`)
   are accepted and printed without the userinfo in `/_stats` and the logs.

3. Keep `protocol = "http/protobuf"`. It is the only supported protocol and the
   default; a different value fails validation with the key named.

4. Validate the file and restart the server:

   ```sh
   microwave-ctl --config /etc/microwave/microwave.toml check-config
   systemctl restart microwave
   ```

   `check-config` and `serve --check` validate the block without dialing the
   collector. Validation covers the endpoint scheme, the protocol, a non-zero
   interval, timeout, gauge refresh and shutdown budget, the header syntax, and
   the `https`-only TLS keys (below).

5. Confirm the export. `GET /_stats` always carries an `otel` object, so it
   reports the state:

   ```json
   "otel": { "enabled": true, "endpoint": "http://otel-collector:4318", "last_export_error": null }
   ```

   `last_export_error` is `null` while the last attempt succeeded, and names the
   endpoint and the error class while it failed. The object is `enabled: false`
   and `endpoint: null` while the export is off, even when an endpoint is
   configured.

All keys, their defaults and their `MW_OTEL__*` overrides are in
[`docs/deployment.md`](./deployment.md#opentelemetry-metrics).

### Headers and the secret store

Collectors commonly want an `Authorization` header. Header values are secrets,
so they belong in the secret store instead of the configuration file:

```sh
microwave-ctl --config /etc/microwave/microwave.toml \
  set-secret otel headers 'Authorization: Bearer <token>'
```

The secret holds one `Name: value` entry per line; a value may be empty and the
name may not. The stored headers are used only when `otel.headers` is empty, so
a TOML list or `MW_OTEL__HEADERS` wins. The block is validated again after the
secret is read, so a malformed stored header fails startup rather than the first
export. `microwave-server config` prints header names with `***` values; no log
line includes them.

### TLS

Collectors reached over `https` need no extra key when the certificate chains to
a public CA. For a private CA, point at a PEM bundle:

```toml
[otel]
enabled = true
endpoint = "https://collector.example.com:4318"
tls_ca_path = "/etc/microwave/collector-ca.pem"
```

The CA file must exist and is read at startup. `insecure_skip_tls_verify = true`
turns certificate-chain verification off, for a self-signed collector in a test
setup; keep it off in production. Both keys require an `https` endpoint and are
rejected on a plain `http` one. TLS goes through rustls.

## Metric reference

The two name columns are the two surfaces. `OTLP/Prometheus (collector)` is the
instrument name and the spelling the collector's Prometheus exporter gives it;
`/_stats` is the name on the local scrape surface, and `—` where `/_stats` does
not render the instrument at all. Names with a `_total` suffix are monotonic
counters and are queried with `rate()`.

| Instrument | Type | Attributes | OTLP/Prometheus (collector) | `/_stats` |
|---|---|---|---|---|
| `http.server.request.duration` | histogram (s) | `http.request.method`, `http.route`, `http.response.status_code`, `url.scheme`, `microwave.organization` | `http_server_request_duration_seconds` | `microwave_server_request_duration_seconds` (short `org` label) |
| `microwave.server.requests` | observable counter | — | `microwave_server_requests_total` | `microwave_server_requests_total` |
| `microwave.server.responses` | observable counter | `class` = `2xx`/`4xx`/`5xx` | `microwave_server_responses_total` | `microwave_server_responses_total` |
| `microwave.server.authentication.failures` | observable counter | — | `microwave_server_authentication_failures_total` | `microwave_server_authentication_failures_total` |
| `microwave.server.uptime` | observable gauge | — | `microwave_server_uptime_seconds` | `microwave_server_uptime_seconds` |
| `microwave.server.maintenance` | observable gauge | — | `microwave_server_maintenance` | `microwave_server_maintenance` |
| `microwave.server.nodes` | observable gauge | — | `microwave_server_nodes` | `microwave_server_nodes` |
| `microwave.server.db.pool.connections` | observable gauge | `state` = `idle`/`in_use` | `microwave_server_db_pool_connections` | `microwave_server_db_pool_connections` |
| `microwave.server.db.pool.connections.max` | observable gauge | — | `microwave_server_db_pool_connections_max` | `microwave_server_db_pool_connections_max` |
| `microwave.server.build.info` | observable gauge (1) | `service.version`, `indexing.mode` | `microwave_server_build_info` | `microwave_server_build_info` |
| `microwave.server.dependencies` | observable gauge | `dependency`, `state` = `up`/`down` | `microwave_server_dependencies` | `microwave_server_dependency_up` |
| `microwave.keygen.capacity`, `microwave.keygen.cached`, `microwave.keygen.workers`, `microwave.keygen.worker_failures`, `microwave.keygen.warm_up_failures`, `microwave.keygen.generated_on_demand`, `microwave.keygen.worker_healthy` | observable gauges | — | `microwave_keygen_*` | `microwave_server_keygen_*` |
| `microwave.mcp.tool.calls` | counter | `tool`, `outcome`, `microwave.organization` | `microwave_mcp_tool_calls_total` | — |
| `microwave.mcp.tool.duration` | histogram (s) | `tool`, `outcome`, `microwave.organization` | `microwave_mcp_tool_duration_seconds` | — |
| `microwave.converge.runs` | counter | `status`, `microwave.organization` | `microwave_converge_runs_total` | — |
| `microwave.store.operation.duration` | histogram (s) | `db.system`, `db.operation`, `outcome` | `microwave_store_operation_duration_seconds` | — |
| `microwave.search.request.duration` | histogram (s) | `backend`, `outcome` | `microwave_search_request_duration_seconds` | — |
| `microwave.object_store.request.duration` | histogram (s) | `backend`, `operation`, `outcome` | `microwave_object_store_request_duration_seconds` | — |

`http.route` is a route template, never a path with IDs, so the label set stays
bounded. `http.response.status_code` is the exact code, so `401`, `403`, `404`
and the maintenance `503` stay distinguishable. Durations are seconds, in the
OpenTelemetry default buckets up to 10 s plus an explicit `+Inf`. The
observable counters and gauges are read from the process when the collector
scrapes; the `microwave.server.*` traffic counters mirror the legacy `/_stats`
counters. `microwave.server.build.info` is the constant `1` with the version and
the active search backend, which makes "which build and mode is this?" a join
instead of a guess.

Resource attributes on every batch are `service.name` (from `otel.service_name`,
default `microwave-server`), `service.version`, `service.instance.id`,
`host.name`, and `deployment.environment` when `otel.environment` is set, plus
the SDK's `telemetry.sdk.*`. The instance id is fresh per process, so a restart
is a new series set.

The MCP tool metrics sit next to the endpoint's structured log line and use the
same outcome and duration values; the endpoint itself is described in
[the MCP guide](./mcp.md).

## Whole-server and per-org queries

`microwave.organization` is on the request histogram because the server is
multi-org: the same route can be slow for one organization and fine for another.
Aggregate it away for the whole-server view and group by it for the per-org one.
Requests on a default-org compatibility path (`/nodes`, `/environments`, …)
carry no organization segment and are reported as `_default`
([`crates/microwave-metrics/src/route.rs`](../crates/microwave-metrics/src/route.rs)),
so a chef-client fleet lands in that bucket.

Prometheus escapes the dots to underscores, so the label is
`microwave_organization` there. Whole-server, the p95 per route:

```promql
histogram_quantile(
  0.95,
  sum without (microwave_organization) (
    rate(http_server_request_duration_seconds_bucket[5m])
  )
)
```

Per organization across all routes, the same quantile:

```promql
histogram_quantile(
  0.95,
  sum by (microwave_organization) (
    rate(http_server_request_duration_seconds_bucket[5m])
  )
)
```

The same split works for `microwave.mcp.tool.calls` and
`microwave.converge.runs`, which keep the label for the same reason.

`_count` is the observation counter of the histogram. Per route and status,
`rate(http_server_request_duration_seconds_count[5m])` is the request rate the
legacy unlabelled `requests_total` cannot give. On `microwave.search.request.duration`
and `microwave.object_store.request.duration`, `_count` is **not** a "searches
per second" or "object fetches per second" figure: the `/_status` dependency
probes and every gauge-refresh tick hit the search and object-store clients too,
so both add to the count at the probe and refresh cadence.

`/_stats` spells the same request histogram
`microwave_server_request_duration_seconds`, with the short `org` label; read it
with `curl -s 'http://127.0.0.1:8080/_stats?format=prometheus'`, because a plain
`/_stats` is JSON. The exported series use the OTel semconv name and the dotted
attribute, so a dashboard that reads both should not mix them up.

## Cardinality

`route × method × status × org` is the series set of the request histogram.
Method and status are bounded by the protocol, the route is a template, and the
organization count is bounded by the deployment. Object names, node names,
client names and cookbook names are never labels for that reason.

Both sinks enforce a hard per-instrument limit: `2,000` series for every
instrument, `10,000` for `http.server.request.duration`, which multiplies route
by org. What happens at the limit differs per sink:

- `/_stats` keeps the series it already has and does not store a new label set;
  the new set collapses onto one series whose unbounded label reads `_other` —
  `route="_other"` on the request histogram. The local store collapses the same
  way on `tool`, `operation` and `backend`, which `/_stats` does not render.
- The export collapses the *whole attribute set* into a single overflow series
  per instrument, so it adds one series, not one per remaining label
  combination, and that series carries `otel.metric.overflow="true"`
  (`otel_metric_overflow="true"` in Prometheus).

`_other` on `/_stats` is the symptom: the deployment is over budget, usually
through an organization count that grew or a client generating unusual routes.
In the export the symptom is an `otel_metric_overflow="true"` series whose count
keeps rising. Both mean the same thing, and the rule for adding a label is that
you can prove it is bounded.

## Watching it locally

The root `docker-compose.yml` carries an `observability` profile: an OTLP
collector on `4318`, its Prometheus exporter on `8889`, and a Prometheus on
`9090`. Only `4318`, `13133` and `9090` are published on the host; `8889` and
the collector's own telemetry on `8888` stay inside the compose network, where
Prometheus reaches them. The compose `microwave` service already sets
`MW_OTEL__ENABLED=true` and points the endpoint at the collector, so the profile
is the only switch:

```sh
docker compose --profile observability up -d --build
```

Check that the collector is up and that Prometheus found it:

```sh
curl -fsS http://127.0.0.1:13133/
curl -s http://127.0.0.1:9090/api/v1/targets
```

Then ask the question the export exists for, first as a rate and then as a
quantile:

```sh
curl -s http://127.0.0.1:9090/api/v1/query \
  --data-urlencode 'query=http_server_request_duration_seconds_count'
curl -s http://127.0.0.1:9090/api/v1/query \
  --data-urlencode 'query=histogram_quantile(0.95, sum without (microwave_organization) (rate(http_server_request_duration_seconds_bucket[5m])))'
```

`/_stats` stays reachable next to it and shows the same histogram under its
`microwave_server_*` name, so a single request appears on both surfaces:

```sh
curl -s 'http://127.0.0.1:8080/_stats?format=prometheus'
```

Tear the profile down again when you are done; `down -v --rmi local` removes
the containers and volumes, and the two pulled images keep their upstream tags:

```sh
docker compose --profile observability down -v --rmi local
docker rmi otel/opentelemetry-collector-contrib:0.164.0 prom/prometheus:v3.15.1
```

## Troubleshooting

**`last_export_error` is set.** `GET /_stats` reports the failing endpoint and
the error class in the `otel` object and the server logs one `warn` line per
interval. Start with the address: is the collector listening, does the
`NetworkPolicy` allow the egress, does a proxy need the header secret? The slot
clears itself on the next successful export. Requests are unaffected
throughout, which is why this is the first place to look rather than an error
page.

**The collector is silent, `/_stats` looks fine.** An accepted connection is not
an accepted payload. Check the collector's own logs and its receiver errors: a
protocol mismatch (gRPC listener, protobuf disabled), a wrong signal path, or a
`413` from a proxy in front of it all show there. The reference config's health
extension answers `curl -fsS http://<collector>:13133/`.

**Series are dropped at the collector.** Memory limiters and the exporter queue
drop data under pressure before it ever reaches the backend. Neither a
`memory_limiter` processor nor an exporter `sending_queue` is in the reference
config, so add a limiter or a queue when you need them. In the reference profile
the collector publishes its own telemetry on `8888` and Prometheus scrapes it,
so `otelcol_receiver_refused_metric_points` and
`otelcol_exporter_send_failed_metric_points` are visible next to the data. Raise
the limiter or the queue — the reference config keeps no retention of its own —
but check [Cardinality](#cardinality) first: an overflow series set that grew
(`_other` on `/_stats`, `otel_metric_overflow="true"` in the export) is usually
a label problem, not a collector problem.

**Nothing arrives at all.** Confirm the export is on in the running process, not
just in the file: `/_stats` must show `"enabled": true`, and a restart is
required after a configuration change. `serve --check` validates the block
without dialing, so it passing does not prove reachability.

**Numbers look doubled.** Two exporters or two collectors in the pipeline
duplicate every point. Each instance also exports its own `service.instance.id`,
so check the target list for the same instance scraped twice.
