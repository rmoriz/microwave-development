//! HTTP API for Microwave Server.

pub mod acls;
pub mod actions;
pub mod bookshelf;
pub mod chef_json;
pub mod clients;
pub mod compliance_proxy;
pub mod containers;
pub mod cookbook_artifacts;
pub mod cookbooks;
pub mod data_bags;
pub mod data_collector;
pub mod depsolver;
pub mod environments;
pub mod error;
pub mod groups;
pub mod health;
pub mod helpers;
pub mod internal_api;
pub mod keys;
pub mod landing;
pub mod mcp;
pub mod membership;
pub mod middleware;
pub mod nodes;
pub mod organizations;
pub mod policies;
mod report_ingest;
pub mod required_recipe;
pub mod roles;
pub mod sandboxes;
pub mod search;
pub mod state;
pub mod users;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use microwave_config::OtelConfig;
use microwave_metrics::{
    DependencyState, HistogramSeries, HttpSeries, KeygenGauges, LATENCY_BUCKETS_SECONDS,
};
use microwave_store::PoolStats;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use subtle::ConstantTimeEq;

pub use error::ApiError;
pub use microwave_metrics::{Metrics, MetricsSnapshot};
pub use microwave_oauth as oauth;
pub use state::{ApiVersion, AppState, AuthContext, StateError};

pub fn router(state: AppState) -> Router {
    let mut inner = Router::new()
        .route("/_status", get(status))
        .route("/_stats", get(stats))
        // The landing assets are mounted inside, next to the auth middleware
        // rather than on the outer router, so they match the trailing-slash
        // normalization that runs in front of this router. They pass the
        // middleware through unauthenticated (see `authenticate_request`).
        // The bare `/css` and `/fonts` spellings (what `/css/` normalizes to)
        // answer 404 with the header block instead of the JSON API fallback.
        .route("/index.html", get(|| async { landing::page() }))
        .route("/favicon.svg", get(|| async { landing::favicon() }))
        .route("/css", get(|| async { landing::not_found() }))
        .route("/css/{file}", get(stylesheet_handler))
        .route("/fonts", get(|| async { landing::not_found() }))
        .route("/fonts/{file}", get(font_handler))
        .route("/server_api_version", get(server_api_version))
        .route("/license", get(license))
        .merge(users::routes())
        .merge(acls::routes())
        .merge(organizations::routes())
        .merge(membership::routes())
        .merge(clients::routes())
        .merge(groups::routes())
        .merge(containers::routes())
        .merge(keys::routes())
        .merge(nodes::routes())
        .merge(policies::routes())
        .merge(environments::routes())
        .merge(roles::routes())
        .merge(data_bags::routes())
        .merge(search::routes())
        .merge(bookshelf::routes())
        .merge(sandboxes::routes())
        .merge(cookbooks::routes())
        .merge(cookbook_artifacts::routes())
        .merge(depsolver::routes())
        .merge(internal_api::routes())
        .merge(oauth::routes());

    // Both optional surfaces are absent from the router when disabled so the
    // fallback produces upstream's 404 for every method.
    if state.config.data_collector.is_enabled() {
        inner = inner.merge(data_collector::routes());
    }
    if state.config.compliance_proxy.is_configured() {
        inner = inner.merge(compliance_proxy::routes());
    }
    if state.config.required_recipe.enabled {
        inner = inner.merge(required_recipe::routes());
    }
    if state.config.mcp.enabled {
        inner = inner.merge(mcp::routes());
    }

    let request_state = state.clone();
    let inner = inner
        .fallback(not_found)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::authenticate,
        ))
        // Captures the matched route template for the request histogram. It
        // wraps `authenticate` (later layers wrap earlier ones), so the `401`
        // that auth answers without reaching a handler still carries its
        // template.
        .layer(axum::middleware::from_fn(middleware::record_route))
        // HTTP/2 carries the request authority in the URI rather than a
        // `Host` header. Normalize it before handlers generate absolute URLs.
        .layer(axum::middleware::from_fn(middleware::ensure_host_header))
        .with_state(state);

    // Axum adds the `Allow` header for method-not-allowed responses in its
    // top-level `RouteFuture`, after all route middleware has run. Wrapping the
    // whole router lets us rewrite it to the upstream spacing and drop `HEAD`.
    //
    // `/version` is served by nginx upstream, outside erchef authentication,
    // so it is mounted on the outer router and bypasses the signed-request
    // middleware (see `chef_http_lb_common`'s unauthenticated `location
    // /version`).
    Router::new()
        .route("/version", get(version))
        .fallback_service(tower::service_fn(move |mut request| {
            let inner = inner.clone();
            async move {
                middleware::normalize_trailing_slash(&mut request);
                let mut response = match tower::ServiceExt::oneshot(inner, request).await {
                    Ok(response) => response,
                    Err(error) => match error {},
                };
                middleware::normalize_allow_header(&mut response);
                Ok::<_, std::convert::Infallible>(response)
            }
        }))
        // Compression sits inside the request context so the access log also
        // covers the encoding work, but both wrap `/version` and the fallback.
        .layer(axum::middleware::from_fn(middleware::compress_response))
        .layer(axum::middleware::from_fn_with_state(
            request_state,
            middleware::observe_request,
        ))
}

