# OpenTelemetry Metrics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Export Microwave's HTTP and domain metrics over OTLP/HTTP to a collector, opt-in and off by default, while `/_stats` keeps its Chef-parity contract and gains the same instruments.

**Architecture:** The in-process registry moves into a new low-level crate `microwave-metrics` that owns the counters, gauges, histograms, the `Recorder` trait, the route normalizer, and the series caps. A new `microwave-otel` crate implements `Recorder` on top of `opentelemetry_sdk` 0.33.1 and an OTLP/HTTP exporter; the binary builds the registry first, attaches the recorder, and wires the provider through startup and shutdown. Call sites keep calling `state.metrics.*`; the fan-out to local counters and OTel instruments happens inside the registry, so both sinks share one catalogue.

**Tech Stack:** Rust 2021, axum 0.8, tokio, `opentelemetry` / `opentelemetry_sdk` / `opentelemetry-otlp` 0.33.1 (http-proto + reqwest, rustls), `opentelemetry-proto` (dev only, for decoding in tests).

**Spec:** `docs/superpowers/specs/2026-11-03-opentelemetry-metrics-design.md`

## Global Constraints

- Pin `opentelemetry`, `opentelemetry_sdk`, `opentelemetry_otlp` at `=0.33.1` in `[workspace.dependencies]`; `opentelemetry-proto` at `=0.33.1` as a workspace dev-dependency.
- rustls only. No OpenSSL. `opentelemetry-otlp` features: `http-proto`, `reqwest-client`, `reqwest-rustls`, `tls-webpki-roots`.
- OTel is off by default: `otel.enabled` defaults to `false`, and with it off no socket, task, or provider exists.
- No crate outside `microwave-otel` names an `opentelemetry` type.
- One catalogue for both sinks: OTLP name on the left, Prometheus `microwave_server_*` name on the right, pinned by a test.
- Counters get `_total` only in the Prometheus spelling.
- Prometheus labels use the short spelling (`org`); OTLP keeps `microwave.organization`.
- `LATENCY_BUCKETS_SECONDS` is one `const` array shared by both sinks; strips are seconds.
- Series caps: `DEFAULT_SERIES_LIMIT = 2_000`, `REQUEST_SERIES_LIMIT = 10_000`, overflow label `"_other"`.
- `microwave.organization` on the request histogram; whole-server aggregation via `sum without (microwave_organization)`.
- `/_stats` keeps its existing counters, JSON keys, Basic auth, and maintenance exemption byte-for-byte in phase 1. Text additions only append.
- MCP keeps its structured log line with organization, principal, tool, duration, result count, outcome, and target.
- Metric values are numbers only; no node, client, role, or cookbook names as labels.
- The guide is published: `docs/opentelemetry.md`, listed in `website/docs/docusaurus.config.ts` `include` and `website/docs/sidebars.ts`.
- Every new prose surface needs a `docs/claims.toml` claim (`scripts/check-claims.py` fails otherwise).

## Review Focus

- Unsigned requests and the landing page (with its hardening headers) must be measured as landing responses, never as a `401` on an API route, and never with an object name as label.
- `/version`, `/_status`, `/_stats`, `/id*`, `/bookshelf/*`, `…/validate`, `…/required_recipe`, `…/data-collector`, `…/mcp` and compliance paths get their own stable route label, not `unknown`.
- A collector that is down or hangs: request latency unaffected, shutdown capped at `shutdown_timeout_ms`, `last_export_error` set, server keeps serving.
- A series explosion (many orgs, many routes): the cap collapses to `_other` and the rendered Prometheus text stays valid, with no duplicate label sets.
- Header secrets: `microwave-server config` prints `***`, values survive spaces, an empty value is legal.

---

### Task 1: Registry crate and route normalizer

**Files:**
- Create: `crates/microwave-metrics/Cargo.toml`
- Create: `crates/microwave-metrics/src/lib.rs`
- Create: `crates/microwave-metrics/src/registry.rs`
- Create: `crates/microwave-metrics/src/recorder.rs`
- Create: `crates/microwave-metrics/src/histogram.rs`
- Create: `crates/microwave-metrics/src/route.rs`
- Delete: `crates/microwave-api/src/metrics.rs`
- Modify: `Cargo.toml` (workspace members, workspace deps)
- Modify: `crates/microwave-api/Cargo.toml`
- Modify: `crates/microwave-api/src/lib.rs:25,50`
- Modify: `crates/microwave-api/src/state.rs:2,86,134`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `microwave_metrics::{Metrics, MetricsSnapshot, Recorder, KeygenGauges, PoolGauges, DependencyState}`
  - Series structs, one per instrument, labels as `String` (except `HttpSeries::status: u16`): `HttpSeries { route, method, org, status }`, `McpSeries { tool, org, outcome }`, `StoreSeries { db_system, operation, outcome }`, `SearchSeries { backend, outcome }`, `ObjectStoreSeries { backend, operation, outcome }`, `ConvergeSeries { org, status }` — every series also carries `count: u64`, `sum: f64`, and `buckets: Vec<u64>` of `LATENCY_BUCKETS_SECONDS.len() + 1` slots; `ConvergeSeries` is the only counter and leaves `sum`/`buckets` empty
  - Accessors: `http_series()`, `mcp_series()`, `converge_series()`, `store_series()`, `search_series()`, `object_store_series()`
  - Gauge payloads: `KeygenGauges { cached, capacity, workers, worker_failures, warm_up_failures, generated_on_demand, worker_healthy }` (mirroring `microwave_crypto::KeygenStatus`, `bool` for the last), `PoolGauges { idle, in_use, max }`, `DependencyState { name: &'static str, up: bool }`
  - `microwave_metrics::{LATENCY_BUCKETS_SECONDS, DEFAULT_SERIES_LIMIT, REQUEST_SERIES_LIMIT, OVERFLOW_LABEL}`
  - `microwave_metrics::{metrics_route, organization_label}`
  - `Metrics::{new, attach, record_request, record_response, record_http, record_mcp_tool, record_converge, record_store_operation, record_search_request, record_object_store_request, snapshot, set_export_error, export_error, clear_export_error, set_keygen, set_dependencies, set_maintenance, set_nodes, set_pool, uptime_seconds}` and the accessors above

- [ ] **Step 1: Write the failing tests**

