use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::Permission;
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/data",
            get(list_data_bags).post(create_data_bag),
        )
        .route(
            "/organizations/{organization}/data/{bag}",
            get(get_data_bag)
                .post(create_data_bag_item)
                .delete(delete_data_bag),
        )
        .route(
            "/organizations/{organization}/data/{bag}/{item}",
            get(get_data_bag_item)
                .put(update_data_bag_item)
                .delete(delete_data_bag_item),
        )
}

fn valid_name_token(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '_' | '-' | ':' | '.')
        })
}

fn data_bag_not_found(bag: &str) -> ApiError {
    ApiError::not_found(format!("Cannot load data bag {bag}"))
}

fn item_not_found(bag: &str, item: &str) -> ApiError {
    ApiError::not_found(format!(
        "Cannot load data bag item {item} for data bag {bag}"
    ))
}

fn item_not_found_from_delete(bag: &str, item: &str) -> ApiError {
    ApiError::not_found(format!("Cannot load data bag {bag} item {item}"))
}

/// Erchef force-adds `chef_type` and `data_bag` to POST/PUT responses while
/// storing the item body as posted.
fn item_response(bag: &str, item: &Value) -> Value {
    let mut map = item.as_object().cloned().unwrap_or_default();
    map.insert("chef_type".into(), json!("data_bag_item"));
    map.insert("data_bag".into(), json!(bag));
    Value::Object(map)
}

async fn list_data_bags(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "data")?;
    require_permission(&state, &auth, &container, Permission::Read)?;

    let base = base_url(&headers, &state);
    let mut bags = Map::new();
    for bag in state
        .store
        .list_data_bags(&org.id)
        .map_err(ApiError::from)?
    {
        bags.insert(
            bag.name.clone(),
            json!(format!(
                "{base}/organizations/{}/data/{}",
                org.name, bag.name
            )),
        );
    }
    Ok(Json(Value::Object(bags)))
}

async fn create_data_bag(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "data")?;
    require_permission(&state, &auth, &container, Permission::Create)?;

    let name = match body.get("name") {
        None => return Err(ApiError::bad_request("Field 'name' missing")),
        Some(Value::String(name)) if valid_name_token(name) => name.clone(),
        Some(_) => return Err(ApiError::bad_request("Field 'name' invalid")),
    };
    state
        .store
        .create_data_bag(&org.id, &name, &auth.principal.authz_id)
        .map_err(ApiError::from)?;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/data/{name}", org.name)
        })),
    ))
}

async fn get_data_bag(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, bag_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let bag = state
        .store
        .find_data_bag(&org.id, &bag_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| data_bag_not_found(&bag_name))?;
    require_permission(&state, &auth, &bag.authz_id, Permission::Read)?;

    let base = base_url(&headers, &state);
    let mut items = Map::new();
    for item in state
        .store
        .list_data_bag_items(&org.id, &bag_name)
        .map_err(ApiError::from)?
    {
        items.insert(
            item.item_name.clone(),
            json!(format!(
                "{base}/organizations/{}/data/{}/{}",
                org.name, bag_name, item.item_name
            )),
        );
    }
    Ok(Json(Value::Object(items)))
}

async fn delete_data_bag(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, bag_name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let bag = state
        .store
        .find_data_bag(&org.id, &bag_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| data_bag_not_found(&bag_name))?;
    require_permission(&state, &auth, &bag.authz_id, Permission::Delete)?;
    let items = state
        .store
        .list_data_bag_items(&org.id, &bag_name)
        .map_err(ApiError::from)?;
    state
        .store
        .delete_data_bag(&org.id, &bag_name)
        .map_err(ApiError::from)?;
    for item in items {
        crate::search::index::delete_document(&state, &org.id, &item.id).await;
    }
    Ok(Json(json!({
        "name": bag_name,
        "json_class": "Chef::DataBag",
        "chef_type": "data_bag"
    })))
}

