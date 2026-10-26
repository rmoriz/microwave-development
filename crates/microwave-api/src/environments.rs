use crate::chef_json::ChefJson;
use crate::cookbooks::{parse_num_versions, NumVersions};
use crate::helpers::{
    base_url, container_authz, location_headers, organization, require_permission,
};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_depsolver::{Constraint, Version};
use microwave_store::{Org, Permission, StoredCookbookVersion, StoredObject};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

/// Top-level keys accepted on environment payloads, mirroring `?VALID_KEYS`
/// in `chef_environment.erl`.
const VALID_KEYS: [&str; 7] = [
    "name",
    "description",
    "json_class",
    "chef_type",
    "default_attributes",
    "override_attributes",
    "cookbook_versions",
];

const ATTRIBUTE_KEYS: [&str; 2] = ["default_attributes", "override_attributes"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/environments",
            get(list_environments).post(create_environment),
        )
        .route(
            "/organizations/{organization}/environments/{environment}/cookbooks",
            get(list_environment_cookbooks),
        )
        .route(
            "/organizations/{organization}/environments/{environment}/cookbooks/{cookbook}",
            get(get_environment_cookbook),
        )
        .route(
            "/organizations/{organization}/environments/{environment}/recipes",
            get(list_environment_recipes),
        )
        .route(
            "/organizations/{organization}/environments/{environment}",
            get(get_environment)
                .put(update_environment)
                .delete(delete_environment),
        )
}

async fn list_environments(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "environments").await?;
    require_permission(&state, &auth, &container, Permission::Read).await?;

    let base = base_url(&headers, &state);
    let mut environments = serde_json::Map::new();
    for environment in state
        .store
        .list_environments(&org.id)
        .await
        .map_err(ApiError::from)?
    {
        environments.insert(
            environment.name.clone(),
            json!(format!(
                "{base}/organizations/{}/environments/{}",
                org.name, environment.name
            )),
        );
    }
    Ok(Json(Value::Object(environments)))
}

async fn create_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    ChefJson(mut body): ChefJson<Value>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "environments").await?;
    require_permission(&state, &auth, &container, Permission::Create).await?;

    let name = normalize_environment(&mut body, None)?;
    let environment = state
        .store
        .create_environment(
            &org.id,
            &name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
            &auth.principal.authz_id,
        )
        .await
        .map_err(ApiError::from)?;
    crate::search::index::index_object(
        &state,
        org,
        crate::search::IndexKind::Environment,
        &environment,
    )
    .await;

    let base = base_url(&headers, &state);
    let uri = format!("{base}/organizations/{}/environments/{name}", org.name);
    Ok((
        StatusCode::CREATED,
        location_headers(&uri)?,
        Json(json!({ "uri": uri })),
    ))
}

async fn get_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let environment = state
        .store
        .find_environment(&org.id, &name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("environment {name}")))?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Read).await?;
    let value =
        serde_json::from_str(&environment.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn update_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    ChefJson(mut body): ChefJson<Value>,
) -> Result<Response, ApiError> {
    if let Some(response) = default_environment_response(&name) {
        return Ok(response);
    }
    let org = organization(&auth)?;
    let environment = state
        .store
        .find_environment(&org.id, &name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("environment {name}")))?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Update).await?;

    normalize_environment(&mut body, Some(&name))?;
    let updated = state
        .store
        .update_environment(
            &org.id,
            &name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
        )
        .await
        .map_err(ApiError::from)?;
    crate::search::index::index_object(
        &state,
        org,
        crate::search::IndexKind::Environment,
        &updated,
    )
    .await;
    Ok(Json(serialized_body(&updated.serialized_object)?).into_response())
}

async fn delete_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    if let Some(response) = default_environment_response(&name) {
        return Ok(response);
    }
    let org = organization(&auth)?;
    let environment = state
        .store
        .find_environment(&org.id, &name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("environment {name}")))?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Delete).await?;
    let value = serialized_body(&environment.serialized_object)?;
    state
        .store
        .delete_environment(&org.id, &name)
        .await
        .map_err(ApiError::from)?;
    crate::search::index::delete_document(&state, &org.id, &environment.id).await;
    Ok(Json(value).into_response())
}

async fn list_environment_cookbooks(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, environment_name)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let environment = find_environment_for_cookbooks(&state, &auth, org, &environment_name).await?;
    let num_versions = parse_num_versions(query.get("num_versions"), NumVersions::Count(1))?;

    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .await
        .map_err(ApiError::from)?;
    let constraints = environment_constraints(&environment)?;
    let base = base_url(&headers, &state);

    let mut grouped: BTreeMap<String, Vec<StoredCookbookVersion>> = BTreeMap::new();
    for version in versions {
        grouped
            .entry(version.name.clone())
            .or_default()
            .push(version);
    }

    let mut result = serde_json::Map::new();
    for (name, mut versions) in grouped {
        filter_versions(&mut versions, constraints.get(&name));
        sort_versions_descending(&mut versions);
        let versions = take_versions(versions, &num_versions);
        result.insert(
            name.clone(),
            environment_cookbook_entry(&base, org, &name, &versions),
        );
    }
    Ok(Json(Value::Object(result)))
}