`crates/microwave-metrics/src/route.rs` (tests at the bottom of the file, module compiles with the impl stubs):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_templates_and_drops_ids() {
        assert_eq!(metrics_route("/organizations/acme/nodes/web1"), "/organizations/:org/nodes/:id");
        assert_eq!(metrics_route("/organizations/acme/cookbooks/apache/1.2.3"), "/organizations/:org/cookbooks/:id/:id");
        assert_eq!(
            metrics_route("/organizations/acme/nodes/0f1c2b3a-4d5e-6f70-8192-a3b4c5d6e7f8"),
            "/organizations/:org/nodes/:uuid"
        );
        assert_eq!(metrics_route("/"), "/");
        assert_eq!(metrics_route("/version"), "/version");
        assert_eq!(metrics_route("/_stats?format=json"), "/_stats");
        assert_eq!(metrics_route("/bookshelf/abc123"), "/bookshelf/:id");
        assert_eq!(metrics_route("/favicon.svg"), "/favicon.svg");
        assert_eq!(metrics_route("/totally-unknown"), "unknown");
    }

    #[test]
    fn organization_comes_from_the_path() {
        assert_eq!(organization_label("/organizations/acme/nodes/web1"), "acme");
        assert_eq!(organization_label("/organizations/acme/validate/nodes/web1"), "acme");
        assert_eq!(organization_label("/organizations/acme/mcp"), "acme");
        assert_eq!(organization_label("/_stats"), "_default");
        assert_eq!(organization_label("/organizations/"), "_default");
    }
}
```

`crates/microwave-metrics/src/registry.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    struct Counting(std::sync::atomic::AtomicUsize);

    impl Recorder for Counting {
        fn http_request(&self, _r: &str, _m: &str, _o: &str, _s: u16, _d: Duration) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn mcp_tool(&self, _t: &str, _o: &str, _oc: &str, _d: Duration) {}
        fn converge(&self, _o: &str, _s: &str) {}
        fn store_operation(&self, _d: &str, _op: &str, _oc: &str, _du: Duration) {}
        fn search_request(&self, _b: &str, _oc: &str, _d: Duration) {}
        fn object_store_request(&self, _b: &str, _op: &str, _oc: &str, _d: Duration) {}
    }

    #[test]
    fn attach_happens_once_and_fans_out() {
        let metrics = Metrics::new();
        let recorder = Arc::new(Counting(Default::default()));
        assert!(metrics.attach(recorder.clone()));
        assert!(!metrics.attach(Arc::new(Counting(Default::default()))));
        metrics.record_http("/_status", "GET", "_default", 200, Duration::from_millis(3));
        assert_eq!(recorder.0.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn records_every_family_without_a_recorder() {
        let metrics = Metrics::new();
        metrics.record_http("/_status", "GET", "_default", 200, Duration::from_millis(1));
        metrics.record_mcp_tool("list_nodes", "acme", "ok", Duration::from_millis(2));
        metrics.record_converge("acme", "success");
        metrics.record_store_operation("sqlite", "find_node", "ok", Duration::from_millis(1));
        metrics.record_search_request("embedded", "ok", Duration::from_millis(1));
        metrics.record_object_store_request("local", "get", "error", Duration::from_millis(1));
        assert_eq!(metrics.http_series()[0].count, 1);
        assert_eq!(metrics.mcp_series()[0].tool, "list_nodes");
        assert_eq!(metrics.converge_series()[0].status, "success");
        assert_eq!(metrics.store_series()[0].operation, "find_node");
        assert_eq!(metrics.search_series()[0].backend, "embedded");
        assert_eq!(metrics.object_store_series()[0].outcome, "error");
    }

    #[test]
    fn collapses_unknown_keys_at_the_cap() {
        let metrics = Metrics::new();
        for index in 0..=REQUEST_SERIES_LIMIT {
            metrics.record_http(&format!("/r{index}"), "GET", "_default", 200, Duration::from_millis(1));
        }
        let series = metrics.http_series();
        assert!(series.len() <= REQUEST_SERIES_LIMIT + 1);
        assert!(series.iter().any(|entry| entry.route == OVERFLOW_LABEL));
    }

    #[test]
    fn buckets_are_strictly_increasing() {
        assert!(LATENCY_BUCKETS_SECONDS.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(*LATENCY_BUCKETS_SECONDS.last().unwrap() >= 10.0);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-metrics`
Expected: FAIL to compile — `microwave-metrics` is not a workspace member yet.

- [ ] **Step 3: Create the crate and move the registry**

`crates/microwave-metrics/Cargo.toml`: `name = "microwave-metrics"`, workspace edition/license/rust-version, `[dependencies] http.workspace = true`, `[dev-dependencies] tokio.workspace = true`.

Move `crates/microwave-api/src/metrics.rs` into `registry.rs` unchanged, then extend it: rename the module docs to the new crate, keep `started`, `total_requests`, `responses_*`, `authentication_failures`, and add `recorder: OnceLock<Arc<dyn Recorder>>`, `export_error: Mutex<Option<String>>`, `nodes: AtomicU64`, `maintenance: AtomicBool`, `pool: Mutex<Option<PoolGauges>>`, `keygen: Mutex<KeygenGauges>`, `dependencies: Mutex<Vec<DependencyState>>`, and one `Histograms<K>` per instrument family from `histogram.rs`. `record_request` and `record_response` keep updating only the legacy atomics; `record_http` does the fan-out for the histogram.

`histogram.rs` decides bucket index by linear scan, stores `sum: f64`, `count: u64`, and `Box<[u64]>` of `LATENCY_BUCKETS_SECONDS.len() + 1`, and applies the limit in `record`. `route.rs` splits on `/`, keeps literal segments that are in the known-segment table (`version`, `_status`, `_stats`, `server_api_version`, `license`, `organizations`, `nodes`, `cookbooks`, `data`, `roles`, `environments`, `clients`, `sandboxes`, `users`, `groups`, `containers`, `keys`, `policies`, `search`, `file_store`, `bookshelf`, `id`, `mcp`, `validate`, `required_recipe`, `data-collector`, `compliance`, `css`, `fonts`, `favicon.svg`, `index.html`), maps `organizations/<next>` to `:org`, maps a UUID to `:uuid`, maps anything else to `:id`, and returns `"/"` for the root and `"unknown"` for a path whose first segment is not in the table. The fallback spelling (`:org`, `:id`) intentionally differs from the axum templates (`{organization}`); it only applies when no template exists, so the two never collide.

Delete `crates/microwave-api/src/metrics.rs`, drop `pub mod metrics;` from `lib.rs:25`, turn `lib.rs:50` into `pub use microwave_metrics::{Metrics, MetricsSnapshot};`, switch `state.rs` to `microwave_metrics::Metrics`, and add `microwave-metrics = { path = "../microwave-metrics" }` to `crates/microwave-api/Cargo.toml`.

- [ ] **Step 4: Run the tests and the existing suite**

Run: `cargo test -p microwave-metrics && cargo test -p microwave-api --test ops_test`
Expected: PASS for both; `/_stats` output unchanged.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/microwave-metrics crates/microwave-api/Cargo.toml crates/microwave-api/src/lib.rs crates/microwave-api/src/state.rs
git rm crates/microwave-api/src/metrics.rs
git commit -m "refactor(metrics): move the registry into microwave-metrics"
```

---

### Task 2: `[otel]` configuration

**Files:**
- Modify: `crates/microwave-config/src/config.rs` (struct field, new `OtelConfig`, env, validate, docs list, tests)
- Modify: `packaging/microwave.toml.example`
- Modify: `bin/microwave-server/src/main.rs:249-273` (`redact_config_for_output`)
- Modify: `crates/microwave-ctl/src/secrets.rs` (`KNOWN_CREDENTIALS`, plus a test)

**Interfaces:**
- Consumes: nothing.
- Produces: `microwave_config::OtelConfig` with `enabled: bool`, `endpoint: Option<String>`, `protocol: String`, `interval_seconds: u64`, `timeout_ms: u64`, `gauge_refresh_seconds: u64`, `shutdown_timeout_ms: u64`, `service_name: String`, `environment: Option<String>`, `headers: Vec<String>`, `tls_ca_path: Option<PathBuf>`, `insecure_skip_tls_verify: bool`; `OtelConfig::headers_parsed() -> Result<Vec<(String, String)>, String>`; `Config.otel`.

- [ ] **Step 1: Write the failing tests**

In `crates/microwave-config/src/config.rs` tests:

```rust
#[test]
fn otel_is_disabled_by_default() {
    let config = Config::default();
    assert!(!config.otel.enabled);
    assert_eq!(config.otel.protocol, "http/protobuf");
    assert_eq!(config.otel.interval_seconds, 60);
    assert_eq!(config.otel.shutdown_timeout_ms, 5_000);
    assert!(config.otel.headers.is_empty());
}

#[test]
fn otel_rejects_bad_values() {
    for (toml, key) in [
        ("[otel]\nenabled = true\n", "otel.endpoint"),
        ("[otel]\nenabled = true\nendpoint = \"ftp://x\"\n", "otel.endpoint"),
        ("[otel]\nendpoint = \"http://x\"\nprotocol = \"grpc\"\n", "otel.protocol"),
        ("[otel]\nenabled = true\nendpoint = \"http://x\"\ninterval_seconds = 0\n", "otel.interval_seconds"),
        ("[otel]\nenabled = true\nendpoint = \"http://x\"\ngauge_refresh_seconds = 0\n", "otel.gauge_refresh_seconds"),
        ("[otel]\nenabled = true\nendpoint = \"http://x\"\ntls_ca_path = \"/nope.pem\"\n", "otel.tls_ca_path"),
        ("[otel]\nenabled = true\nendpoint = \"http://x\"\nheaders = [\"NoColon\"]\n", "otel.headers"),
    ] {
        let error = Config::from_toml_str(toml).expect_err(toml);
        assert!(format!("{error}").contains(key), "{error}");
    }
}

#[test]
fn otel_accepts_a_valid_block() {
    let config = Config::from_toml_str(
        "[otel]\nenabled = true\nendpoint = \"https://collector:4318\"\nheaders = [\"Authorization: Bearer t\"]\n",
    )
    .unwrap();
    assert_eq!(
        config.otel.headers_parsed().unwrap(),
        vec![("Authorization".to_string(), "Bearer t".to_string())]
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-config otel_`
Expected: FAIL — `no field otel`.

- [ ] **Step 3: Implement the config**

Add `pub otel: OtelConfig` to `Config` (after `stats`), the `OtelConfig` struct with `#[serde(default)]` and a `Default` impl matching the values above, `otel: OtelConfig::default()` where `Config` is built, `MW_OTEL__*` parsing next to the `MW_STATS__*` block (comma-split `MW_OTEL__HEADERS`), and an `OtelConfig::validate` call in `Config::validate` that maps every failure to `ConfigError::InvalidValue` with the failing key. Because one block can fail on several keys, `OtelConfig::validate(&self) -> Result<(), (&'static str, String)>` returns the key as its first element rather than one fixed string. Validation, in this order: unsupported scheme or missing endpoint when enabled, non-zero timers, `protocol == "http/protobuf"`, headers contain `:` with a non-empty name, TLS keys require an `https` endpoint, CA file must exist. `headers_parsed` splits once on `:` and trims the name only; an empty value stays legal.

`apply_otel_secrets` (Task 4) reuses `validate` and reports `format!("{key}: {message}")` (the key already carries the `otel.` prefix), which keeps the fail-closed behaviour of `serve` on a bad stored header.

Document the keys in the `Config::load` doc list at the end of the `MW_*` block. Add a redaction block in `redact_config_for_output`:

```rust
config.otel.headers = config
    .otel
    .headers
    .iter()
    .map(|header| match header.split_once(':') {
        Some((name, _)) => format!("{name}: ***"),
        None => header.clone(),
    })
    .collect();
```

Add the commented, disabled `[otel]` block to `packaging/microwave.toml.example` exactly as written in the spec's Configuration section (including the egress sentence).

The secret store must accept the group: add `("otel", &["headers"])` to `KNOWN_CREDENTIALS` in `crates/microwave-ctl/src/secrets.rs` and a test next to the existing ones that `set_secret` on a fresh store accepts `otel` / `headers` and that `show_secret` returns the stored line. Without that entry, `microwave-ctl set-secret otel headers` fails on an unknown group.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-config && cargo test -p microwave-server && cargo test -p microwave-ctl`
Expected: PASS, including the redaction test added to `bin/microwave-server/src/main.rs` asserting `Authorization: ***` and the new `otel` credential-group test.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-config/src/config.rs packaging/microwave.toml.example bin/microwave-server/src/main.rs crates/microwave-ctl/src/secrets.rs
git commit -m "feat(otel): add the off-by-default otel config block"
```

---

### Task 3: `microwave-otel` provider and recorder

**Files:**
- Create: `crates/microwave-otel/Cargo.toml`
- Create: `crates/microwave-otel/src/lib.rs`
- Create: `crates/microwave-otel/src/recorder.rs`
- Create: `crates/microwave-otel/src/gauges.rs`
- Modify: `Cargo.toml` (member plus pinned workspace deps)
- Test: `crates/microwave-otel/src/recorder.rs` (module tests)

**Interfaces:**
- Consumes: `microwave_metrics::{Metrics, Recorder, LATENCY_BUCKETS_SECONDS, KeygenGauges, PoolGauges}`, `microwave_config::OtelConfig`.
- Produces: `microwave_otel::{init, OtelGuard, OtelError}`; `init(&OtelConfig, Arc<Metrics>, scheme: &'static str, indexing_mode: &'static str) -> Result<OtelGuard, OtelError>`, which attaches its recorder to the metrics it is given (`scheme` becomes the `url.scheme` attribute; the binary decides it, the recorder cannot); `OtelGuard::{shutdown(self).await, force_flush(&self).await}`; `microwave_otel::testing::init_with_exporter(Arc<Metrics>, InMemoryMetricExporter) -> Result<OtelGuard, OtelError>` (`http`, `embedded`); `microwave_otel::CATALOGUE: &[(&str, &str)]` mapping OTLP name → Prometheus name.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn records_into_an_in_memory_provider() {
    let metrics = Arc::new(Metrics::new());
    let exporter = opentelemetry_sdk::metrics::InMemoryMetricExporter::default();
    let guard = super::testing::init_with_exporter(metrics.clone(), exporter.clone()).unwrap();
    metrics.record_http("/_status", "GET", "acme", 200, std::time::Duration::from_millis(4));
    guard.force_flush().await;
    let names: Vec<String> = exporter
        .get_finished_metrics()
        .unwrap()
        .iter()
        .flat_map(|resource| resource.scope_metrics().flat_map(|scope| scope.metrics().map(|metric| metric.name().to_string())).collect::<Vec<_>>())
        .collect();
    assert!(names.iter().any(|name| name == "http.server.request.duration"));
    assert!(names.iter().any(|name| name == "microwave.server.requests"));
}

#[test]
fn catalogue_pins_the_names_and_stays_unique() {
    assert!(super::CATALOGUE.contains(&("http.server.request.duration", "microwave_server_request_duration_seconds")));
    assert!(super::CATALOGUE.contains(&("microwave.mcp.tool.calls", "microwave_server_mcp_tool_calls_total")));
    assert!(super::CATALOGUE.iter().all(|(otlp, prom)| otlp.contains('.') && prom.starts_with("microwave_server_")));
    let mut prom: Vec<&str> = super::CATALOGUE.iter().map(|(_, prom)| *prom).collect();
    prom.sort_unstable();
    prom.dedup();
    assert_eq!(prom.len(), super::CATALOGUE.len());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p microwave-otel`
Expected: FAIL — crate does not exist.

- [ ] **Step 3: Build the provider, instruments, and gauges**

`Cargo.toml` deps: `opentelemetry.workspace`, `opentelemetry_sdk = { workspace = true, features = ["metrics", "testing", "experimental_metrics_periodicreader_with_async_runtime"] }`, `opentelemetry_otlp = { workspace = true, features = ["http-proto", "reqwest-client", "reqwest-rustls", "tls-webpki-roots"] }`, `microwave-config`, `microwave-metrics`, `tokio`, `thiserror`, `tracing`.

`lib.rs` also holds `struct TrackingExporter<E> { inner: E, metrics: Arc<Metrics>, endpoint: String }` implementing `PushMetricExporter` by delegating every method and, in `export`, turning an `Err` into `metrics.set_export_error(...)` plus one `tracing::warn!` with endpoint and error class, and a success into `metrics.clear_export_error()`. That keeps the API crate free of OTel types and is what feeds `/_stats`. The timeout/`force_flush`/`shutdown` delegation must pass straight through.

`init` builds the `Resource` (`service.name` from config, `service.version` from `CARGO_PKG_VERSION`, `service.instance.id` a fresh `Uuid`, `host.name`, `deployment.environment` when set), the OTLP exporter (`with_endpoint(config.endpoint.as_deref().map(str::trim).unwrap_or_default()`, because validation trims only for its own check), `with_timeout`, headers from `headers_parsed`, CA file and `danger_accept_invalid_certs` when set), a `PeriodicReader` with `interval_seconds`, `SdkMeterProvider::builder().with_resource(...).with_reader(...)` plus one `View` per instrument carrying the cardinality limit, and installs it globally. `init_with_exporter` is the same with a supplied `MetricReader`.

`recorder.rs` registers one instrument per catalogue row on meter `microwave-server`, stores them in `OtelRecorder` together with the `scheme`, and implements `Recorder` by mapping arguments onto attributes exactly as the catalogue specifies — `http.route`, `http.request.method`, `http.response.status_code`, `url.scheme`, `microwave.organization` on the request histogram, `microwave.organization` on the MCP and converge instruments, `db.system`/`db.operation` on store timing, and `backend`/`operation` on search and object store. `testing.rs` is a plain `pub mod` (not `#[cfg(test)]`, integration tests use it too) exposing `init_with_exporter`, which takes the supplied `MetricReader` instead of building the OTLP exporter; keep the SDK `testing` feature enabled. The exporter is wrapped in `TrackingExporter` before it reaches the `PeriodicReader`.

`gauges.rs` registers the observable callbacks (`uptime`, `maintenance`, `nodes`, `pool`, `keygen`, `dependencies`, `build_info`) reading `Metrics` accessors, and `OtelGuard::shutdown` wraps `provider.shutdown()` in the configured timeout.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p microwave-otel`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/microwave-otel
git commit -m "feat(otel): add the OTLP recorder and provider lifecycle"
```

---

### Task 4: Server wiring and shutdown flush

**Files:**
- Modify: `bin/microwave-server/src/main.rs`
- Modify: `bin/microwave-server/src/bootstrap.rs` (`apply_otel_secrets`)
- Modify: `crates/microwave-api/src/state.rs` (`with_metrics`, `dependency_states`)
- Modify: `bin/microwave-server/Cargo.toml`
- Test: `bin/microwave-server/tests/cli_test.rs`

**Interfaces:**
- Consumes: `microwave_otel::init`, `Metrics::attach`, `Config.otel`.
- Produces: `AppState::with_metrics(store, config, object_store, metrics) -> Result<Self, StateError>`; `AppState::dependency_states(&self) -> Vec<(&'static str, bool)>` as an `async fn` (it awaits `health.snapshot`).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn serve_check_reports_the_otel_state_without_dialing() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        format!(
            "paths = {{ data_dir = \"{}\" }}\n[otel]\nenabled = true\nendpoint = \"http://127.0.0.1:1\"\n",
            dir.path().join("data").display()
        ),
    )
    .unwrap();
    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("otel: enabled"), "{stderr}");
}

#[test]
fn serve_check_reports_otel_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        format!("paths = {{ data_dir = \"{}\" }}\n", dir.path().join("data").display()),
    )
    .unwrap();
    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("otel: disabled"));
}
```

`--check` uses port 1 as the endpoint, so a passing test also proves no dial happens.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-server serve_check_reports`
Expected: FAIL — `--check` never mentions otel.

