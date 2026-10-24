use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::{Permission, StoredPolicyGroupAssociation, StoredPolicyRevision};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/policies",
            get(list_policies).post(create_policy),
        )
        .route(
            "/organizations/{organization}/policies/{policy}",
            get(get_policy).delete(delete_policy),
        )
        .route(
            "/organizations/{organization}/policies/{policy}/revisions",
            get(list_policy_revisions).post(create_policy_revision),
        )
        .route(
            "/organizations/{organization}/policies/{policy}/revisions/{revision}",
            get(get_policy_revision).delete(delete_policy_revision),
        )
        .route(
            "/organizations/{organization}/policy_groups",
            get(list_policy_groups).post(create_policy_group),
        )
        .route(
            "/organizations/{organization}/policy_groups/{group}",
            get(get_policy_group).delete(delete_policy_group),
        )
        .route(
            "/organizations/{organization}/policy_groups/{group}/policies/{policy}",
            get(get_policy_group_association)
                .put(put_policy_group_association)
                .delete(delete_policy_group_association),
        )
}

async fn list_policies(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let policies_container = container_authz(&state, org, "policies")?;
    require_permission(&state, &auth, &policies_container, Permission::Read)?;

    let base = base_url(&headers, &state);
    let mut policies = Map::new();
    for revision in state
        .store
        .list_all_policy_revisions(&org.id)
        .map_err(ApiError::from)?
    {
        let entry = policies.entry(revision.name.clone()).or_insert_with(|| {
            json!({
                "uri": format!(
                    "{base}/organizations/{}/policies/{}",
                    org.name, revision.name
                ),
                "revisions": {},
            })
        });
        if let Some(revisions) = entry.get_mut("revisions").and_then(Value::as_object_mut) {
            revisions.insert(revision.revision_id, json!({}));
        }
    }
    Ok(Json(Value::Object(policies)))
}

async fn create_policy(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let (name, revision_id) = validate_policy_revision(&body, None)?;
    require_create_revision_permission(&state, &auth, org, &name)?;
    let serialized = serde_json::to_string(&body).map_err(|_| ApiError::internal())?;
    state
        .store
        .create_policy_revision(
            &org.id,
            &name,
            &revision_id,
            &serialized,
            &auth.principal.authz_id,
        )
        .map_err(ApiError::from)?;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/policies/{name}", org.name)
        })),
    ))
}

async fn get_policy(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let policy = state
        .store
        .find_policy(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("policy {name}")))?;
    require_permission(&state, &auth, &policy.authz_id, Permission::Read)?;
    let revisions = state
        .store
        .list_policy_revisions(&org.id, &name)
        .map_err(ApiError::from)?;
    Ok(Json(json!({ "revisions": revisions_map(&revisions) })))
}

async fn delete_policy(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let policy = state
        .store
        .find_policy(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("policy {name}")))?;
    require_permission(&state, &auth, &policy.authz_id, Permission::Delete)?;
    let revisions = state
        .store
        .list_policy_revisions(&org.id, &name)
        .map_err(ApiError::from)?;
    let body = json!({ "revisions": revisions_map(&revisions) });
    state
        .store
        .delete_policy(&org.id, &name)
        .map_err(ApiError::from)?;
    Ok(Json(body))
}

async fn list_policy_revisions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let policy = state
        .store
        .find_policy(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("policy {name}")))?;
    require_permission(&state, &auth, &policy.authz_id, Permission::Read)?;
    let revisions = state
        .store
        .list_policy_revisions(&org.id, &name)
        .map_err(ApiError::from)?;
    Ok(Json(json!({ "revisions": revisions_map(&revisions) })))
}

async fn create_policy_revision(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let (name, revision_id) = validate_policy_revision(&body, Some(&name))?;
    require_create_revision_permission(&state, &auth, org, &name)?;
    let serialized = serde_json::to_string(&body).map_err(|_| ApiError::internal())?;
    state
        .store
        .create_policy_revision(
            &org.id,
            &name,
            &revision_id,
            &serialized,
            &auth.principal.authz_id,
        )
        .map_err(ApiError::from)?;

    Ok((StatusCode::CREATED, Json(body)))
}

async fn get_policy_revision(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, revision)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let revision = state
        .store
        .find_policy_revision(&org.id, &name, &revision)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("policy {name} revision {revision}")))?;
    require_permission(&state, &auth, &revision.policy_authz_id, Permission::Read)?;

    let mut body: Value =
        serde_json::from_str(&revision.serialized_object).map_err(|_| ApiError::internal())?;
    let groups = state
        .store
        .list_policy_groups_for_revision(&org.id, &name, &revision.revision_id)
        .map_err(ApiError::from)?;
    body["policy_group_list"] = json!(groups);
    Ok(Json(body))
}

async fn delete_policy_revision(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, revision)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let found = state
        .store
        .find_policy_revision(&org.id, &name, &revision)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("policy {name} revision {revision}")))?;
    require_permission(&state, &auth, &found.policy_authz_id, Permission::Delete)?;
    let body: Value =
        serde_json::from_str(&found.serialized_object).map_err(|_| ApiError::internal())?;
    state
        .store
        .delete_policy_revision(&org.id, &name, &revision)
        .map_err(ApiError::from)?;
    Ok(Json(body))
}