async fn stylesheet_handler(Path(file): Path<String>) -> Response {
    landing::stylesheet(&file).unwrap_or_else(landing::not_found)
}

async fn font_handler(Path(file): Path<String>) -> Response {
    landing::font(&file).unwrap_or_else(landing::not_found)
}

async fn status(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let keygen = state.keygen.status();
    let dependencies = state
        .health
        .snapshot(&state.store, &state.search, &state.object_store)
        .await;
    let healthy = dependencies.healthy();
    // Upstream `chef_wm_status` halts with HTTP 500 when any dependency probe
    // fails while still returning the same JSON document. Healthy responses
    // stay 200.
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (
        status,
        Json(json!({
            "status": if healthy { "pong" } else { "fail" },
            "upstreams": dependencies.upstreams(),
            "upstream_details": dependencies.details(),
            "keygen": {
                "status": if keygen.worker_healthy { "ok" } else { "degraded" },
                "cached": keygen.cached,
                "capacity": keygen.capacity,
                "workers": keygen.workers,
                "worker_healthy": keygen.worker_healthy,
                "worker_failures": keygen.worker_failures,
                "warm_up_failures": keygen.warm_up_failures,
                "generated_on_demand": keygen.generated_on_demand,
            },
            "indexing": { "mode": state.search.name() }
        })),
    )
}

#[derive(serde::Deserialize)]
struct StatsQuery {
    format: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatsFormat {
    Json,
    Text,
}

async fn stats(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<StatsQuery>,
) -> Result<Response, ApiError> {
    // Upstream nginx guards `/_stats` with HTTP Basic auth (`auth_basic
    // "Chef Server Admin Stats"`) while `/_status` stays public. The check runs
    // before content negotiation so unauthenticated callers learn nothing about
    // the requested representation.
    if !stats_authorized(&state, &headers) {
        return Ok(stats_unauthorized());
    }

    // Mirrors `chef_wm_stats`: `format=json` serves JSON, `format=text` serves
    // the Prometheus text exposition, and no `format` negotiates on `Accept`.
    // `format=prometheus` is Microwave's existing alias for the same text form.
    // Upstream advertises no content type for any other value, which surfaces
    // as HTTP 406 Not Acceptable.
    let format = match query.format.as_deref() {
        Some("json") => StatsFormat::Json,
        Some("text" | "prometheus") => StatsFormat::Text,
        Some(_) => return Err(stats_not_acceptable()),
        None => {
            if headers
                .get(header::ACCEPT)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("text/plain"))
            {
                StatsFormat::Text
            } else {
                StatsFormat::Json
            }
        }
    };

    let pool = state.store.pool_stats().await;
    let maintenance = state
        .store
        .maintenance_enabled()
        .await
        .map_err(ApiError::from)?;
    let node_count = state.store.count_nodes().await.map_err(ApiError::from)?;
    let keygen = state.keygen.status();
    let dependencies: Vec<DependencyState> = state
        .dependency_states()
        .await
        .into_iter()
        .map(|(name, up)| DependencyState { name, up })
        .collect();