- [ ] **Step 3: Wire it**

In `main.rs`, before the store is built: `let metrics = Arc::new(Metrics::new());`, then after `build_object_store` and before `AppState`: when `config.otel.enabled`, `let otel = Some(microwave_otel::init(&config.otel, metrics.clone(), scheme, indexing_mode)?);` where `scheme` is `if config.server.tls.is_enabled() { "https" } else { "http" }` and `indexing_mode` comes from `config.search.backend` (`"embedded"` / `"opensearch"`), matching the `build_info` attribute, and build the state with `AppState::with_metrics(..., metrics.clone())` — `init` already attached its recorder to `metrics`. The `check` branch prints `otel: enabled` or `otel: disabled` on stderr next to the existing configuration-ok line, and returns before `init` runs. Spawn the gauge refresh task with `tokio::time::interval(config.otel.gauge_refresh_seconds)` at its first tick immediately (`interval.tick()` fires at once; it skips a missed tick with `MissedTickBehavior::Delay`): each tick awaits `state.dependency_states()`, then reads `state.store.pool_stats()`, `state.store.maintenance_enabled()`, `state.store.count_nodes()` and `state.keygen.status()` into the metrics setters, log-and-continue on backend errors. Keep the `JoinHandle` and `abort()` it after `serve(...)` returns, before `otel.shutdown().await` (the guard applies the timeout itself); log a shutdown failure at `warn`.

