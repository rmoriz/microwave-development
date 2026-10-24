use crate::helpers::{base_url, is_superuser, organization, parse_public_key, require_permission};
use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_crypto::generate_rsa_keypair;
use microwave_store::{ActorType, Client, Org, Principal, StoreError};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/clients",
            get(list_clients).post(create_client),
        )
        .route(
            "/organizations/{organization}/clients/{client}",
            get(get_client).put(update_client).delete(delete_client),
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
    let container = client_container_authz(&state, org)?;
    require_permission(&state, &auth, &container, microwave_store::Permission::Read)?;
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
    Extension(api_version): Extension<ApiVersion>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let name = client_name(&body)?;
    let validator = optional_bool(&body, "validator")?.unwrap_or(false);
    let admin = optional_bool(&body, "admin")?.unwrap_or(false);
    let create_key = optional_bool(&body, "create_key")?.unwrap_or(false);
    if api_version.0 >= 1 && body.get("private_key").is_some() {
        return Err(ApiError::bad_request("Field 'private_key' is invalid"));
    }
    let public_key = parse_public_key(&body)?;
    if create_key && public_key.is_some() {
        return Err(ApiError::bad_request(
            "public_key and create_key cannot both be specified",
        ));
    }

    authorize_create(&state, &auth, org, validator)?;

    // A validator performs the create with system privilege upstream: the new
    // client itself, not the validator, is added to its ACL. Normal creators
    // are added explicitly.
    let creator = if is_validator_requestor(&state, &auth, org)? {
        None
    } else {
        Some(auth.principal.authz_id.as_str())
    };
    let client = state
        .store
        .create_client_with_acl(&org.id, &name, validator, admin, creator)
        .map_err(client_error)?;

    let generate_key = public_key.is_none();
    let (response_public_key, private_key) = if let Some(public_key) = public_key {
        state
            .store
            .add_actor_key(&client.id, ActorType::Client, "default", &public_key)
            .map_err(ApiError::from)?;
        (Some(public_key), None)
    } else if generate_key {
        let (private_key, public_key) = generate_rsa_keypair().map_err(|_| ApiError::internal())?;
        state
            .store
            .add_actor_key(&client.id, ActorType::Client, "default", &public_key)
            .map_err(ApiError::from)?;
        (Some(public_key), Some(private_key))
    } else {
        (None, None)
    };

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

    crate::search::index::index_client(&state, org, &client).await;

    let base = base_url(&headers, &state);
    let uri = format!("{base}/organizations/{}/clients/{}", org.name, name);
    let response = if let Some(private_key) = private_key {
        let key_uri = format!("{uri}/keys/default");
        if api_version.0 == 0 {
            json!({
                "uri": uri,
                "public_key": response_public_key,
                "private_key": private_key,
            })
        } else {
            let mut value = json!({
                "uri": uri,
                "chef_key": {
                    "uri": key_uri,
                    "name": "default",
                    "private_key": private_key,
                    "public_key": response_public_key,
                    "expiration_date": "infinity"
                }
            });
            if !create_key {
                // The implicit default-key path is also exposed as
                // top-level key fields for clients that predate chef_key.
                value["public_key"] = json!(response_public_key);
                value["private_key"] = value["chef_key"]["private_key"].clone();
            }
            value
        }
    } else if let Some(public_key) = response_public_key {
        json!({
            "uri": uri,
            "public_key": public_key,
        })
    } else {
        json!({ "uri": uri })
    };

    Ok((StatusCode::CREATED, Json(response)))
}

async fn get_client(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let client = find_client(&state, org, &client_name)?;
    authorize_client(&state, &auth, &client, microwave_store::Permission::Read)?;
    let base = base_url(&headers, &state);
    Ok(Json(client_json(&state, org, &client, &base)?))
}

async fn update_client(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let client = find_client(&state, org, &client_name)?;
    authorize_client(&state, &auth, &client, microwave_store::Permission::Update)?;

    let new_name = client_destination_name(&body, &client_name)?;
    let validator = optional_bool(&body, "validator")?;
    if body.get("create_key").is_some() {
        return Err(ApiError::bad_request("Field 'create_key' is invalid"));
    }
    let private_key_flag = match body.get("private_key") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(ApiError::bad_request("Field 'private_key' is invalid")),
    };
    let public_key = parse_public_key(&body)?;
    if private_key_flag && public_key.is_some() {
        return Err(ApiError::bad_request(
            "public_key and private_key cannot both be specified",
        ));
    }
    if new_name != client_name {
        validate_client_name(&new_name)?;
    }

    let renamed = new_name != client_name;
    let client = if renamed || validator.is_some() {
        state
            .store
            .update_client(&org.id, &client_name, &new_name, validator)
            .map_err(client_error)?
    } else {
        client
    };

    let mut private_key = None;
    if private_key_flag {
        let (generated_private, generated_public) =
            generate_rsa_keypair().map_err(|_| ApiError::internal())?;
        state
            .store
            .update_or_create_default_key(&client.id, ActorType::Client, &generated_public)
            .map_err(ApiError::from)?;
        private_key = Some(generated_private);
    } else if let Some(public_key) = public_key {
        state
            .store
            .update_or_create_default_key(&client.id, ActorType::Client, &public_key)
            .map_err(ApiError::from)?;
    }

    let base = base_url(&headers, &state);
    let mut response = client_json(&state, org, &client, &base)?;
    response["private_key"] = match private_key {
        Some(private_key) => json!(private_key),
        None => json!(false),
    };
    let status = if renamed {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(response)))
}

