//! HTTP API for Microwave Server.

pub mod acls;
pub mod bookshelf;
pub mod clients;
pub mod containers;
pub mod cookbooks;
pub mod data_bags;
pub mod depsolver;
pub mod environments;
pub mod error;
pub mod groups;
pub mod helpers;
pub mod keys;
pub mod membership;
pub mod middleware;
pub mod nodes;
pub mod organizations;
pub mod policies;
pub mod roles;
pub mod sandboxes;
pub mod search;
pub mod state;
pub mod users;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

pub use error::ApiError;
pub use microwave_oauth as oauth;
pub use state::{ApiVersion, AppState, AuthContext};

pub fn router(state: AppState) -> Router {
    let inner = Router::new()
        .route("/_status", get(status))
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
        .merge(depsolver::routes())
        .merge(oauth::routes())
        .fallback(not_found)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::authenticate,
        ))
        .with_state(state);

    // Axum adds the `Allow` header for method-not-allowed responses in its
    // top-level `RouteFuture`, after all route middleware has run. Wrapping the
    // whole router lets us rewrite it to the upstream spacing and drop `HEAD`.
    Router::new().fallback_service(tower::service_fn(move |request| {
        let inner = inner.clone();
        async move {
            let mut response = match tower::ServiceExt::oneshot(inner, request).await {
                Ok(response) => response,
                Err(error) => match error {},
            };
            middleware::normalize_allow_header(&mut response);
            Ok::<_, std::convert::Infallible>(response)
        }
    }))
}

async fn status(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "pong",
        "upstreams": [],
        "keygen": { "status": "ok" },
        "indexing": { "mode": state.search.name() }
    }))
}

async fn server_api_version() -> Json<Value> {
    Json(json!({
        "min_api_version": middleware::MIN_API_VERSION,
        "max_api_version": middleware::MAX_API_VERSION
    }))
}

async fn license(State(state): State<AppState>) -> Json<Value> {
    let _ = state;
    Json(json!({
        "limit_exceeded": false,
        "node_license": 1000,
        "node_count": 0,
        "upgrade_url": ""
    }))
}

async fn not_found() -> ApiError {
    ApiError::not_found("not found")
}