The header secret is read next to the other bootstrap helpers, before `Arc::new(config)` and only when `config.otel.headers` is empty: add `bootstrap::apply_otel_secrets(&mut config, store.as_ref()).await?`, which loads the `otel`/`headers` secret and splits it on newlines into `config.otel.headers`, then re-runs `config.otel.validate()` so a malformed stored value fails closed at startup rather than at the first export. Config and environment win over the secret; an absent secret is not an error.

`AppState::new` and `from_sqlite` delegate to `with_metrics(..., Arc::new(Metrics::new()))`. `dependency_states` awaits `self.health.snapshot(&self.store, &self.search, &self.object_store)` and maps each probe to `(name, status == "pong")`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-server && cargo test -p microwave-api`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bin/microwave-server crates/microwave-api/src/state.rs
git commit -m "feat(otel): wire the provider, gauge refresh and shutdown flush"
```

### Task 5: Request route labels and durations

**Files:**
- Modify: `crates/microwave-api/src/middleware.rs:47-88` (`observe_request`)
- Modify: `crates/microwave-api/src/lib.rs:100-135` (inner router layer)
- Test: `crates/microwave-api/tests/otel_metrics_test.rs`

**Interfaces:**
- Consumes: `Metrics::record_http`, `metrics_route`, `organization_label`.
- Produces: `middleware::RouteTemplate(pub String)` extension; `middleware::record_route` layer.

- [ ] **Step 1: Write the failing tests**

`crates/microwave-api/tests/otel_metrics_test.rs` starts with the usual preamble and helpers:

```rust
mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

async fn get(app: &axum::Router, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// A Chef client marker, so the landing page does not answer instead of the API.
async fn get_as_client(app: &axum::Router, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("x-chef-version", "15.0.0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}
```

