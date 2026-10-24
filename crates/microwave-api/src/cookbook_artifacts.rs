use crate::cookbooks::{delete_blobs, manifest_checksums, render_manifest};
use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::{Org, Permission, StoredCookbookArtifact};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/cookbook_artifacts",
            get(list_artifacts),
        )
        .route(
            "/organizations/{organization}/cookbook_artifacts/",
            get(list_artifacts),
        )
        .route(
            "/organizations/{organization}/cookbook_artifacts/{name}",
            get(get_artifact),
        )
        .route(
            "/organizations/{organization}/cookbook_artifacts/{name}/{identifier}",
            get(get_artifact_version)
                .put(put_artifact_version)
                .delete(delete_artifact_version),
        )
}

async fn list_artifacts(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "cookbook_artifacts")?;
    require_permission(&state, &auth, &container, Permission::Read)?;

    let base = base_url(&headers, &state);
    let artifacts = state
        .store
        .list_cookbook_artifacts(&org.id)
        .map_err(ApiError::from)?;
    let mut map = Map::new();
    for artifact in artifacts {
        map.insert(artifact.name.clone(), artifact_entry(&base, org, &artifact));
    }
    Ok(Json(Value::Object(map)))
}

async fn get_artifact(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "cookbook_artifacts")?;
    require_permission(&state, &auth, &container, Permission::Read)?;

    let artifact = state
        .store
        .find_cookbook_artifact(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(not_found)?;
    let base = base_url(&headers, &state);
    let mut map = Map::new();
    map.insert(artifact.name.clone(), artifact_entry(&base, org, &artifact));
    Ok(Json(Value::Object(map)))
}

async fn get_artifact_version(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path((_organization, name, identifier)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let artifact = state
        .store
        .find_cookbook_artifact_version(&org.id, &name, &identifier)
        .map_err(ApiError::from)?
        .ok_or_else(not_found)?;
    require_permission(&state, &auth, &artifact.authz_id, Permission::Read)?;

    let mut manifest = render_manifest(&artifact.manifest, api_version, Some(&state), &org.id)?;
    remove_json_class(&mut manifest);
    Ok(Json(manifest))
}

async fn put_artifact_version(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, identifier)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let body = normalize_artifact(&body, &name, &identifier)?;

    let existing = state
        .store
        .find_cookbook_artifact_version(&org.id, &name, &identifier)
        .map_err(ApiError::from)?;
    if let Some(existing) = existing {
        require_permission(&state, &auth, &existing.authz_id, Permission::Update)?;
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({ "error": "Cookbook artifact already exists" })),
        ));
    }

    let container = container_authz(&state, org, "cookbook_artifacts")?;
    require_permission(&state, &auth, &container, Permission::Create)?;

    let checksums = manifest_checksums(&body);
    for checksum in &checksums {
        if !state
            .store
            .checksum_exists(&org.id, checksum)
            .map_err(ApiError::from)?
        {
            return Err(ApiError::bad_request(
                "Manifest has a checksum that hasn't been uploaded.",
            ));
        }
    }

    state
        .store
        .create_cookbook_artifact_version(
            &org.id,
            &name,
            &identifier,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
            &checksums,
            &auth.principal.authz_id,
        )
        .map_err(ApiError::from)?;

    Ok((StatusCode::CREATED, Json(body)))
}

async fn delete_artifact_version(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path((_organization, name, identifier)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let artifact = state
        .store
        .find_cookbook_artifact_version(&org.id, &name, &identifier)
        .map_err(ApiError::from)?
        .ok_or_else(not_found)?;
    require_permission(&state, &auth, &artifact.authz_id, Permission::Delete)?;

    let (deleted, removed) = state
        .store
        .delete_cookbook_artifact_version(&org.id, &name, &identifier)
        .map_err(ApiError::from)?;
    delete_blobs(&state, &org.id, &removed);

    let mut manifest = render_manifest(&deleted.manifest, api_version, Some(&state), &org.id)?;
    remove_json_class(&mut manifest);
    Ok(Json(manifest))
}

fn artifact_entry(base: &str, org: &Org, artifact: &StoredCookbookArtifact) -> Value {
    let versions: Vec<Value> = artifact
        .identifiers
        .iter()
        .map(|identifier| {
            json!({
                "url": format!(
                    "{base}/organizations/{}/cookbook_artifacts/{}/{}",
                    org.name, artifact.name, identifier
                ),
                "identifier": identifier,
            })
        })
        .collect();
    json!({
        "url": format!(
            "{base}/organizations/{}/cookbook_artifacts/{}",
            org.name, artifact.name
        ),
        "versions": versions
    })
}

fn normalize_artifact(
    body: &Value,
    url_name: &str,
    url_identifier: &str,
) -> Result<Value, ApiError> {
    let document = body
        .as_object()
        .cloned()
        .ok_or_else(|| ApiError::bad_request("Incorrect JSON type for request body"))?;

    let name = document
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| field_missing("name"))?;
    if !valid_name(name) {
        return Err(field_invalid("name"));
    }
    let identifier = document
        .get("identifier")
        .and_then(Value::as_str)
        .ok_or_else(|| field_missing("identifier"))?;
    if !valid_identifier(identifier) {
        return Err(field_invalid("identifier"));
    }
    if name != url_name {
        return Err(ApiError::bad_request(format!(
            "Field 'name' invalid : {url_name} does not match {name}"
        )));
    }
    if identifier != url_identifier {
        return Err(ApiError::bad_request(format!(
            "Field 'identifier' invalid : {url_identifier} does not match {identifier}"
        )));
    }

    Ok(Value::Object(document))
}

fn remove_json_class(manifest: &mut Value) {
    if let Some(object) = manifest.as_object_mut() {
        object.remove("json_class");
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
        })
}

fn valid_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= 255
        && identifier.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-' | ':')
        })
}

fn field_missing(key: &str) -> ApiError {
    ApiError::bad_request(format!("Field '{key}' missing"))
}

fn field_invalid(key: &str) -> ApiError {
    ApiError::bad_request(format!("Field '{key}' invalid"))
}

fn not_found() -> ApiError {
    ApiError::not_found("not_found")
}
