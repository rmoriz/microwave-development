use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::Permission;
use serde_json::{json, Value};

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

async fn list_data_bags(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "data")?;
    require_permission(&state, &auth, &container, Permission::Read)?;

    let base = base_url(&headers, &state);
    let mut bags = serde_json::Map::new();
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

    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("data bag name is required"))?;
    state
        .store
        .create_data_bag(&org.id, name, &auth.principal.authz_id)
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
        .ok_or_else(|| ApiError::not_found(format!("data bag {bag_name}")))?;
    require_permission(&state, &auth, &bag.authz_id, Permission::Read)?;

    let base = base_url(&headers, &state);
    let mut items = serde_json::Map::new();
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
        .ok_or_else(|| ApiError::not_found(format!("data bag {bag_name}")))?;
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
    Ok(Json(json!({})))
}

async fn create_data_bag_item(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, bag_name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let bag = state
        .store
        .find_data_bag(&org.id, &bag_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("data bag {bag_name}")))?;
    require_permission(&state, &auth, &bag.authz_id, Permission::Create)?;

    let item_name = body
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("data bag item id is required"))?;
    let item = state
        .store
        .create_data_bag_item(
            &org.id,
            &bag_name,
            item_name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
        )
        .map_err(ApiError::from)?;
    crate::search::index::index_data_bag_item(&state, org, &item).await;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/data/{bag_name}/{item_name}", org.name)
        })),
    ))
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
        .ok_or_else(|| ApiError::not_found(format!("data bag {bag_name}")))?;
    require_permission(&state, &auth, &bag.authz_id, Permission::Read)?;

    let item = state
        .store
        .find_data_bag_item(&org.id, &bag_name, &item_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("data bag item {bag_name}/{item_name}")))?;
    let value = serde_json::from_str(&item.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn update_data_bag_item(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, bag_name, item_name)): Path<(String, String, String)>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let bag = state
        .store
        .find_data_bag(&org.id, &bag_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("data bag {bag_name}")))?;

    let existing = state
        .store
        .find_data_bag_item(&org.id, &bag_name, &item_name)
        .map_err(ApiError::from)?;
    let permission = if existing.is_some() {
        Permission::Update
    } else {
        Permission::Create
    };
    require_permission(&state, &auth, &bag.authz_id, permission)?;

    if let Some(id) = body.get("id").and_then(Value::as_str) {
        if id != item_name {
            return Err(ApiError::bad_request(
                "data bag item id does not match request path",
            ));
        }
    } else {
        body["id"] = json!(item_name);
    }

    let serialized = serde_json::to_string(&body).map_err(|_| ApiError::internal())?;
    let item = if existing.is_some() {
        state
            .store
            .update_data_bag_item(&org.id, &bag_name, &item_name, &serialized)
            .map_err(ApiError::from)?
    } else {
        state
            .store
            .create_data_bag_item(&org.id, &bag_name, &item_name, &serialized)
            .map_err(ApiError::from)?
    };
    crate::search::index::index_data_bag_item(&state, org, &item).await;
    let value = serde_json::from_str(&item.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
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
        .ok_or_else(|| ApiError::not_found(format!("data bag {bag_name}")))?;
    require_permission(&state, &auth, &bag.authz_id, Permission::Delete)?;
    let item = state
        .store
        .find_data_bag_item(&org.id, &bag_name, &item_name)
        .map_err(ApiError::from)?;
    state
        .store
        .delete_data_bag_item(&org.id, &bag_name, &item_name)
        .map_err(ApiError::from)?;
    if let Some(item) = item {
        crate::search::index::delete_document(&state, &org.id, &item.id).await;
    }
    Ok(Json(json!({})))
}