then the tests:

```rust
#[tokio::test]
async fn records_a_route_template_and_the_org() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    let response = get_as_client(&app, "/organizations/acme/nodes").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let series = state.metrics.http_series();
    let entry = series.first().expect("a recorded series");
    assert_eq!(entry.route, "/organizations/{organization}/nodes");
    assert_eq!(entry.org, "acme");
    assert_eq!(entry.status, 401);
}

#[tokio::test]
async fn records_unmatched_paths_with_the_normalizer() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    get(&app, "/version").await;
    let series = state.metrics.http_series();
    assert_eq!(series[0].route, "/version");
    assert_eq!(series[0].org, "_default");
}

#[tokio::test]
async fn records_the_landing_page_as_its_own_route() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    let response = get(&app, "/").await;
    assert_eq!(response.status(), StatusCode::OK);
    let series = state.metrics.http_series();
    assert_eq!(series[0].route, "/");
    assert_eq!(series[0].org, "_default");
}

#[tokio::test]
async fn unsigned_client_requests_stay_a_401() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    let response = get_as_client(&app, "/organizations/acme/nodes").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let series = state.metrics.http_series();
    assert_eq!(series[0].status, 401);
    assert_eq!(series[0].route, "/organizations/{organization}/nodes");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test otel_metrics_test`
Expected: FAIL — no `http_series` entries beyond the local histogram, and no `RouteTemplate` layer.

- [ ] **Step 3: Implement the two layers**

Add `record_route` as a `.layer(axum::middleware::from_fn(middleware::record_route))` on the `inner` router, inserted directly *below* the existing `.layer(... authenticate)` call in `lib.rs` (layers added later wrap earlier ones, so this runs after matching but outside auth). `Router::layer` only runs for matched routes, so the fallback and `/version` legitimately have no template.

`record_route` extracts `Option<MatchedPath>` from the request extensions *first*, then `next.run(request)`, then inserts `RouteTemplate(template)` into `response.extensions_mut()` when a template exists — including on the `401` that `authenticate` returns early, which is exactly why the template has to be captured before the call rather than read from a surviving request.

In `observe_request`, right after `next.run(request)` (before the `tracing::info!`), take the template from `response.extensions()`, fall back to `metrics_route(&path)` when absent, take the org from `organization_label(&path)`, and call `state.metrics.record_http(&route, method.as_str(), &org, response.status().as_u16(), started.elapsed())`. Keep the existing log line and fields unchanged.

The `RequestContextApplied` early return stays unmeasured: the layer sits only on the outer router and runs once per request, so the guard never fires for a request that has not already been logged and measured.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --test otel_metrics_test && cargo test -p microwave-api --test ops_test`
Expected: PASS; the `/_stats` totals still count every request exactly once.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/middleware.rs crates/microwave-api/src/lib.rs crates/microwave-api/tests/otel_metrics_test.rs
git commit -m "feat(otel): label requests with route templates and orgs"
```

---

### Task 6: `/_stats` histograms, gauges, and the `otel` object

**Files:**
- Modify: `crates/microwave-api/src/lib.rs:203-360` (`stats`, `push_pool_gauges`, `stats_json`)
- Test: `crates/microwave-api/tests/ops_test.rs`

**Interfaces:**
- Consumes: `Metrics::{http_series, keygen, dependencies, export_error}`, `microwave_metrics::{HistogramSeries, …}`.
- Produces: text histogram blocks and the JSON `otel` object.

- [ ] **Step 1: Write the failing tests**

Add a body helper next to `get`, plus the same `get_as_client` helper Task 5 introduced:

```rust
async fn body_text(response: axum::response::Response) -> String {
    String::from_utf8(axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
}
```

then the tests:

```rust
#[tokio::test]
async fn stats_renders_request_histograms() {
    let app = support::app(support::state_with_pivotal());
    get_as_client(&app, "/organizations/acme/nodes").await;
    let response = get(&app, "/_stats?format=prometheus").await;
    let body = body_text(response).await;
    assert!(body.contains("# TYPE microwave_server_request_duration_seconds histogram"));
    assert!(body.contains("microwave_server_request_duration_seconds_count{route=\"/organizations/{organization}/nodes\",method=\"GET\",org=\"acme\",status=\"401\"} 1"));
    assert!(body.contains("le=\"+Inf\""));
}

#[tokio::test]
async fn collapsed_series_render_one_label_set_each() {
    let state = support::state_with_pivotal();
    for index in 0..=microwave_metrics::REQUEST_SERIES_LIMIT {
        state
            .metrics
            .record_http(&format!("/r{index}"), "GET", "_default", 200, std::time::Duration::from_millis(1));
    }
    let app = support::app(state);
    let body = body_text(get(&app, "/_stats?format=prometheus").await).await;
    let mut seen = std::collections::HashSet::new();
    for line in body.lines().filter(|line| line.starts_with("microwave_server_request_duration_seconds_count{")) {
        let labels = line.split_once('{').unwrap().1.split_once("} ").unwrap().0;
        assert!(seen.insert(labels.to_string()), "duplicate label set: {labels}");
    }
    assert!(body.contains("route=\"_other\""));
}

#[tokio::test]
async fn stats_json_reports_the_otel_state() {
    let app = support::app(support::state_with_pivotal());
    let body = support::response_json(get(&app, "/_stats?format=json").await).await;
    assert_eq!(body["otel"]["enabled"], false);
    assert!(body["otel"]["endpoint"].is_null());
    assert!(body["otel"]["last_export_error"].is_null());
    assert!(body["keygen"]["capacity"].is_number());
    assert!(body["upstreams"]["database"].is_string());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test ops_test stats_`
Expected: FAIL — no histogram lines, no `otel` object.

- [ ] **Step 3: Render them**

Render every series as `_bucket`/`_sum`/`_count` with label order route, method, org, status, and `le` last, help/type lines first, placed before the pool block. Add `otel`, `keygen`, `upstreams` to the JSON document, leaving the existing keys untouched. `upstreams` uses the stable keys `database`, `search`, `bookshelf` (mapped from the `chef_sql` / `chef_index` / `chef_opensearch` / `chef_elasticsearch` / `bookshelf` probes) and the probe's `status` string as value, `"unknown"` for a missing probe; the text gauge keeps the raw probe name as `dependency`. `/_stats` still writes the node/maintenance/pool atomics before reading them.

`stats_json` currently takes the snapshot only; it needs the state (or the metrics handle plus the health snapshot) for `otel`, `keygen` and `upstreams`, so its existing unit-test call sites change with it. `otel.endpoint` comes from `state.config.otel.endpoint`, `null` when disabled, and is printed without credentials: add `fn endpoint_without_credentials(endpoint: &str) -> String` that drops the `user:pass@` part of the authority, and a unit test with `https://user:pass@collector:4318` plus one with plain `http://collector:4318` (unchanged). A userinfo-bearing endpoint is legal because `Config` only checks the scheme.

The unlabelled counters already exist on this surface — `microwave_server_requests_total`, `microwave_server_responses_total{class}`, `microwave_server_authentication_failures_total` — and stay byte-identical. Everything new is appended after them: the histograms, then the gauges (`uptime_seconds`, `maintenance`, `nodes`, `db_pool_connections{state}`, `db_pool_connections_max`, `build_info`, `dependency_up{dependency}`, `keygen_*`), rendered from the same atomics the JSON reads. The existing JSON keys `requests_total` and `responses_total` keep their spelling.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --test ops_test`
Expected: PASS, including the three pre-existing assertions on the unlabelled counters.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/lib.rs crates/microwave-api/tests/ops_test.rs
git commit -m "feat(otel): expose histograms and exporter state on /_stats"
```

