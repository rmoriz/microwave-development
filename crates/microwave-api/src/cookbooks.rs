use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::Permission;
use serde_json::{json, Map, Value};

const SEGMENTS: [&str; 9] = [
    "recipes",
    "definitions",
    "libraries",
    "attributes",
    "files",
    "templates",
    "resources",
    "providers",
    "root_files",
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/cookbooks",
            get(list_cookbooks),
        )
        .route(
            "/organizations/{organization}/cookbooks/{cookbook}",
            get(get_cookbook),
        )
        .route(
            "/organizations/{organization}/cookbooks/{cookbook}/{version}",
            get(get_cookbook_version)
                .put(put_cookbook_version)
                .delete(delete_cookbook_version),
        )
}

async fn list_cookbooks(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "cookbooks")?;
    require_permission(&state, &auth, &container, Permission::Read)?;
    Ok(Json(cookbook_map(&state, org, &headers)?))
}

async fn get_cookbook(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .map_err(ApiError::from)?;
    let versions: Vec<_> = versions
        .into_iter()
        .filter(|version| version.name == name)
        .collect();
    if versions.is_empty() {
        return Err(ApiError::not_found(format!("cookbook {name}")));
    }
    let base = base_url(&headers, &state);
    Ok(Json(cookbook_entry(&base, org, &name, &versions)))
}

async fn get_cookbook_version(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, version)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let cookbook = state
        .store
        .find_cookbook_version(&org.id, &name, &version)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("cookbook {name} {version}")))?;
    require_permission(&state, &auth, &cookbook.authz_id, Permission::Read)?;

    let mut manifest: Value =
        serde_json::from_str(&cookbook.manifest).map_err(|_| ApiError::internal())?;
    add_download_urls(&mut manifest, &state, &org.id);
    Ok(Json(manifest))
}

async fn put_cookbook_version(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, version)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(manifest): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "cookbooks")?;
    require_permission(&state, &auth, &container, Permission::Create)?;

    let checksums = manifest_checksums(&manifest);
    let missing: Vec<String> = checksums
        .iter()
        .filter(|checksum| {
            !state
                .store
                .checksum_exists(&org.id, checksum)
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(ApiError::bad_request(format!(
            "manifest references missing checksums: {}",
            missing.join(", ")
        )));
    }

    state
        .store
        .create_cookbook_version(
            &org.id,
            &name,
            &version,
            &serde_json::to_string(&manifest).map_err(|_| ApiError::internal())?,
            &checksums,
            &auth.principal.authz_id,
        )
        .map_err(ApiError::from)?;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/cookbooks/{name}/{version}", org.name)
        })),
    ))
}

async fn delete_cookbook_version(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, version)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let cookbook = state
        .store
        .find_cookbook_version(&org.id, &name, &version)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("cookbook {name} {version}")))?;
    require_permission(&state, &auth, &cookbook.authz_id, Permission::Delete)?;
    state
        .store
        .delete_cookbook_version(&org.id, &name, &version)
        .map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

fn cookbook_map(
    state: &AppState,
    org: &microwave_store::Org,
    headers: &HeaderMap,
) -> Result<Value, ApiError> {
    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .map_err(ApiError::from)?;
    let base = base_url(headers, state);
    let mut names: Vec<String> = versions
        .iter()
        .map(|version| version.name.clone())
        .collect();
    names.sort();
    names.dedup();

    let mut map = Map::new();
    for name in names {
        let selected: Vec<_> = versions
            .iter()
            .filter(|version| version.name == name)
            .cloned()
            .collect();
        map.insert(name.clone(), cookbook_entry(&base, org, &name, &selected));
    }
    Ok(Value::Object(map))
}

fn cookbook_entry(
    base: &str,
    org: &microwave_store::Org,
    name: &str,
    versions: &[microwave_store::StoredCookbookVersion],
) -> Value {
    let versions: Vec<Value> = versions
        .iter()
        .map(|version| {
            json!({
                "version": version.version,
                "url": format!("{base}/organizations/{}/cookbooks/{name}/{}", org.name, version.version)
            })
        })
        .collect();
    json!({
        "url": format!("{base}/organizations/{}/cookbooks/{name}", org.name),
        "versions": versions
    })
}

fn manifest_checksums(manifest: &Value) -> Vec<String> {
    let mut checksums = Vec::new();
    for segment in SEGMENTS {
        if let Some(files) = manifest.get(segment).and_then(Value::as_array) {
            for file in files {
                if let Some(checksum) = file.get("checksum").and_then(Value::as_str) {
                    checksums.push(checksum.to_string());
                }
            }
        }
    }
    checksums
}

pub(crate) fn add_download_urls(manifest: &mut Value, state: &AppState, org_id: &str) {
    for segment in SEGMENTS {
        if let Some(files) = manifest.get_mut(segment).and_then(Value::as_array_mut) {
            for file in files {
                if let Some(checksum) = file.get("checksum").and_then(Value::as_str) {
                    let url = state.object_store.presigned_url("GET", org_id, checksum);
                    file["url"] = json!(url);
                }
            }
        }
    }
}