    // Publish the fresh values before rendering: the text view, the JSON view
    // and the OTLP callbacks then read one set of numbers per scrape.
    state.metrics.set_maintenance(maintenance);
    state.metrics.set_nodes(node_count);
    state
        .metrics
        .set_pool(pool.map(|pool| microwave_metrics::PoolGauges {
            idle: pool.idle,
            in_use: pool.in_use,
            max: pool.max,
        }));
    state.metrics.set_keygen(KeygenGauges {
        cached: keygen.cached,
        capacity: keygen.capacity,
        workers: keygen.workers,
        worker_failures: keygen.worker_failures,
        warm_up_failures: keygen.warm_up_failures,
        generated_on_demand: keygen.generated_on_demand,
        worker_healthy: keygen.worker_healthy,
    });
    state.metrics.set_dependencies(dependencies);

    let snapshot = state.metrics.snapshot();
    let maintenance = state.metrics.maintenance();
    let node_count = state.metrics.nodes();
    let pool = state.metrics.pool().map(|pool| PoolStats {
        idle: pool.idle,
        in_use: pool.in_use,
        max: pool.max,
    });

    if format == StatsFormat::Text {
        let mut body = String::new();
        body.push_str(
            "# HELP microwave_server_requests_total Total HTTP requests received.\n\
             # TYPE microwave_server_requests_total counter\n",
        );
        body.push_str(&format!(
            "microwave_server_requests_total {}\n",
            snapshot.total_requests
        ));
        body.push_str(
            "# HELP microwave_server_responses_total HTTP responses by status class.\n\
             # TYPE microwave_server_responses_total counter\n",
        );
        body.push_str(&format!(
            "microwave_server_responses_total{{class=\"2xx\"}} {}\n",
            snapshot.responses_2xx
        ));
        body.push_str(&format!(
            "microwave_server_responses_total{{class=\"4xx\"}} {}\n",
            snapshot.responses_4xx
        ));
        body.push_str(&format!(
            "microwave_server_responses_total{{class=\"5xx\"}} {}\n",
            snapshot.responses_5xx
        ));
        body.push_str(
            "# HELP microwave_server_authentication_failures_total Requests rejected with 401.\n\
             # TYPE microwave_server_authentication_failures_total counter\n",
        );
        body.push_str(&format!(
            "microwave_server_authentication_failures_total {}\n",
            snapshot.authentication_failures
        ));
        body.push_str(
            "# HELP microwave_server_uptime_seconds Seconds since process start.\n\
             # TYPE microwave_server_uptime_seconds gauge\n",
        );
        body.push_str(&format!(
            "microwave_server_uptime_seconds {}\n",
            snapshot.uptime_seconds
        ));
        body.push_str(
            "# HELP microwave_server_maintenance Maintenance mode enabled (1) or disabled (0).\n\
             # TYPE microwave_server_maintenance gauge\n",
        );
        body.push_str(&format!(
            "microwave_server_maintenance {}\n",
            u8::from(maintenance)
        ));
        body.push_str(
            "# HELP microwave_server_nodes Number of nodes in the metadata store.\n\
             # TYPE microwave_server_nodes gauge\n",
        );
        body.push_str(&format!("microwave_server_nodes {node_count}\n"));
        push_http_histograms(&mut body, state.metrics.http_series());
        push_build_info(&mut body, state.search.name());
        push_dependency_gauges(&mut body, &state.metrics.dependencies());
        push_keygen_gauges(&mut body, &state.metrics.keygen());
        push_pool_gauges(&mut body, pool);

        return Ok((
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            body,
        )
            .into_response());
    }

    Ok(Json(stats_json(
        &state.metrics,
        &state.config.otel,
        &snapshot,
        maintenance,
        node_count,
        pool,
    ))
    .into_response())
}

