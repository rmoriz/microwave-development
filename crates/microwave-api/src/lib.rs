//! HTTP API for Microwave Server.

pub mod acls;
pub mod bookshelf;
pub mod clients;
pub mod cookbooks;
pub mod data_bags;
pub mod depsolver;
pub mod environments;
pub mod error;
pub mod helpers;
pub mod middleware;
pub mod nodes;
pub mod organizations;
pub mod roles;
pub mod sandboxes;
pub mod state;
pub mod users;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

pub use error::ApiError;
pub use state::{ApiVersion, AppState, AuthContext};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/_status", get(status))
        .route("/server_api_version", get(server_api_version))
        .route("/license", get(license))
        .merge(users::routes())
        .merge(acls::routes())
        .merge(organizations::routes())
        .merge(clients::routes())
        .merge(nodes::routes())
        .merge(environments::routes())
        .merge(roles::routes())
        .merge(data_bags::routes())
        .merge(bookshelf::routes())
        .merge(sandboxes::routes())
        .merge(cookbooks::routes())
        .merge(depsolver::routes())
        .fallback(not_found)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::authenticate,
        ))
        .with_state(state)
}

async fn status() -> Json<Value> {
    Json(json!({
        "status": "pong",
        "upstreams": [],
        "keygen": { "status": "ok" },
        "indexing": { "mode": "embedded" }
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