async fn delete_client(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let client = find_client(&state, org, &client_name)?;
    authorize_client(&state, &auth, &client, microwave_store::Permission::Delete)?;
    let base = base_url(&headers, &state);
    let response = client_json(&state, org, &client, &base)?;
    state
        .store
        .delete_client(&org.id, &client_name)
        .map_err(ApiError::from)?;
    crate::search::index::delete_document(&state, &org.id, &client.id).await;
    Ok(Json(response))
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

fn authorize_create(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    creating_validator: bool,
) -> Result<(), ApiError> {
    if is_superuser(auth) {
        return Ok(());
    }
    if auth.principal.actor_type == ActorType::Client {
        if creating_validator {
            return Err(ApiError::forbidden());
        }
        if is_validator_requestor(state, auth, org)? {
            return Ok(());
        }
    }
    let container = client_container_authz(state, org)?;
    require_permission(state, auth, &container, microwave_store::Permission::Create)
}

fn authorize_client(
    state: &AppState,
    auth: &AuthContext,
    client: &Client,
    permission: microwave_store::Permission,
) -> Result<(), ApiError> {
    if is_superuser(auth) {
        return Ok(());
    }
    // Normal clients may always manage themselves; validators may not.
    if auth.principal.actor_type == ActorType::Client
        && auth.principal.name == client.name
        && !client.validator
    {
        return Ok(());
    }
    require_permission(state, auth, &client.authz_id, permission)
}

fn is_validator_requestor(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
) -> Result<bool, ApiError> {
    if auth.principal.actor_type != ActorType::Client {
        return Ok(false);
    }
    Ok(state
        .store
        .find_client(&org.id, &auth.principal.name)
        .map_err(ApiError::from)?
        .is_some_and(|client| client.validator))
}

fn client_container_authz(state: &AppState, org: &Org) -> Result<String, ApiError> {
    Ok(state
        .store
        .find_container(&org.id, "clients")
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found("container clients"))?
        .authz_id)
}

fn find_client(state: &AppState, org: &Org, name: &str) -> Result<Client, ApiError> {
    state
        .store
        .find_client(&org.id, name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("client {name}")))
}

fn client_name(body: &Value) -> Result<String, ApiError> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| body.get("clientname").and_then(Value::as_str))
        .unwrap_or_default();
    if name.is_empty() {
        return Err(ApiError::bad_request("Field 'name' missing"));
    }
    validate_client_name(name)?;
    Ok(name.to_string())
}

fn client_destination_name(body: &Value, path_name: &str) -> Result<String, ApiError> {
    let name = match body.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.as_str()),
        Some(_) => return Err(ApiError::bad_request("Field 'name' is invalid")),
    };
    let clientname = match body.get("clientname") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.as_str()),
        Some(_) => return Err(ApiError::bad_request("Field 'clientname' is invalid")),
    };
    if let (Some(name), Some(clientname)) = (name, clientname) {
        if name != clientname {
            return Err(ApiError::bad_request("Field 'name' is invalid"));
        }
    }
    let name = name.or(clientname).unwrap_or(path_name);
    if name.is_empty() {
        return Err(ApiError::bad_request("Field 'name' is invalid"));
    }
    Ok(name.to_string())
}

fn validate_client_name(name: &str) -> Result<(), ApiError> {
    let valid = !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '_'
                || character == '-'
                || character == '.'
        });
    if valid {
        Ok(())
    } else {
        Err(ApiError::bad_request(format!(
            "Invalid client name '{name}' using regex: 'Malformed client name.  Must be A-Z, a-z, 0-9, _, -, or .'."
        )))
    }
}

fn optional_bool(body: &Value, field: &str) -> Result<Option<bool>, ApiError> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ApiError::bad_request(format!("Field '{field}' is invalid"))),
    }
}

fn client_error(error: StoreError) -> ApiError {
    match error {
        StoreError::Conflict(_) => ApiError::conflict("Client already exists"),
        other => other.into(),
    }
}

fn client_json(
    state: &AppState,
    org: &Org,
    client: &Client,
    _base: &str,
) -> Result<Value, ApiError> {
    let keys = state
        .store
        .find_actor_keys(&client.id, ActorType::Client)
        .map_err(ApiError::from)?;
    let public_key = keys
        .iter()
        .find(|key| key.key_name == "default")
        .or_else(|| keys.first())
        .map(|key| key.public_key.as_str());
    let mut value = json!({
        "name": client.name,
        "clientname": client.name,
        "validator": client.validator,
        "orgname": org.name,
        "json_class": "Chef::ApiClient",
        "chef_type": "client",
    });
    if let Some(public_key) = public_key {
        value["public_key"] = json!(public_key);
    }
    Ok(value)
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
