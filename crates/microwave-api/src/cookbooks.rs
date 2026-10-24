use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, ApiVersion, AppState, AuthContext};
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
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path((_organization, name, version)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let cookbook = state
        .store
        .find_cookbook_version(&org.id, &name, &version)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("cookbook {name} {version}")))?;
    require_permission(&state, &auth, &cookbook.authz_id, Permission::Read)?;

    let stored: Value =
        serde_json::from_str(&cookbook.manifest).map_err(|_| ApiError::internal())?;
    let mut manifest = manifest_for_api_version(&stored, api_version, &name, &version);
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
    for segment in std::iter::once("all_files").chain(SEGMENTS) {
        if let Some(files) = manifest.get(segment).and_then(Value::as_array) {
            for file in files {
                if let Some(checksum) = file.get("checksum").and_then(Value::as_str) {
                    checksums.push(checksum.to_string());
                }
            }
        }
    }
    checksums.sort();
    checksums.dedup();
    checksums
}

/// Convert a stored manifest to the shape requested by the API version.
///
/// API versions 0 and 1 expose the legacy segment arrays, while version 2
/// exposes a single `all_files` list. Stored manifests may be in either shape
/// because both v0 and v2 uploads are accepted.
pub(crate) fn manifest_for_api_version(
    manifest: &Value,
    api_version: u8,
    name: &str,
    version: &str,
) -> Value {
    let mut manifest = manifest.clone();
    if api_version >= 2 {
        to_v2_manifest(&mut manifest, name, version);
    } else {
        to_v0_manifest(&mut manifest);
    }
    manifest
}

fn to_v2_manifest(manifest: &mut Value, name: &str, version: &str) {
    if manifest.get("all_files").is_none() {
        let mut all_files = Vec::new();
        for segment in SEGMENTS {
            if let Some(files) = manifest.get(segment).and_then(Value::as_array) {
                for file in files {
                    let mut entry = file.clone();
                    let file_name = entry
                        .get("name")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .or_else(|| {
                            entry.get("path").and_then(Value::as_str).map(|path| {
                                path.strip_prefix(&format!("{segment}/"))
                                    .unwrap_or(path)
                                    .to_string()
                            })
                        })
                        .unwrap_or_default();
                    entry["name"] = json!(format!("{segment}/{file_name}"));
                    all_files.push(entry);
                }
            }
        }
        if let Some(object) = manifest.as_object_mut() {
            for segment in SEGMENTS {
                object.remove(segment);
            }
        }
        manifest["all_files"] = Value::Array(all_files);
    }

    manifest["cookbook_name"] = json!(name);
    manifest["name"] = json!(format!("{name}-{version}"));
    manifest["version"] = json!(version);
    manifest["chef_type"] = json!("cookbook_version");
    manifest["json_class"] = json!("Chef::CookbookVersion");
}

fn to_v0_manifest(manifest: &mut Value) {
    let Some(all_files) = manifest.get("all_files").and_then(Value::as_array).cloned() else {
        return;
    };

    let mut segments: Vec<(&str, Vec<Value>)> = SEGMENTS
        .iter()
        .map(|segment| (*segment, Vec::new()))
        .collect();
    for file in all_files {
        let file_name = file
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let Some((segment, name)) = legacy_segment(&file_name) else {
            continue;
        };
        let mut entry = file;
        entry["name"] = json!(name);
        if entry.get("path").is_none() {
            let path = file_name
                .strip_prefix(&format!("{segment}/"))
                .unwrap_or(file_name.as_str());
            entry["path"] = json!(path);
        }
        if let Some((_, files)) = segments
            .iter_mut()
            .find(|(candidate, _)| *candidate == segment)
        {
            files.push(entry);
        }
    }

    if let Some(object) = manifest.as_object_mut() {
        object.remove("all_files");
        for (segment, files) in segments {
            object.insert(segment.to_string(), Value::Array(files));
        }
    }
}

/// Map a v2 `all_files` entry name to its v0 segment and file name. Names
/// without a leading segment (for example `metadata.rb`) belong to
/// `root_files`. Files under an unknown segment are dropped, matching upstream.
fn legacy_segment(name: &str) -> Option<(&'static str, String)> {
    let mut parts = name.split('/');
    let first = parts.next()?;
    if let Some(segment) = SEGMENTS.iter().find(|segment| **segment == first) {
        let file_name = parts.last().unwrap_or(first);
        Some((segment, file_name.to_string()))
    } else if !name.contains('/') {
        Some(("root_files", name.to_string()))
    } else {
        None
    }
}

pub(crate) fn add_download_urls(manifest: &mut Value, state: &AppState, org_id: &str) {
    if let Some(files) = manifest.get_mut("all_files").and_then(Value::as_array_mut) {
        add_urls(files, state, org_id);
        return;
    }
    for segment in SEGMENTS {
        if let Some(files) = manifest.get_mut(segment).and_then(Value::as_array_mut) {
            add_urls(files, state, org_id);
        }
    }
}

fn add_urls(files: &mut [Value], state: &AppState, org_id: &str) {
    for file in files {
        if let Some(checksum) = file.get("checksum").and_then(Value::as_str) {
            let url = state.object_store.presigned_url("GET", org_id, checksum);
            file["url"] = json!(url);
        }
    }
}