/// Escape a label value for the Prometheus text exposition.
fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Render one label set for the request histogram, in the fixed order route,
/// method, org, status. `le` is appended by the caller.
fn http_labels(entry: &HttpSeries) -> String {
    format!(
        "route=\"{}\",method=\"{}\",org=\"{}\",status=\"{}\"",
        escape_label(&entry.route),
        escape_label(&entry.method),
        escape_label(&entry.org),
        entry.status
    )
}

/// Merge series that render the same label set, so every label set is printed
/// exactly once even if the registry ever hands out a duplicate.
fn collapse_http_series(series: Vec<HttpSeries>) -> Vec<(String, HttpSeries)> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut merged: Vec<(String, HttpSeries)> = Vec::new();
    for entry in series {
        let labels = http_labels(&entry);
        match positions.get(&labels) {
            Some(&position) => {
                let existing = &mut merged[position].1;
                existing.count += entry.count;
                existing.sum += entry.sum;
                for (slot, count) in existing.buckets.iter_mut().zip(entry.buckets) {
                    *slot += count;
                }
            }
            None => {
                positions.insert(labels.clone(), merged.len());
                merged.push((labels, entry));
            }
        }
    }
    merged
}

const REQUEST_DURATION: &str = "microwave_server_request_duration_seconds";

/// Append the request duration histogram, one `_bucket`/`_sum`/`_count` group
/// per label set, with `le` last and `+Inf` before `_sum`.
fn push_http_histograms(body: &mut String, series: Vec<HttpSeries>) {
    body.push_str(
        "# HELP microwave_server_request_duration_seconds HTTP request duration in seconds.\n\
         # TYPE microwave_server_request_duration_seconds histogram\n",
    );
    for (labels, entry) in collapse_http_series(series) {
        for (index, bound) in LATENCY_BUCKETS_SECONDS.iter().enumerate() {
            body.push_str(&format!(
                "{REQUEST_DURATION}_bucket{{{labels},le=\"{bound}\"}} {}\n",
                entry.bucket_counts()[index]
            ));
        }
        body.push_str(&format!(
            "{REQUEST_DURATION}_bucket{{{labels},le=\"+Inf\"}} {}\n",
            entry.bucket_counts()[LATENCY_BUCKETS_SECONDS.len()]
        ));
        body.push_str(&format!(
            "{REQUEST_DURATION}_sum{{{labels}}} {}\n",
            entry.sum()
        ));
        body.push_str(&format!(
            "{REQUEST_DURATION}_count{{{labels}}} {}\n",
            entry.count()
        ));
    }
}

/// Append the `build_info` gauge, which is always 1 and carries the version and
/// the active indexing mode.
fn push_build_info(body: &mut String, indexing_mode: &str) {
    body.push_str(
        "# HELP microwave_server_build_info Build information.\n\
         # TYPE microwave_server_build_info gauge\n",
    );
    body.push_str(&format!(
        "microwave_server_build_info{{service_version=\"{}\",indexing_mode=\"{}\"}} 1\n",
        env!("CARGO_PKG_VERSION"),
        escape_label(indexing_mode)
    ));
}

/// Append one `dependency_up` sample per dependency probe. The raw probe name
/// stays the `dependency` label; `state` mirrors it as `up`/`down`.
fn push_dependency_gauges(body: &mut String, dependencies: &[DependencyState]) {
    body.push_str(
        "# HELP microwave_server_dependency_up Dependency probes, 1 when up.\n\
         # TYPE microwave_server_dependency_up gauge\n",
    );
    for probe in dependencies {
        body.push_str(&format!(
            "microwave_server_dependency_up{{dependency=\"{}\",state=\"{}\"}} {}\n",
            escape_label(probe.name),
            if probe.up { "up" } else { "down" },
            u8::from(probe.up)
        ));
    }
}