async fn list_policy_groups(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let groups_container = container_authz(&state, org, "policy_groups")?;
    require_permission(&state, &auth, &groups_container, Permission::Read)?;

    let base = base_url(&headers, &state);
    let mut groups = Map::new();
    for group in state
        .store
        .list_policy_groups(&org.id)
        .map_err(ApiError::from)?
    {
        let associations = state
            .store
            .list_policy_group_associations(&org.id, &group.name)
            .map_err(ApiError::from)?;
        let mut entry = json!({
            "uri": format!(
                "{base}/organizations/{}/policy_groups/{}",
                org.name, group.name
            ),
        });
        if !associations.is_empty() {
            entry["policies"] = associations_map(&associations);
        }
        groups.insert(group.name, entry);
    }
    Ok(Json(Value::Object(groups)))
}

async fn create_policy_group(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ApiError::bad_request("Field 'name' missing"))?;
    let groups_container = container_authz(&state, org, "policy_groups")?;
    require_permission(&state, &auth, &groups_container, Permission::Create)?;

    let serialized = serde_json::to_string(&body).map_err(|_| ApiError::internal())?;
    state
        .store
        .create_policy_group(&org.id, name, &serialized, &auth.principal.authz_id)
        .map_err(ApiError::from)?;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/policy_groups/{name}", org.name)
        })),
    ))
}

async fn get_policy_group(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let group = state
        .store
        .find_policy_group(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("policy group {name}")))?;
    require_permission(&state, &auth, &group.authz_id, Permission::Read)?;

    let associations = state
        .store
        .list_policy_group_associations(&org.id, &name)
        .map_err(ApiError::from)?;
    let base = base_url(&headers, &state);
    Ok(Json(json!({
        "uri": format!("{base}/organizations/{}/policy_groups/{name}", org.name),
        "policies": associations_map(&associations),
    })))
}

async fn delete_policy_group(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let group = state
        .store
        .find_policy_group(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("policy group {name}")))?;
    require_permission(&state, &auth, &group.authz_id, Permission::Delete)?;
    let associations = state
        .store
        .list_policy_group_associations(&org.id, &name)
        .map_err(ApiError::from)?;
    let base = base_url(&headers, &state);
    let body = json!({
        "uri": format!("{base}/organizations/{}/policy_groups/{name}", org.name),
        "policies": associations_map(&associations),
    });
    state
        .store
        .delete_policy_group(&org.id, &name)
        .map_err(ApiError::from)?;
    Ok(Json(body))
}

async fn get_policy_group_association(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, group, policy)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_policy_and_group(&state, &auth, org, &policy, &group, Permission::Read)?;
    let association = state
        .store
        .find_policy_group_association(&org.id, &group, &policy)
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "Cannot load policy {policy} in policy group {group}"
            ))
        })?;
    let body: Value =
        serde_json::from_str(&association.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(body))
}

async fn put_policy_group_association(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, group, policy)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let (name, revision_id) = validate_policy_revision(&body, Some(&policy))?;

    match state
        .store
        .find_policy(&org.id, &name)
        .map_err(ApiError::from)?
    {
        Some(existing) => {
            require_permission(&state, &auth, &existing.authz_id, Permission::Update)?
        }
        None => {
            let container = container_authz(&state, org, "policies")?;
            require_permission(&state, &auth, &container, Permission::Create)?;
        }
    }
    match state
        .store
        .find_policy_group(&org.id, &group)
        .map_err(ApiError::from)?
    {
        Some(existing) => {
            require_permission(&state, &auth, &existing.authz_id, Permission::Update)?
        }
        None => {
            let container = container_authz(&state, org, "policy_groups")?;
            require_permission(&state, &auth, &container, Permission::Create)?;
        }
    }

    let serialized = serde_json::to_string(&body).map_err(|_| ApiError::internal())?;
    let group_body =
        serde_json::to_string(&json!({ "name": group })).map_err(|_| ApiError::internal())?;
    let (_, created) = state
        .store
        .associate_policy_revision(
            &org.id,
            &group,
            &name,
            &revision_id,
            &serialized,
            &group_body,
            &auth.principal.authz_id,
        )
        .map_err(ApiError::from)?;

    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(body),
    ))
}

async fn delete_policy_group_association(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, group, policy)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_policy_and_group(&state, &auth, org, &policy, &group, Permission::Delete)?;
    let association = state
        .store
        .find_policy_group_association(&org.id, &group, &policy)
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "Cannot load policy {policy} in policy group {group}"
            ))
        })?;
    let body: Value =
        serde_json::from_str(&association.serialized_object).map_err(|_| ApiError::internal())?;
    state
        .store
        .delete_policy_group_association(&org.id, &group, &policy)
        .map_err(ApiError::from)?;
    Ok(Json(body))
}

