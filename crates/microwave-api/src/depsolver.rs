use crate::cookbooks::add_download_urls;
use crate::helpers::{container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Extension, Json, Router};
use microwave_store::{parse_version, Permission, StoredCookbookVersion};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/organizations/{organization}/environments/{environment}/cookbook_versions",
        post(solve),
    )
}

async fn solve(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, _environment)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "cookbooks")?;
    require_permission(&state, &auth, &container, Permission::Read)?;

    let run_list = body
        .get("run_list")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("run_list is required"))?;

    let mut names = Vec::new();
    for item in run_list {
        let Some(item) = item.as_str() else { continue };
        if let Some(recipe) = item.strip_prefix("recipe[") {
            let recipe = recipe.trim_end_matches(']');
            let cookbook = recipe.split("::").next().unwrap_or(recipe);
            if !cookbook.is_empty() {
                names.push(cookbook.to_string());
            }
        }
    }
    names.sort();
    names.dedup();

    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .map_err(ApiError::from)?;

    let mut result = Map::new();
    for name in names {
        let selected = select_latest(&versions, &name).ok_or_else(|| {
            ApiError::precondition_failed(format!("cookbook {name} does not exist"))
        })?;
        let mut manifest: Value =
            serde_json::from_str(&selected.manifest).map_err(|_| ApiError::internal())?;
        add_download_urls(&mut manifest, &state, &org.id);
        result.insert(name, manifest);
    }

    Ok(Json(json!({ "cookbook_versions": result })))
}

fn select_latest<'a>(
    versions: &'a [StoredCookbookVersion],
    name: &str,
) -> Option<&'a StoredCookbookVersion> {
    versions
        .iter()
        .filter(|version| version.name == name)
        .max_by_key(|version| parse_version(&version.version).unwrap_or((0, 0, 0)))
}