---

### Task 7: MCP and converge metrics

**Files:**
- Modify: `crates/microwave-api/src/mcp/tools.rs:1167-1224` (`logged_tool`)
- Modify: `crates/microwave-api/src/data_collector.rs:90-112`
- Test: `crates/microwave-api/tests/mcp_test.rs`, `crates/microwave-api/tests/otel_metrics_test.rs`

**Interfaces:**
- Consumes: `Metrics::{record_mcp_tool, record_converge}`.
- Produces: nothing new.

- [ ] **Step 1: Write the failing tests**

```rust
// In `mcp_test.rs`, built on the file's existing `fixture` + `call_tool`
// helpers (which return the state alongside nothing else) — `fixture` returns
// `(state, org, app)`, so bind the state:
#[tokio::test]
async fn mcp_tool_calls_record_outcome_and_duration() {
    let (state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    call_tool(&app, "acme", "a", 1, "list_nodes", json!({})).await;
    let series = state.metrics.mcp_series();
    let entry = series.iter().find(|entry| entry.tool == "list_nodes").unwrap();
    assert_eq!(entry.outcome, "ok");
    assert_eq!(entry.org, "acme");
}

#[tokio::test]
async fn a_failing_mcp_tool_records_an_error_outcome() {
    let (state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    call_tool(&app, "acme", "a", 1, "get_node", json!({ "name": "missing" })).await;
    assert!(state
        .metrics
        .mcp_series()
        .iter()
        .any(|entry| entry.tool == "get_node" && entry.outcome != "ok"));
}

// In `otel_metrics_test.rs`, reusing the existing data-collector request shape:
#[tokio::test]
async fn converge_runs_record_their_status() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    post_run_converge(&app, "acme", "success").await;
    let series = state.metrics.converge_series();
    assert!(series.iter().any(|entry| entry.status == "success" && entry.org == "acme"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test mcp_test mcp_tool_calls_record && cargo test -p microwave-api --test otel_metrics_test converge_`
Expected: FAIL — no `mcp_series` and no `converge_series`.

- [ ] **Step 3: Record at both sites**

In `logged_tool`, after `outcome` is computed, `ctx.state.metrics.record_mcp_tool(tool, &ctx.org.name, outcome, elapsed)` before the log line, so both share `elapsed` and `outcome`. In `data_collector`, call `prepare_node_run(&body)` once *outside* the `storing` branch (the forward path never enters it), record `state.metrics.record_converge(&organization, &run.status)` for an `Ok(Some(run))`, and let the `storing` branch reuse the same parsed value, keeping its error arm. The path org is the label because the forward path resolves no org.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --test otel_metrics_test && cargo test -p microwave-api --test mcp_test`
Expected: PASS; the existing log-line assertions still hold.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/mcp/tools.rs crates/microwave-api/src/data_collector.rs crates/microwave-api/tests/mcp_test.rs crates/microwave-api/tests/otel_metrics_test.rs
git commit -m "feat(otel): measure mcp tool calls and converge outcomes"
```

---

### Task 8: Store operation timing

**Files:**
- Create: `crates/microwave-store/src/timing.rs`
- Modify: `crates/microwave-store/src/lib.rs`
- Modify: `crates/microwave-store/src/sqlite.rs:29-33` (store struct)
- Modify: `crates/microwave-store/src/objects.rs` (`create_node` 53, `find_node` 101, `list_nodes` 115, `count_nodes` 173, `delete_node` 234)
- Modify: `crates/microwave-store/src/reports.rs` (`store_node_run`)
- Modify: `crates/microwave-store-postgres/src/store.rs:20-25` (store struct)
- Modify: `crates/microwave-store-postgres/src/objects.rs` (the same five methods)
- Modify: `crates/microwave-store-postgres/src/reports.rs` (`store_node_run`)
- Modify: both store `Cargo.toml`s (`microwave-metrics` dependency)
- Modify: `bin/microwave-server/src/main.rs` (both arms of the store `match`)
- Test: `crates/microwave-store/src/timing.rs`

**Interfaces:**
- Consumes: `Metrics::record_store_operation`.
- Produces: `microwave_store::timing::{StoreTimer, DbSystem}`; `StoreTimer` records on drop; `SqliteStore::set_metrics`, `PostgresStore::set_metrics`.

The metrics handle arrives through `set_metrics` rather than a constructor, because the store is built (and bootstrapped) before the registry exists and because the binary only ever holds an `Arc<dyn MetadataStore>`. Calling `set_metrics` on the concrete type inside each `match` arm in `main.rs` is therefore the only reachable place.

- [ ] **Step 1: Write the failing test**

Module head, so the test has its imports:

```rust
use microwave_metrics::Metrics;
use std::sync::Arc;
use std::time::Instant;
```

```rust
#[test]
fn a_finished_timer_records_the_operation() {
    let metrics = Arc::new(Metrics::new());
    {
        let _timer = StoreTimer::start(Some(metrics.clone()), DbSystem::Sqlite, "create_node");
    }
    let entry = &metrics.store_series()[0];
    assert_eq!(entry.operation, "create_node");
    assert_eq!(entry.outcome, "ok");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p microwave-store timing`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement the guard and instrument the hot methods**

`enum DbSystem { Sqlite, Postgres }` with `fn as_str(self) -> &'static str` (`"sqlite"` / `"postgres"`). `StoreTimer::start(metrics: Option<Arc<Metrics>>, system: DbSystem, operation: &'static str) -> Self` stores the start; `Drop` records with `outcome = "ok"`; add `fn failed(&mut self)` that flips the recorded outcome to `"error"`. Both stores gain `metrics: Arc<RwLock<Option<Arc<Metrics>>>>` with `set_metrics` and `fn timer(&self, operation: &'static str) -> StoreTimer`. In `main.rs`, call `store.set_metrics(metrics.clone())` right after each `Arc::new(SqliteStore::open_with_compression(…)?)` and each `PostgresStore::connect_with_compression_and_pool(…)`, before `bootstrap::ensure_superuser`. Wrap `create_node`, `find_node`, `list_nodes`, `count_nodes`, `delete_node`, and `store_node_run` by holding the guard for the body and calling `failed()` in the error arm.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-store timing && cargo test -p microwave-store && cargo test -p microwave-store-postgres`
Expected: PASS, with Postgres tests skipping when no database is configured.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-store crates/microwave-store-postgres bin/microwave-server/src/main.rs
git commit -m "feat(otel): time hot store operations"
```

---

### Task 9: Search and object-store decorators

**Files:**
- Create: `crates/microwave-api/src/search/timed.rs`
- Modify: `crates/microwave-api/src/search/mod.rs`
- Create: `crates/microwave-objectstore/src/timed.rs`
- Modify: `crates/microwave-objectstore/src/lib.rs`
- Modify: `crates/microwave-objectstore/Cargo.toml`
- Modify: `bin/microwave-server/src/main.rs` (`build_object_store` call site)
- Modify: `crates/microwave-api/src/state.rs` (wrap the search backend in `with_metrics`)
- Test: `crates/microwave-api/src/search/timed.rs`, `crates/microwave-objectstore/src/timed.rs`