/// Return the alphabetically sorted, cookbook-qualified recipe names from the
/// best-matching version of every cookbook in the environment. Default recipes
/// are reported as just the cookbook name, matching upstream
/// `chef_wm_environment_recipes`.
async fn list_environment_recipes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, environment_name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let environment = state
        .store
        .find_environment(&org.id, &environment_name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::not_found(format!("Cannot load environment {environment_name}"))
        })?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Read).await?;

    let constraints = environment_constraints(&environment)?;
    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .await
        .map_err(ApiError::from)?;
    let mut grouped: BTreeMap<String, Vec<StoredCookbookVersion>> = BTreeMap::new();
    for version in versions {
        grouped
            .entry(version.name.clone())
            .or_default()
            .push(version);
    }

    let mut recipes = Vec::new();
    for (name, mut versions) in grouped {
        filter_versions(&mut versions, constraints.get(&name));
        sort_versions_descending(&mut versions);
        let best = take_versions(versions, &NumVersions::Count(1));
        let Some(version) = best.into_iter().next() else {
            continue;
        };
        let manifest: Value =
            serde_json::from_str(&version.manifest).map_err(|_| ApiError::internal())?;
        recipes.extend(crate::cookbooks::recipe_names(&name, &manifest));
    }
    recipes.sort();
    Ok(Json(json!(recipes)))
}

async fn get_environment_cookbook(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, environment_name, cookbook)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let environment = find_environment_for_cookbooks(&state, &auth, org, &environment_name).await?;
    let num_versions = parse_num_versions(query.get("num_versions"), NumVersions::All)?;

    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .await
        .map_err(ApiError::from)?;
    let mut versions: Vec<_> = versions
        .into_iter()
        .filter(|version| version.name == cookbook)
        .collect();
    if versions.is_empty() {
        return Err(ApiError::not_found(format!(
            "Cannot find a cookbook named {cookbook}"
        )));
    }

    let constraints = environment_constraints(&environment)?;
    filter_versions(&mut versions, constraints.get(&cookbook));
    sort_versions_descending(&mut versions);
    let versions = take_versions(versions, &num_versions);
    let base = base_url(&headers, &state);
    let mut result = serde_json::Map::new();
    result.insert(
        cookbook.clone(),
        environment_cookbook_entry(&base, org, &cookbook, &versions),
    );
    Ok(Json(Value::Object(result)))
}

async fn find_environment_for_cookbooks(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    name: &str,
) -> Result<StoredObject, ApiError> {
    let environment = state
        .store
        .find_environment(&org.id, name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("Cannot load environment {name}")))?;
    require_permission(state, auth, &environment.authz_id, Permission::Read).await?;
    Ok(environment)
}

fn environment_constraints(
    environment: &StoredObject,
) -> Result<BTreeMap<String, Constraint>, ApiError> {
    let value: Value =
        serde_json::from_str(&environment.serialized_object).map_err(|_| ApiError::internal())?;
    let mut constraints = BTreeMap::new();
    if let Some(object) = value.get("cookbook_versions").and_then(Value::as_object) {
        for (name, value) in object {
            if let Some(text) = value.as_str() {
                if let Ok(constraint) = Constraint::parse(text) {
                    constraints.insert(name.clone(), constraint);
                }
            }
        }
    }
    Ok(constraints)
}

#[allow(clippy::ptr_arg)]
fn filter_versions(versions: &mut Vec<StoredCookbookVersion>, constraint: Option<&Constraint>) {
    let Some(constraint) = constraint else {
        return;
    };
    versions.retain(|version| {
        Version::parse(&version.version)
            .map(|version| constraint.matches(&version))
            .unwrap_or(false)
    });
}

fn sort_versions_descending(versions: &mut [StoredCookbookVersion]) {
    versions.sort_by_key(|version| std::cmp::Reverse(version_key(&version.version)));
}

fn version_key(version: &str) -> (i64, i64, i64) {
    microwave_store::parse_version(version).unwrap_or((0, 0, 0))
}

fn take_versions(
    versions: Vec<StoredCookbookVersion>,
    num_versions: &NumVersions,
) -> Vec<StoredCookbookVersion> {
    match num_versions {
        NumVersions::All => versions,
        NumVersions::Count(count) => versions.into_iter().take(*count).collect(),
    }
}

fn environment_cookbook_entry(
    base: &str,
    org: &Org,
    name: &str,
    versions: &[StoredCookbookVersion],
) -> Value {
    let versions: Vec<Value> = versions
        .iter()
        .map(|version| {
            json!({
                "url": format!(
                    "{base}/organizations/{}/cookbooks/{name}/{}",
                    org.name, version.version
                ),
                "version": version.version,
            })
        })
        .collect();
    json!({
        "url": format!("{base}/organizations/{}/cookbooks/{name}", org.name),
        "versions": versions
    })
}