async fn create_data_bag_item(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, bag_name)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let bag = state
        .store
        .find_data_bag(&org.id, &bag_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "No data bag '{bag_name}' could be found. Please create this data bag before adding items to it."
            ))
        })?;
    require_permission(&state, &auth, &bag.authz_id, Permission::Create)?;

    let item_name = match body.get("id") {
        None => return Err(ApiError::bad_request("Field 'id' missing")),
        Some(Value::String(id)) if valid_name_token(id) => id.clone(),
        Some(_) => return Err(ApiError::bad_request("Field 'id' invalid")),
    };
    if state
        .store
        .find_data_bag_item(&org.id, &bag_name, &item_name)
        .map_err(ApiError::from)?
        .is_some()
    {
        return Err(ApiError::conflict(format!(
            "Data Bag Item '{item_name}' already exists in Data Bag '{bag_name}'."
        )));
    }

    let item = state
        .store
        .create_data_bag_item(
            &org.id,
            &bag_name,
            &item_name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
        )
        .map_err(ApiError::from)?;
    crate::search::index::index_data_bag_item(&state, org, &item).await;

    Ok((StatusCode::CREATED, Json(item_response(&bag_name, &body))))
}

async fn get_data_bag_item(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, bag_name, item_name)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let bag = state
        .store
        .find_data_bag(&org.id, &bag_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| item_not_found(&bag_name, &item_name))?;
    require_permission(&state, &auth, &bag.authz_id, Permission::Read)?;

    let item = state
        .store
        .find_data_bag_item(&org.id, &bag_name, &item_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| item_not_found(&bag_name, &item_name))?;
    let value = serde_json::from_str(&item.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn update_data_bag_item(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, bag_name, item_name)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let bag = state
        .store
        .find_data_bag(&org.id, &bag_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| item_not_found(&bag_name, &item_name))?;

    if state
        .store
        .find_data_bag_item(&org.id, &bag_name, &item_name)
        .map_err(ApiError::from)?
        .is_none()
    {
        return Err(item_not_found(&bag_name, &item_name));
    }
    require_permission(&state, &auth, &bag.authz_id, Permission::Update)?;

    let mut body = match body {
        Value::Object(map) => Value::Object(map),
        _ => {
            return Err(ApiError::bad_request(
                "Incorrect JSON type for request body",
            ))
        }
    };
    match body.get("id") {
        None => body["id"] = json!(item_name),
        Some(Value::String(id)) if id == &item_name => {}
        Some(_) => {
            return Err(ApiError::bad_request("DataBagItem name mismatch."));
        }
    }
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !valid_name_token(&id) {
        return Err(ApiError::bad_request("Field 'id' invalid"));
    }

    let serialized = serde_json::to_string(&body).map_err(|_| ApiError::internal())?;
    let item = state
        .store
        .update_data_bag_item(&org.id, &bag_name, &item_name, &serialized)
        .map_err(ApiError::from)?;
    crate::search::index::index_data_bag_item(&state, org, &item).await;
    Ok(Json(item_response(&bag_name, &body)))
}

async fn delete_data_bag_item(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, bag_name, item_name)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let bag = state
        .store
        .find_data_bag(&org.id, &bag_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| item_not_found_from_delete(&bag_name, &item_name))?;
    require_permission(&state, &auth, &bag.authz_id, Permission::Delete)?;
    let item = state
        .store
        .find_data_bag_item(&org.id, &bag_name, &item_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| item_not_found_from_delete(&bag_name, &item_name))?;
    let raw_data: Value =
        serde_json::from_str(&item.serialized_object).map_err(|_| ApiError::internal())?;
    state
        .store
        .delete_data_bag_item(&org.id, &bag_name, &item_name)
        .map_err(ApiError::from)?;
    crate::search::index::delete_document(&state, &org.id, &item.id).await;
    Ok(Json(json!({
        "name": format!("data_bag_item_{bag_name}_{item_name}"),
        "json_class": "Chef::DataBagItem",
        "chef_type": "data_bag_item",
        "data_bag": bag_name,
        "raw_data": raw_data
    })))
}
