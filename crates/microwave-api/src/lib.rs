//! HTTP API for Microwave Server.

pub mod acls;
pub mod actions;
pub mod bookshelf;
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
pub mod membership;
pub mod metrics;
pub mod middleware;
pub mod nodes;
pub mod organizations;
pub mod policies;
pub mod required_recipe;
pub mod roles;
pub mod sandboxes;
pub mod search;
pub mod state;
pub mod users;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

pub use error::ApiError;
pub use metrics::{Metrics, MetricsSnapshot};
pub use microwave_oauth as oauth;
pub use state::{ApiVersion, AppState, AuthContext};

pub fn router(state: AppState) -> Router {
    let mut inner = Router::new()
        .route("/_status", get(status))
        .route("/_stats", get(stats))
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
    if state.config.data_collector.is_configured() {
        inner = inner.merge(data_collector::routes());
    }
    if state.config.compliance_proxy.is_configured() {
        inner = inner.merge(compliance_proxy::routes());
    }
    if state.config.required_recipe.enabled {
        inner = inner.merge(required_recipe::routes());
    }

    let inner = inner
        .fallback(not_found)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::authenticate,
        ))
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

    let snapshot = state.metrics.snapshot();
    let maintenance = state
        .store
        .maintenance_enabled()
        .await
        .map_err(ApiError::from)?;
    let node_count = state.store.count_nodes().await.map_err(ApiError::from)?;

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

        return Ok((
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            body,
        )
            .into_response());
    }

    Ok(Json(json!({
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
    }))
    .into_response())
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