**Interfaces:**
- Consumes: `Metrics::{record_search_request, record_object_store_request}`.
- Produces: `search::TimedSearch::new(Arc<dyn SearchBackend>, Arc<Metrics>)`; `microwave_objectstore::TimedObjectStore::new(Arc<dyn ObjectStore + Send + Sync>, Arc<Metrics>, &'static str)`.

- [ ] **Step 1: Write the failing tests**

Module head for `timed.rs` (both files use `super::*` plus these):

```rust
use microwave_metrics::Metrics;
use std::sync::Arc;

/// An empty store is enough: the embedded backend answers from the store.
fn store() -> Arc<dyn microwave_store::MetadataStore> {
    let dir = tempfile::tempdir().unwrap();
    Arc::new(microwave_store::SqliteStore::open(&dir.path().join("metrics.db")).unwrap())
}

fn request() -> super::SearchRequest {
    super::SearchRequest {
        org_id: "acme".into(),
        kind: super::IndexKind::Node,
        query: super::SearchQuery::MatchAll,
        start: 0,
        rows: 10,
        actor_authz_id: None,
    }
}
```

`crates/microwave-objectstore/src/timed.rs` additionally defines `struct FailingStore;` implementing `ObjectStore` with `get` returning a real `ObjectStoreError` variant and every other method a trivial value. That file needs no `tempfile`; the search test head does (already a dev-dependency of `microwave-api`).

```rust
#[tokio::test]
async fn searches_record_backend_and_outcome() {
    let metrics = Arc::new(Metrics::new());
    let backend = TimedSearch::new(Arc::new(EmbeddedSearch::new(store())), metrics.clone());
    let _ = backend.search(&request()).await;
    let entry = &metrics.search_series()[0];
    assert_eq!(entry.backend, "embedded");
    assert_eq!(entry.outcome, "ok");
}

#[test]
fn object_store_calls_record_operation_and_outcome() {
    let metrics = Arc::new(Metrics::new());
    let store = TimedObjectStore::new(Arc::new(FailingStore), metrics.clone(), "local");
    let _ = store.get("org", "checksum");
    assert_eq!(metrics.object_store_series()[0].outcome, "error");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api timed && cargo test -p microwave-objectstore timed`
Expected: FAIL — modules missing.

- [ ] **Step 3: Implement both decorators**

Each implements the full trait by delegating and timing every method, recording `backend`, the method name as operation, and `ok`/`error`. Cheap non-future methods (`name`, `supports_purge`, `exists` on the object store) delegate without a timer; `search`/`index`/`index_many`/`refresh`/`delete`/`delete_index`/`health_check` and `put`/`get`/`delete`/`metadata` are timed. `TimedSearch::name` must return the inner name so `/_status` keeps reporting the real backend. Attach `TimedSearch` in `AppState::with_metrics` (`self.search = Arc::new(TimedSearch::new(search, metrics.clone()))`) and wrap the object store in `main` right after `build_object_store`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api timed && cargo test -p microwave-objectstore && cargo test -p microwave-server`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/search crates/microwave-objectstore bin/microwave-server/src/main.rs crates/microwave-api/src/state.rs
git commit -m "feat(otel): time search and object store calls"
```

---

### Task 10: OTLP export integration test

**Files:**
- Create: `crates/microwave-otel/tests/export_test.rs`
- Modify: `crates/microwave-otel/Cargo.toml` (dev-deps `axum`, `opentelemetry-proto` with `gen-tonic-messages`, `prost`)
- Modify: `Cargo.toml` (workspace dev-dep `opentelemetry-proto = { version = "=0.33.1", default-features = false, features = ["gen-tonic-messages", "metrics"] }` and `prost = "0.14"`)

**Interfaces:**
- Consumes: `init`, `OtelGuard`.
- Produces: nothing.

- [ ] **Step 1: Write the failing test**

Test head:

```rust
use microwave_config::OtelConfig;
use microwave_metrics::Metrics;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use std::sync::Arc;
use std::time::Duration;

/// A config that points at `endpoint` and exports fast enough for a test.
fn config_for(endpoint: &str) -> OtelConfig {
    OtelConfig {
        enabled: true,
        endpoint: Some(endpoint.to_string()),
        interval_seconds: 1,
        ..Default::default()
    }
}
```

```rust
#[tokio::test]
async fn exports_a_request_histogram_to_a_stub_collector() {
    let (endpoint, mut received) = stub_collector(200).await;
    let metrics = Arc::new(Metrics::new());
    let guard = init(&config_for(&endpoint), metrics.clone(), "http").unwrap();
    metrics.record_http("/_status", "GET", "acme", 200, Duration::from_millis(3));
    guard.force_flush().await;

    let mut names: Vec<String> = Vec::new();
    while names.is_empty() {
        let body = received.recv().await.expect("an export");
        let Ok(request) = prost::Message::decode(&body[..]) else {
            continue;
        };
        names = request
            .resource_metrics
            .iter()
            .flat_map(|rm| rm.scope_metrics.iter())
            .flat_map(|sm| sm.metrics.iter())
            .map(|metric| metric.name.clone())
            .collect();
    }
    assert!(names.iter().any(|name| name == "http.server.request.duration"));
}

#[tokio::test]
async fn a_failing_collector_does_not_break_recording() {
    let (endpoint, mut received) = stub_collector(500).await;
    let metrics = Arc::new(Metrics::new());
    let guard = init(&config_for(&endpoint), metrics.clone(), "http").unwrap();
    metrics.record_http("/_status", "GET", "_default", 200, Duration::from_millis(1));
    guard.force_flush().await;
    assert!(received.try_recv().is_ok());
    assert_eq!(metrics.http_series()[0].count, 1);
    assert_eq!(metrics.snapshot().total_requests, 0);
    assert!(metrics.export_error().is_some(), "a failed export is recorded");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p microwave-otel --test export_test`
Expected: FAIL — no test file, no dev-deps.

- [ ] **Step 3: Implement the stub and the test helpers**

`stub_collector(status: u16) -> (String, mpsc::Receiver<Vec<u8>>)` binds `127.0.0.1:0`, serves `POST /v1/metrics` with axum (accepting any body, replying with `status`), pushes each raw body into the channel, and returns `http://<addr>` plus the receiver. Run it with `tokio::spawn` and leak the task for the test's lifetime.

The recorder must hold a `Meter` taken from the concrete `SdkMeterProvider` (`provider.meter("microwave-server")`), not `opentelemetry::global::meter`, so two tests that both install a global provider in the same test binary do not fight over one instrument set; still call `opentelemetry::global::set_meter_provider` so the global handle is real. `init_with_exporter` (the `testing` module from Task 3) shares the same construction path minus the OTLP exporter, so the export test and the in-memory test cannot drift.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p microwave-otel`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/microwave-otel
git commit -m "feat(otel): add the OTLP recorder and provider lifecycle"
```

---

### Task 11: Observability compose profile

**Files:**
- Create: `docker/otel/collector.yaml`
- Create: `docker/prometheus/prometheus.yml`
- Modify: `docker-compose.yml` (`otel-collector`, `prometheus` services; `MW_OTEL__*` on `microwave`)
- Modify: `AGENTS.md` (one line under the compose notes)

**Interfaces:**
- Consumes: the server's OTLP/HTTP endpoint `http://otel-collector:4318`.
- Produces: nothing.

- [ ] **Step 1: Write the failing check**

