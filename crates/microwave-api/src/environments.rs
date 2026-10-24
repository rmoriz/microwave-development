use crate::cookbooks::{parse_num_versions, NumVersions};
use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_depsolver::{Constraint, Version};
use microwave_store::{Org, Permission, StoredCookbookVersion, StoredObject};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

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
    let container = container_authz(&state, org, "environments")?;
    require_permission(&state, &auth, &container, Permission::Read)?;

    let base = base_url(&headers, &state);
    let mut environments = serde_json::Map::new();
    for environment in state
        .store
        .list_environments(&org.id)
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
    Json(mut body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "environments")?;
    require_permission(&state, &auth, &container, Permission::Create)?;

    let name = normalize_environment(&mut body)?;
    let environment = state
        .store
        .create_environment(
            &org.id,
            &name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
            &auth.principal.authz_id,
        )
        .map_err(ApiError::from)?;
    crate::search::index::index_object(
        &state,
        org,
        crate::search::IndexKind::Environment,
        &environment,
    )
    .await;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/environments/{name}", org.name)
        })),
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
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("environment {name}")))?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Read)?;
    let value =
        serde_json::from_str(&environment.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn update_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    protect_default(&name)?;
    let org = organization(&auth)?;
    let environment = state
        .store
        .find_environment(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("environment {name}")))?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Update)?;

    let body_name = normalize_environment(&mut body)?;
    if body_name != name {
        return Err(ApiError::bad_request(
            "environment name does not match request path",
        ));
    }
    let updated = state
        .store
        .update_environment(
            &org.id,
            &name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
        )
        .map_err(ApiError::from)?;
    crate::search::index::index_object(
        &state,
        org,
        crate::search::IndexKind::Environment,
        &updated,
    )
    .await;
    let value =
        serde_json::from_str(&updated.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn delete_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    protect_default(&name)?;
    let org = organization(&auth)?;
    let environment = state
        .store
        .find_environment(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("environment {name}")))?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Delete)?;
    state
        .store
        .delete_environment(&org.id, &name)
        .map_err(ApiError::from)?;
    crate::search::index::delete_document(&state, &org.id, &environment.id).await;
    Ok(Json(json!({})))
}

async fn list_environment_cookbooks(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, environment_name)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let environment = find_environment_for_cookbooks(&state, &auth, org, &environment_name)?;
    let num_versions = parse_num_versions(query.get("num_versions"), NumVersions::Count(1))?;

    let versions = state
        .store
        .list_cookbook_versions(&org.id)
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

async fn get_environment_cookbook(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, environment_name, cookbook)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let environment = find_environment_for_cookbooks(&state, &auth, org, &environment_name)?;
    let num_versions = parse_num_versions(query.get("num_versions"), NumVersions::All)?;

    let versions = state
        .store
        .list_cookbook_versions(&org.id)
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

fn find_environment_for_cookbooks(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    name: &str,
) -> Result<StoredObject, ApiError> {
    let environment = state
        .store
        .find_environment(&org.id, name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("Cannot load environment {name}")))?;
    require_permission(state, auth, &environment.authz_id, Permission::Read)?;
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

fn protect_default(name: &str) -> Result<(), ApiError> {
    if name == "_default" {
        Err(ApiError::method_not_allowed(
            "The '_default' environment cannot be modified.",
        ))
    } else {
        Ok(())
    }
}

fn normalize_environment(body: &mut Value) -> Result<String, ApiError> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("environment name is required"))?
        .to_string();
    body["name"] = json!(name);
    body["json_class"] = json!("Chef::Environment");
    body["chef_type"] = json!("environment");
    if body.get("cookbook_versions").is_none() {
        body["cookbook_versions"] = json!({});
    }
    if body.get("default_attributes").is_none() {
        body["default_attributes"] = json!({});
    }
    if body.get("override_attributes").is_none() {
        body["override_attributes"] = json!({});
    }
    Ok(name)
}