/// Append the keygen cache gauges.
fn push_keygen_gauges(body: &mut String, keygen: &KeygenGauges) {
    for (name, help, value) in [
        (
            "capacity",
            "Configured maximum cached keypairs.",
            keygen.capacity as u64,
        ),
        (
            "cached",
            "Keypairs waiting in the cache.",
            keygen.cached as u64,
        ),
        (
            "workers",
            "Background keygen workers that started.",
            keygen.workers as u64,
        ),
        (
            "worker_failures",
            "Cumulative background keygen failures.",
            keygen.worker_failures,
        ),
        (
            "warm_up_failures",
            "Cumulative keygen warm-up failures.",
            keygen.warm_up_failures,
        ),
        (
            "generated_on_demand",
            "Keypairs generated on demand.",
            keygen.generated_on_demand,
        ),
        (
            "worker_healthy",
            "Keygen worker healthy (1) or not (0).",
            u64::from(keygen.worker_healthy),
        ),
    ] {
        body.push_str(&format!(
            "# HELP microwave_server_keygen_{name} {help}\n\
             # TYPE microwave_server_keygen_{name} gauge\n\
             microwave_server_keygen_{name} {value}\n"
        ));
    }
}

/// Append the PostgreSQL pool gauges to a Prometheus text exposition body, if
/// the backend has a pool.
fn push_pool_gauges(body: &mut String, pool: Option<PoolStats>) {
    let Some(pool) = pool else {
        return;
    };
    body.push_str(
        "# HELP microwave_server_db_pool_connections Database pool connections by state.\n\
         # TYPE microwave_server_db_pool_connections gauge\n",
    );
    body.push_str(&format!(
        "microwave_server_db_pool_connections{{state=\"idle\"}} {}\n",
        pool.idle
    ));
    body.push_str(&format!(
        "microwave_server_db_pool_connections{{state=\"in_use\"}} {}\n",
        pool.in_use
    ));
    body.push_str(
        "# HELP microwave_server_db_pool_connections_max Configured pool size.\n\
         # TYPE microwave_server_db_pool_connections_max gauge\n",
    );
    body.push_str(&format!(
        "microwave_server_db_pool_connections_max {}\n",
        pool.max
    ));
}

/// Render the pool gauges as the JSON `db_pool` document, if the backend has a
/// pool.
fn pool_gauges_json(pool: Option<PoolStats>) -> Option<Value> {
    pool.map(|pool| {
        json!({
            "idle": pool.idle,
            "in_use": pool.in_use,
            "max": pool.max,
        })
    })
}

/// The stable `/_stats` key for a dependency probe, if the probe maps onto one.
fn upstream_key(probe: &str) -> Option<&'static str> {
    match probe {
        "chef_sql" => Some("database"),
        "chef_index" | "chef_opensearch" | "chef_elasticsearch" => Some("search"),
        "bookshelf" => Some("bookshelf"),
        _ => None,
    }
}

/// The stable `upstreams` document: one key per dependency class, with the
/// probe status (`pong`/`fail`) as value and `"unknown"` when no probe ran.
fn upstreams_json(dependencies: &[DependencyState]) -> Map<String, Value> {
    ["database", "search", "bookshelf"]
        .into_iter()
        .map(|key| {
            let status = dependencies
                .iter()
                .find(|probe| upstream_key(probe.name) == Some(key))
                .map(|probe| if probe.up { "pong" } else { "fail" })
                .unwrap_or("unknown");
            (key.to_string(), json!(status))
        })
        .collect()
}

/// The endpoint as it may be printed: the `user:pass@` userinfo is dropped.
fn endpoint_without_credentials(endpoint: &str) -> String {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return endpoint.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let Some(at) = rest[..authority_end].rfind('@') else {
        return endpoint.to_string();
    };
    format!("{scheme}://{}", &rest[at + 1..])
}