Run: `cd /Users/rmoriz/Sync/Projekte/microwave-server && docker compose --profile observability config --services`
Expected: only `microwave`.

- [ ] **Step 2: Add the services**

Both new services carry `profiles: [observability]`, so the default `up` is unchanged. They join the default network, so `http://otel-collector:4318` resolves from `microwave`.

`docker/otel/collector.yaml`: an OTLP receiver (HTTP on 4318, gRPC off), a `batch` processor, an exporter block with a `prometheus` exporter listening on `0.0.0.0:8889` (`send_timestamps: true`, `add_metric_suffixes: true`), and a one-pipeline `service` wiring that receiver through the batch to that exporter. `docker/prometheus/prometheus.yml`: global `scrape_interval: 5s` and one static target `otel-collector:8889`.

The `microwave` service gains two environment entries so the profile actually produces series:

```yaml
      MW_OTEL__ENABLED: "true"
      MW_OTEL__ENDPOINT: http://otel-collector:4318
```

They stay unconditional, so a plain `docker compose up` without the profile exports into nothing. That is the documented fail-open case: the server serves normally and `/_stats` reports the export error. Say so in the `AGENTS.md` line.

- [ ] **Step 3: Verify**

Run: `docker compose --profile observability config --services`
Expected: `microwave`, `otel-collector`, `prometheus`.
Run: `docker compose --profile observability up -d` (the image exists from the previous step), wait for `docker compose --profile observability ps` to show all three healthy, then `curl -s http://127.0.0.1:9090/api/v1/query --data-urlencode 'query=microwave_server_request_duration_seconds_count'`.
Expected: the scrape target is up and the query returns a series per route; then `docker compose --profile observability down -v --rmi local` and confirm `docker images` no longer lists `mw-*` or the collector/prometheus tags you pulled.

- [ ] **Step 4: Commit**

```bash
git add docker docker-compose.yml AGENTS.md
git commit -m "feat(otel): add an observability compose profile"
```

`docker/otel/collector.yaml` and `docker/prometheus/prometheus.yml` are new pinned-config surfaces; `check-claims.py` does not cover `docker/**`, but the guide in Task 12 links the profile and needs its port numbers to match this file.

---

### Task 12: Guide, deployment docs, website entry, claims

**Files:**
- Create: `docs/opentelemetry.md`
- Modify: `docs/index.md`
- Modify: `docs/deployment.md` (configuration subsection plus monitoring subsection)
- Modify: `README.md`
- Modify: `website/docs/docusaurus.config.ts` (`include`)
- Modify: `website/docs/sidebars.ts`
- Modify: `docs/claims.toml`

**Interfaces:**
- Consumes: the finished config keys, metric catalogue, and profile.
- Produces: nothing.

- [ ] **Step 1: Write the guide**

`docs/opentelemetry.md` covers: what the export is for; off by default and what enabling costs (the one outbound connection, `NetworkPolicy`/egress allowlist); enabling step by step with the collector snippet; the appended `/v1/metrics` path; header secrets via `microwave-ctl set-secret otel headers`; the TLS keys; the metric table with types and attributes; whole-server versus per-org queries; the cardinality rules and the `_other` symptom; watching it locally with the profile and the queries to start with; troubleshooting (`last_export_error`, silent collector, dropped series). Plain paragraph text and fenced blocks, no raw HTML anchors, internal links only in the `./guide.md` and `../crates/...` forms.

- [ ] **Step 2: Wire the links and nav**

`docs/index.md` gets an entry under Start here. `docs/deployment.md` gets `#### OpenTelemetry metrics` with the keys and validation errors and a Monitoring paragraph linking `./opentelemetry.md`. `README.md` gets one monitoring line. `docusaurus.config.ts` adds `'opentelemetry.md'` to `include`; `sidebars.ts` adds `{type: 'doc', id: 'opentelemetry', label: 'OpenTelemetry metrics'}` after `mcp`.

- [ ] **Step 3: Add claims**

`scripts/check-claims.py` fails on any unclaimed `docs/*.md`, so the guide needs an owning claim before the suite is green. Append blocks of this exact shape (`area` must be one of the inventory's areas; `docs` and `deployment` fit):

```toml
[[claim]]
id = "otel-off-by-default"
summary = "OpenTelemetry export is configured in `[otel]`, off by default, and fails open."
area = "deployment"
status = "verified"
surfaces = [ { path = "docs/opentelemetry.md", needle = "off by default" } ]
verify = { kind = "grep", path = "crates/microwave-config/src/config.rs", pattern = "pub struct OtelConfig" }

[[claim]]
id = "otel-guide-published"
summary = "The OpenTelemetry guide is part of the published docs site."
area = "docs"
status = "verified"
surfaces = [
  { path = "docs/opentelemetry.md", needle = "OpenTelemetry" },
  { path = "website/docs/sidebars.ts", needle = "opentelemetry" },
]
verify = { kind = "grep", path = "website/docs/docusaurus.config.ts", pattern = "opentelemetry.md" }

[[claim]]
id = "otel-request-histogram"
summary = "Requests are measured as a histogram labelled by route, method, org, and status."
area = "docs"
status = "verified"
surfaces = [ { path = "docs/opentelemetry.md", needle = "http.server.request.duration" } ]
verify = { kind = "grep", path = "crates/microwave-otel/src/recorder.rs", pattern = "http.server.request.duration" }

[[claim]]
id = "otel-cardinality-cap"
summary = "Per-instrument series caps collapse overflow into an `_other` series."
area = "docs"
status = "verified"
surfaces = [ { path = "docs/opentelemetry.md", needle = "_other" } ]
verify = { kind = "grep", path = "crates/microwave-metrics/src/registry.rs", pattern = "REQUEST_SERIES_LIMIT" }

[[claim]]
id = "otel-header-secret"
summary = "Collector headers are supplied through the secret store, not the config file."
area = "deployment"
status = "verified"
surfaces = [ { path = "docs/opentelemetry.md", needle = "set-secret otel headers" } ]
verify = { kind = "grep", path = "crates/microwave-ctl/src/secrets.rs", pattern = "\"otel\", &[\"headers\"]" }
```

Also extend `packaging-example-complete` with `{ path = "packaging/microwave.toml.example", needle = "[otel]" }` (the Task 2 block added the section; the claim promises every supported block is documented).

Then re-run the claims script; a needle that does not literally occur in the file fails with the surface path named, so fix the wording rather than the needle.

- [ ] **Step 4: Verify**

Run: `python3 scripts/check-claims.py && cd website/docs && npm run build`
Expected: claims pass (no unclaimed surface, every needle found), and the site build publishes the page with every link resolving.

Then `cd website/homepage && npm test` — the homepage content test asserts guide facts and the shared favicon, so it must stay green even though this task does not touch `website/homepage`.

- [ ] **Step 5: Commit**

```bash
git add docs/opentelemetry.md docs/index.md docs/deployment.md docs/claims.toml README.md website/docs/docusaurus.config.ts website/docs/sidebars.ts
git commit -m "docs(otel): publish the OpenTelemetry metrics guide"
```

---

## Final verification

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets`
- [ ] `cargo test --workspace`
- [ ] `scripts/run-pedant.sh strict` for the touched suites
- [ ] `python3 scripts/check-claims.py`
- [ ] `cd website/docs && npm run build`
- [ ] `cd website/homepage && npm test`
- [ ] `docker compose --profile observability up -d --build`, eyeball `/targets` and a scrape, then `down -v --rmi local`