fn require_create_revision_permission(
    state: &AppState,
    auth: &AuthContext,
    org: &microwave_store::Org,
    name: &str,
) -> Result<(), ApiError> {
    match state
        .store
        .find_policy(&org.id, name)
        .map_err(ApiError::from)?
    {
        Some(existing) => require_permission(state, auth, &existing.authz_id, Permission::Create),
        None => {
            let container = container_authz(state, org, "policies")?;
            require_permission(state, auth, &container, Permission::Create)
        }
    }
}

fn require_policy_and_group(
    state: &AppState,
    auth: &AuthContext,
    org: &microwave_store::Org,
    policy_name: &str,
    group_name: &str,
    permission: Permission,
) -> Result<(), ApiError> {
    let policy = state
        .store
        .find_policy(&org.id, policy_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("policy {policy_name}")))?;
    let group = state
        .store
        .find_policy_group(&org.id, group_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("policy group {group_name}")))?;
    require_permission(state, auth, &policy.authz_id, permission)?;
    require_permission(state, auth, &group.authz_id, permission)?;
    Ok(())
}

fn revisions_map(revisions: &[StoredPolicyRevision]) -> Value {
    let mut map = Map::new();
    for revision in revisions {
        map.insert(revision.revision_id.clone(), json!({}));
    }
    Value::Object(map)
}

fn associations_map(associations: &[StoredPolicyGroupAssociation]) -> Value {
    let mut map = Map::new();
    for association in associations {
        map.insert(
            association.policy_name.clone(),
            json!({ "revision_id": association.revision_id }),
        );
    }
    Value::Object(map)
}

fn validate_policy_revision(
    body: &Value,
    path_name: Option<&str>,
) -> Result<(String, String), ApiError> {
    let object = body
        .as_object()
        .ok_or_else(|| ApiError::bad_request("Incorrect JSON type for request body"))?;

    let revision_id = match object.get("revision_id") {
        None => return Err(ApiError::bad_request("Field 'revision_id' missing")),
        Some(Value::String(value)) if valid_limited_token(value) => value.clone(),
        Some(_) => return Err(ApiError::bad_request("Field 'revision_id' invalid")),
    };

    let name = match object.get("name") {
        None => return Err(ApiError::bad_request("Field 'name' missing")),
        Some(Value::String(value)) if valid_limited_token(value) => value.clone(),
        Some(_) => return Err(ApiError::bad_request("Field 'name' invalid")),
    };

    match object.get("run_list") {
        None => return Err(ApiError::bad_request("Field 'run_list' missing")),
        Some(Value::Array(items)) if items.iter().all(valid_policy_run_list_item) => {}
        Some(_) => {
            return Err(ApiError::bad_request(
                "Field 'run_list' is not a valid run list",
            ))
        }
    }

    match object.get("cookbook_locks") {
        None => return Err(ApiError::bad_request("Field 'cookbook_locks' missing")),
        Some(Value::Object(locks)) => {
            for (cookbook, lock) in locks {
                if !valid_cookbook_name(cookbook) {
                    return Err(ApiError::bad_request("Field 'cookbook_locks' invalid"));
                }
                let lock = lock
                    .as_object()
                    .ok_or_else(|| ApiError::bad_request("Field 'cookbook_locks' invalid"))?;
                match lock.get("identifier") {
                    None => {
                        return Err(ApiError::bad_request("Field 'identifier' missing"));
                    }
                    Some(Value::String(identifier)) if valid_limited_token(identifier) => {}
                    Some(_) => {
                        return Err(ApiError::bad_request("Field 'identifier' invalid"));
                    }
                }
                match lock.get("version") {
                    None => return Err(ApiError::bad_request("Field 'version' missing")),
                    Some(Value::String(version)) if valid_cookbook_version(version) => {}
                    Some(_) => {
                        return Err(ApiError::bad_request(
                            "Field 'version' is not a valid version",
                        ));
                    }
                }
                if let Some(value) = lock.get("dotted_decimal_identifier") {
                    match value {
                        Value::String(version) if valid_cookbook_version(version) => {}
                        _ => {
                            return Err(ApiError::bad_request(
                                "Field 'dotted_decimal_identifier' is not a valid version",
                            ));
                        }
                    }
                }
            }
        }
        Some(_) => return Err(ApiError::bad_request("Field 'cookbook_locks' invalid")),
    }

    if let Some(path_name) = path_name {
        if name != path_name {
            return Err(ApiError::bad_request(format!(
                "Field 'name' invalid : {path_name} does not match {name}"
            )));
        }
    }

    Ok((name, revision_id))
}

fn valid_limited_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '_' | '-' | ':' | '.')
        })
}

fn valid_cookbook_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '-' | '.'))
}

fn valid_policy_run_list_item(value: &Value) -> bool {
    let Some(value) = value.as_str() else {
        return false;
    };
    let Some(inner) = value
        .strip_prefix("recipe[")
        .and_then(|value| value.strip_suffix(']'))
    else {
        return false;
    };
    let mut parts = inner.split("::");
    let cookbook = parts.next().unwrap_or_default();
    let recipe = parts.next().unwrap_or_default();
    parts.next().is_none() && valid_cookbook_name(cookbook) && valid_cookbook_name(recipe)
}

fn valid_cookbook_version(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}