/// Build the `/_stats` JSON document, with `db_pool` only when the backend has
/// a pool.
fn stats_json(
    metrics: &Metrics,
    otel: &OtelConfig,
    snapshot: &MetricsSnapshot,
    maintenance: bool,
    node_count: u64,
    pool: Option<PoolStats>,
) -> Value {
    let keygen = metrics.keygen();
    let mut body = json!({
        "uptime_seconds": snapshot.uptime_seconds,
        "requests": {
            "total": snapshot.total_requests,
            "responses": {
                "2xx": snapshot.responses_2xx,
                "4xx": snapshot.responses_4xx,
                "5xx": snapshot.responses_5xx,
            },
            "authentication_failures": snapshot.authentication_failures,
        },
        "maintenance": maintenance,
        "nodes": node_count,
        "otel": {
            "enabled": otel.enabled,
            // `null` while export is off, even if an endpoint is configured.
            "endpoint": match (otel.enabled, otel.endpoint.as_deref()) {
                (true, Some(endpoint)) => json!(endpoint_without_credentials(endpoint)),
                _ => Value::Null,
            },
            "last_export_error": metrics.export_error(),
        },
        "keygen": {
            "status": if keygen.worker_healthy { "ok" } else { "degraded" },
            "cached": keygen.cached,
            "capacity": keygen.capacity,
            "workers": keygen.workers,
            "worker_healthy": keygen.worker_healthy,
            "worker_failures": keygen.worker_failures,
            "warm_up_failures": keygen.warm_up_failures,
            "generated_on_demand": keygen.generated_on_demand,
        },
        "upstreams": upstreams_json(&metrics.dependencies()),
    });
    if let Some(db_pool) = pool_gauges_json(pool) {
        body["db_pool"] = db_pool;
    }
    body
}

fn stats_not_acceptable() -> ApiError {
    ApiError {
        status: StatusCode::NOT_ACCEPTABLE,
        message: "Not Acceptable".into(),
        not_found_kind: None,
        error_as_string: false,
    }
}

/// Validate the optional Basic credentials configured for `/_stats`.
///
/// Returns `true` when authentication is disabled. Comparisons are
/// constant-time so malformed credentials do not leak the expected secret.
fn stats_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let Some((expected_username, expected_password)) = state.config.stats.credentials() else {
        return true;
    };
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some((scheme, encoded)) = value.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("basic") {
        return false;
    }
    let Ok(decoded) = STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let Ok(decoded) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((username, password)) = decoded.split_once(':') else {
        return false;
    };
    bool::from(expected_username.as_bytes().ct_eq(username.as_bytes()))
        && bool::from(expected_password.as_bytes().ct_eq(password.as_bytes()))
}

fn stats_unauthorized() -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": ["Unauthorized"] })),
    )
        .into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Chef-Server\""),
    );
    response
}

/// Version manifest served at `GET /version`.
///
/// Upstream nginx serves `version-manifest.txt` as plain text on the load
/// balancer (outside erchef authentication). Microwave emits the same
/// `Key: value` shape with its own package version so operators and tooling get
/// a stable, machine-readable version string.
async fn version() -> Response {
    let version = env!("CARGO_PKG_VERSION");
    let manifest = format!(
        "Package: microwave-server/{version}\n\
         Habitat: microwave-server/{version}\n\
         Member: microwave-server\n"
    );
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        manifest,
    )
        .into_response()
}

async fn server_api_version() -> Json<Value> {
    Json(json!({
        "min_api_version": middleware::MIN_API_VERSION,
        "max_api_version": middleware::MAX_API_VERSION
    }))
}

async fn license(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let node_license = state.config.license.node_license;
    let node_count = state.store.count_nodes().await.map_err(ApiError::from)?;
    Ok(Json(json!({
        "limit_exceeded": node_count > node_license,
        "node_license": node_license,
        "node_count": node_count,
        "upgrade_url": state.config.license.upgrade_url
    })))
}