/// Upstream advertises `GET` on the `_default` environment and rejects
/// modification with a `405` that carries an `Allow` header.
fn default_environment_response(name: &str) -> Option<Response> {
    if name != "_default" {
        return None;
    }
    let mut response =
        ApiError::method_not_allowed("The '_default' environment cannot be modified.")
            .into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("GET"));
    Some(response)
}

fn serialized_body(serialized: &str) -> Result<Value, ApiError> {
    serde_json::from_str(serialized).map_err(|_| ApiError::internal())
}

/// Validates and normalizes an environment payload following
/// `chef_environment:parse_binary_json/1`.
///
/// `path_name` is `Some` for PUT requests: the body name is optional, but when
/// present it must match the URL. Create requests require an explicit name.
fn normalize_environment(body: &mut Value, path_name: Option<&str>) -> Result<String, ApiError> {
    let Some(object) = body.as_object_mut() else {
        return Err(ApiError::bad_request(
            "Incorrect JSON type for request body",
        ));
    };

    let name = match path_name {
        Some(path_name) => {
            match object.get("name") {
                None => {
                    object.insert("name".into(), json!(path_name));
                }
                Some(Value::String(body_name)) if body_name == path_name => {}
                Some(_) => {
                    return Err(ApiError::bad_request("Environment name mismatch."));
                }
            }
            path_name.to_string()
        }
        None => object
            .get("name")
            .ok_or_else(|| ApiError::bad_request("Field 'name' missing"))?
            .as_str()
            .ok_or_else(|| ApiError::bad_request("Field 'name' invalid"))?
            .to_string(),
    };
    if !valid_name(&name) {
        return Err(ApiError::bad_request("Field 'name' invalid"));
    }
    object.insert("name".into(), json!(name));

    if let Some(key) = object
        .keys()
        .find(|key| !VALID_KEYS.contains(&key.as_str()))
    {
        return Err(ApiError::bad_request(format!(
            "Invalid key {key} in request body"
        )));
    }

    validate_fixed_field(object, "json_class", "Chef::Environment")?;
    validate_fixed_field(object, "chef_type", "environment")?;

    match object.get("description") {
        None => {
            object.insert("description".into(), json!(""));
        }
        Some(Value::String(_)) => {}
        Some(_) => return Err(ApiError::bad_request("Field 'description' invalid")),
    }

    for key in ATTRIBUTE_KEYS {
        match object.get(key) {
            None => {
                object.insert(key.into(), json!({}));
            }
            Some(Value::Object(_)) => {}
            Some(_) => {
                return Err(ApiError::bad_request(format!(
                    "Field '{key}' is not a hash"
                )));
            }
        }
    }

    let cookbook_versions = match object.get("cookbook_versions") {
        None => Map::new(),
        Some(Value::Object(versions)) => {
            validate_cookbook_versions(versions)?;
            versions.clone()
        }
        Some(_) => {
            return Err(ApiError::bad_request(
                "Field 'cookbook_versions' is not a hash",
            ));
        }
    };
    object.insert("cookbook_versions".into(), Value::Object(cookbook_versions));

    Ok(name)
}

fn validate_fixed_field(
    object: &mut Map<String, Value>,
    key: &str,
    expected: &str,
) -> Result<(), ApiError> {
    match object.get(key) {
        None => {
            object.insert(key.into(), json!(expected));
            Ok(())
        }
        Some(Value::String(value)) if value == expected => Ok(()),
        Some(_) => Err(ApiError::bad_request(format!("Field '{key}' invalid"))),
    }
}

fn validate_cookbook_versions(versions: &Map<String, Value>) -> Result<(), ApiError> {
    for (name, constraint) in versions {
        if !valid_name(name) {
            return Err(ApiError::bad_request(format!(
                "Invalid key '{name}' for cookbook_versions"
            )));
        }
        match constraint {
            Value::String(value) if valid_constraint(value) => {}
            Value::String(value) => {
                return Err(ApiError::bad_request(format!(
                    "Invalid value '{value}' for cookbook_versions"
                )));
            }
            value => {
                return Err(ApiError::bad_request(format!(
                    "Invalid value '{}' for cookbook_versions",
                    json_value_text(value)
                )));
            }
        }
    }
    Ok(())
}

/// Upstream's `parse_constraint/1` requires exactly one space between an
/// operator and the version, and a version with two or three numeric parts.
fn valid_constraint(value: &str) -> bool {
    let version = value
        .strip_prefix("<= ")
        .or_else(|| value.strip_prefix(">= "))
        .or_else(|| value.strip_prefix("~> "))
        .or_else(|| value.strip_prefix("= "))
        .or_else(|| value.strip_prefix("< "))
        .or_else(|| value.strip_prefix("> "))
        .unwrap_or(value);
    let parts: Vec<&str> = version.split('.').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn json_value_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}
