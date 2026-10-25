//! `GET /organizations/:org/required_recipe` when the feature is enabled.
//!
//! Upstream serves the contents of a single administrator-provided recipe as
//! plain text. The route is only registered when `required_recipe.enabled` is
//! true; while disabled the normal fallback returns `404` for every method,
//! matching `required_recipe_spec.rb`.

use crate::{ApiError, AppState};
use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/organizations/{organization}/required_recipe",
        get(get_recipe),
    )
}

async fn get_recipe(State(state): State<AppState>) -> Result<Response, ApiError> {
    let path = state
        .config
        .required_recipe
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            tracing::error!("required_recipe is enabled but required_recipe.path is not set");
            ApiError::internal()
        })?;

    let recipe_path = path.to_string();
    let contents = tokio::task::spawn_blocking(move || std::fs::read_to_string(recipe_path))
        .await
        .map_err(|error| {
            tracing::error!(%error, path, "required recipe read task failed");
            ApiError::internal()
        })?
        .map_err(|error| {
            tracing::error!(%error, path, "failed to read required recipe");
            ApiError::internal()
        })?;

    let mut response = contents.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(response)
}