async fn not_found() -> ApiError {
    ApiError::not_found("not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    const POOL: PoolStats = PoolStats {
        idle: 1,
        in_use: 2,
        max: 3,
    };

    fn snapshot() -> MetricsSnapshot {
        Metrics::new().snapshot()
    }

    #[test]
    fn pool_gauges_keep_their_shape_and_order() {
        let mut body = String::new();
        push_pool_gauges(&mut body, Some(POOL));

        let idle = body
            .find("microwave_server_db_pool_connections{state=\"idle\"} 1\n")
            .expect("idle sample");
        let in_use = body
            .find("microwave_server_db_pool_connections{state=\"in_use\"} 2\n")
            .expect("in_use sample");
        let max = body
            .find("microwave_server_db_pool_connections_max 3\n")
            .expect("max sample");
        assert!(idle < in_use, "idle must precede in_use");
        assert!(in_use < max, "in_use must precede max");
        assert!(body.contains(
            "# HELP microwave_server_db_pool_connections Database pool connections by state.\n\
             # TYPE microwave_server_db_pool_connections gauge\n"
        ));
        assert!(body.contains(
            "# HELP microwave_server_db_pool_connections_max Configured pool size.\n\
             # TYPE microwave_server_db_pool_connections_max gauge\n"
        ));
    }

    #[test]
    fn pool_gauges_append_after_the_node_count() {
        let mut body = String::from(
            "# HELP microwave_server_nodes Number of nodes in the metadata store.\n\
             # TYPE microwave_server_nodes gauge\n\
             microwave_server_nodes 0\n",
        );
        push_pool_gauges(&mut body, Some(POOL));

        let nodes = body.find("microwave_server_nodes 0\n").expect("nodes line");
        let pool = body
            .find("microwave_server_db_pool_connections{state=\"idle\"}")
            .expect("pool block");
        assert!(nodes < pool, "the pool block must follow the node count");
    }

    #[test]
    fn pool_gauges_leave_the_body_alone_without_a_pool() {
        let body = "microwave_server_nodes 0\n";
        let mut with_none = body.to_string();
        push_pool_gauges(&mut with_none, None);
        assert_eq!(with_none, body);
    }

    #[test]
    fn pool_gauges_json_serialises_every_field() {
        assert_eq!(
            serde_json::to_string(&pool_gauges_json(Some(POOL)).unwrap()).unwrap(),
            r#"{"idle":1,"in_use":2,"max":3}"#
        );
        assert!(pool_gauges_json(None).is_none());
    }

    #[test]
    fn stats_json_attaches_the_pool_only_when_present() {
        let metrics = Metrics::new();
        let otel = OtelConfig::default();
        let with_pool = stats_json(&metrics, &otel, &snapshot(), false, 0, Some(POOL));
        assert_eq!(with_pool["db_pool"]["idle"], 1);
        assert_eq!(with_pool["db_pool"]["in_use"], 2);
        assert_eq!(with_pool["db_pool"]["max"], 3);
        assert_eq!(with_pool["nodes"], 0);

        let without_pool = stats_json(&metrics, &otel, &snapshot(), false, 0, None);
        assert!(without_pool.get("db_pool").is_none());
    }

    #[test]
    fn endpoint_credentials_are_dropped() {
        assert_eq!(
            endpoint_without_credentials("https://user:pass@collector:4318"),
            "https://collector:4318"
        );
        assert_eq!(
            endpoint_without_credentials("http://collector:4318"),
            "http://collector:4318"
        );
    }

    #[test]
    fn the_otel_document_hides_credentials_and_off_endpoints() {
        let metrics = Metrics::new();
        let mut otel = OtelConfig {
            enabled: true,
            endpoint: Some("https://user:pass@collector:4318".into()),
            ..OtelConfig::default()
        };
        let enabled = stats_json(&metrics, &otel, &snapshot(), false, 0, None);
        assert_eq!(enabled["otel"]["enabled"], true);
        assert_eq!(enabled["otel"]["endpoint"], "https://collector:4318");

        otel.enabled = false;
        let disabled = stats_json(&metrics, &otel, &snapshot(), false, 0, None);
        assert_eq!(disabled["otel"]["enabled"], false);
        assert!(disabled["otel"]["endpoint"].is_null());
    }

    #[test]
    fn upstreams_use_the_stable_keys() {
        let dependencies = [
            DependencyState {
                name: "chef_sql",
                up: true,
            },
            DependencyState {
                name: "chef_opensearch",
                up: false,
            },
        ];
        let upstreams = upstreams_json(&dependencies);
        assert_eq!(upstreams["database"], "pong");
        assert_eq!(upstreams["search"], "fail");
        assert_eq!(upstreams["bookshelf"], "unknown");
    }
}
