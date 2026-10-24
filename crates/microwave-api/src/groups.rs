use crate::helpers::{base_url, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::{Group, GroupMembers, Org, Permission};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/groups",
            get(list_groups).post(create_group),
        )
        .route(
            "/organizations/{organization}/groups/{group}",
            get(get_group).put(update_group).delete(delete_group),
        )
}

async fn list_groups(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "groups")?;
    require_permission(&state, &auth, &container, Permission::Read)?;
    let base = base_url(&headers, &state);
    let mut groups = Map::new();
    for group in state.store.list_groups(&org.id).map_err(ApiError::from)? {
        groups.insert(
            group.name.clone(),
            json!(format!(
                "{base}/organizations/{}/groups/{}",
                org.name, group.name
            )),
        );
    }
    Ok(Json(Value::Object(groups)))
}

async fn create_group(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "groups")?;
    require_permission(&state, &auth, &container, Permission::Create)?;
    let name = group_name(&body)?;

    let group = state
        .store
        .create_group_for_org_with_acl(&org.id, &name, &auth.principal.authz_id)
        .map_err(ApiError::from)?;
    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/groups/{}", org.name, group.name)
        })),
    ))
}

async fn get_group(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, group_name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let group = find_group(&state, org, &group_name)?;
    require_permission(&state, &auth, &group.authz_id, Permission::Read)?;
    let members = state
        .store
        .list_group_members(&org.id, &group.authz_id)
        .map_err(ApiError::from)?;
    Ok(Json(group_json(org, &group, &members)))
}

async fn update_group(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, group_name)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let group = find_group(&state, org, &group_name)?;
    require_permission(&state, &auth, &group.authz_id, Permission::Update)?;

    let new_name = match body.get("groupname") {
        None => None,
        Some(Value::String(name)) => {
            validate_group_name(name)?;
            Some(name.clone())
        }
        Some(_) => return Err(ApiError::bad_request("groupname must be a string")),
    };

    let actors = match body.get("actors") {
        None => None,
        Some(Value::Object(actors)) => Some(actors),
        Some(_) => return Err(ApiError::bad_request("actors must be an object")),
    };
    let users = actors
        .map(|actors| optional_string_array(actors.get("users"), "users"))
        .transpose()?
        .flatten();
    let clients = actors
        .map(|actors| optional_string_array(actors.get("clients"), "clients"))
        .transpose()?
        .flatten();
    let groups = actors
        .map(|actors| optional_string_array(actors.get("groups"), "groups"))
        .transpose()?
        .flatten();

    let renamed = new_name.as_deref().is_some_and(|name| name != group_name);
    let group = if renamed {
        state
            .store
            .rename_group(&org.id, &group_name, new_name.as_deref().unwrap())
            .map_err(ApiError::from)?
    } else {
        group
    };

    state
        .store
        .replace_group_members(
            &org.id,
            &group.authz_id,
            users.as_deref(),
            clients.as_deref(),
            groups.as_deref(),
        )
        .map_err(ApiError::from)?;

    let members = state
        .store
        .list_group_members(&org.id, &group.authz_id)
        .map_err(ApiError::from)?;
    let status = if renamed {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(group_json(org, &group, &members))))
}

async fn delete_group(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, group_name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let group = find_group(&state, org, &group_name)?;
    require_permission(&state, &auth, &group.authz_id, Permission::Delete)?;
    if microwave_authz::DEFAULT_GROUPS.contains(&group.name.as_str()) {
        return Err(ApiError::forbidden_with(
            "default organization groups cannot be deleted",
        ));
    }
    let members = state
        .store
        .list_group_members(&org.id, &group.authz_id)
        .map_err(ApiError::from)?;
    let response = group_json(org, &group, &members);
    state
        .store
        .delete_group(&org.id, &group_name)
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

fn container_authz(state: &AppState, org: &Org, name: &str) -> Result<String, ApiError> {
    Ok(state
        .store
        .find_container(&org.id, name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("container {name}")))?
        .authz_id)
}

fn find_group(state: &AppState, org: &Org, name: &str) -> Result<Group, ApiError> {
    state
        .store
        .find_group(&org.id, name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("group {name}")))
}

fn group_name(body: &Value) -> Result<String, ApiError> {
    let name = body
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| body.get("groupname").and_then(Value::as_str))
        .unwrap_or_default();
    validate_group_name(name)?;
    Ok(name.to_string())
}

fn validate_group_name(name: &str) -> Result<(), ApiError> {
    let valid = !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || "_-".contains(character)
        });
    if valid {
        Ok(())
    } else {
        Err(ApiError::bad_request("Invalid group name."))
    }
}

fn optional_string_array(
    value: Option<&Value>,
    field: &str,
) -> Result<Option<Vec<String>>, ApiError> {
    match value {
        None => Ok(None),
        Some(value) => {
            let array = value
                .as_array()
                .ok_or_else(|| ApiError::bad_request(format!("{field} must be an array")))?;
            let values = array
                .iter()
                .map(|item| {
                    item.as_str().map(ToString::to_string).ok_or_else(|| {
                        ApiError::bad_request(format!("{field} must contain strings"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(values))
        }
    }
}

fn group_json(org: &Org, group: &Group, members: &GroupMembers) -> Value {
    let mut actors = members.clients.clone();
    actors.extend(members.users.clone());
    json!({
        "actors": actors,
        "users": members.users,
        "clients": members.clients,
        "groups": members.groups,
        "orgname": org.name,
        "name": group.name,
        "groupname": group.name,
    })
}
