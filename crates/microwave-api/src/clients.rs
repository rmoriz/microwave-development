use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_crypto::generate_rsa_keypair;
use microwave_store::{ActorType, Client, Org, Principal};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/clients",
            get(list_clients).post(create_client),
        )
        .route(
            "/organizations/{organization}/clients/{client}",
            get(get_client).delete(delete_client),
        )
        .route(
            "/organizations/{organization}/principals/{name}",
            get(get_principal),
        )
}

async fn list_clients(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let base = base_url(&headers, &state);
    let mut clients = Map::new();
    for client in state.store.list_clients(&org.id).map_err(ApiError::from)? {
        clients.insert(
            client.name.clone(),
            json!(format!(
                "{base}/organizations/{}/clients/{}",
                org.name, client.name
            )),
        );
    }
    Ok(Json(Value::Object(clients)))
}

async fn create_client(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("name is required"))?;
    let validator = body
        .get("validator")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let admin = body.get("admin").and_then(Value::as_bool).unwrap_or(false);

    let client = state
        .store
        .create_client(&org.id, name, validator, admin)
        .map_err(ApiError::from)?;

    let base = base_url(&headers, &state);
    let mut response = json!({
        "uri": format!("{base}/organizations/{}/clients/{}", org.name, name)
    });

    if let Some(public_key) = body.get("public_key").and_then(Value::as_str) {
        state
            .store
            .add_actor_key(&client.id, ActorType::Client, "default", public_key)
            .map_err(ApiError::from)?;
    } else {
        let (private_key, public_key) = generate_rsa_keypair().map_err(|_| ApiError::internal())?;
        state
            .store
            .add_actor_key(&client.id, ActorType::Client, "default", &public_key)
            .map_err(ApiError::from)?;
        response["private_key"] = json!(private_key);
        response["chef_key"] = json!({
            "uri": format!("{base}/organizations/{}/clients/{}/keys/default", org.name, name),
            "name": "default",
            "private_key": private_key,
            "public_key": public_key,
            "expiration_date": "infinity"
        });
    }

    if let Some(group) = state
        .store
        .find_group(&org.id, "clients")
        .map_err(ApiError::from)?
    {
        state
            .store
            .add_group_member(&group.authz_id, &client.authz_id)
            .map_err(ApiError::from)?;
    }

    Ok((StatusCode::CREATED, Json(response)))
}

async fn get_client(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let client = state
        .store
        .find_client(&org.id, &client_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("client {client_name}")))?;
    let base = base_url(&headers, &state);
    Ok(Json(client_json(org, &client, &base)))
}

async fn delete_client(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    state
        .store
        .delete_client(&org.id, &client_name)
        .map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

async fn get_principal(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(api_version): Extension<ApiVersion>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let principals = state
        .store
        .find_principals(Some(&org.id), &name)
        .map_err(ApiError::from)?;
    if principals.is_empty() {
        return Err(ApiError::not_found(format!("principal {name}")));
    }

    let values = principals
        .iter()
        .map(|principal| principal_json(&state, principal))
        .collect::<Result<Vec<_>, _>>()?;

    if api_version.0 == 0 {
        Ok(Json(values.into_iter().next().unwrap_or(Value::Null)))
    } else {
        Ok(Json(json!({ "principals": values })))
    }
}

fn client_json(org: &Org, client: &Client, base: &str) -> Value {
    json!({
        "name": client.name,
        "validator": client.validator,
        "admin": client.admin,
        "uri": format!("{base}/organizations/{}/clients/{}", org.name, client.name)
    })
}

fn principal_json(state: &AppState, principal: &Principal) -> Result<Value, ApiError> {
    let keys = state
        .store
        .find_principal_keys(principal)
        .map_err(ApiError::from)?;
    let public_key = keys.first().map(|key| key.public_key.as_str());
    let mut value = json!({
        "name": principal.name,
        "type": match principal.actor_type {
            ActorType::User => "user",
            ActorType::Client => "client",
        },
        "authz_id": principal.authz_id,
        "org_member": true
    });
    if let Some(public_key) = public_key {
        value["public_key"] = json!(public_key);
    }
    Ok(value)
}

fn organization(auth: &AuthContext) -> Result<&Org, ApiError> {
    auth.org
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("organization is required"))
}

fn base_url(headers: &HeaderMap, state: &AppState) -> String {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or(state.config.server.bind.as_str());
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}
