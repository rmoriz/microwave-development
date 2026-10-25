use crate::helpers::{base_url, is_superuser, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use microwave_crypto::parse_public_key_pem;
use microwave_store::{ActorKey, ActorType, Permission};
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/users/{username}/keys",
            get(list_user_keys).post(create_user_key),
        )
        .route(
            "/users/{username}/keys/{key_name}",
            get(get_user_key)
                .put(update_user_key)
                .delete(delete_user_key),
        )
        .route(
            "/organizations/{organization}/clients/{client}/keys",
            get(list_client_keys).post(create_client_key),
        )
        .route(
            "/organizations/{organization}/clients/{client}/keys/{key_name}",
            get(get_client_key)
                .put(update_client_key)
                .delete(delete_client_key),
        )
        .route(
            "/organizations/{organization}/users/{username}/keys",
            get(list_org_user_keys),
        )
        .route(
            "/organizations/{organization}/users/{username}/keys/{key_name}",
            get(get_org_user_key),
        )
}

struct KeyTarget {
    actor_id: String,
    authz_id: String,
    actor_type: ActorType,
    actor_name: String,
    validator: bool,
    uri: String,
}

async fn list_user_keys(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let target = user_target(&state, &headers, &username).await?;
    authorize(&state, &auth, &target, Permission::Read).await?;
    list_keys(&state, &target).await
}

async fn create_user_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let target = user_target(&state, &headers, &username).await?;
    authorize(&state, &auth, &target, Permission::Update).await?;
    create_key(&state, &target, &body).await
}

async fn get_user_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((username, key_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let target = user_target(&state, &headers, &username).await?;
    authorize(&state, &auth, &target, Permission::Read).await?;
    get_key(&state, &target, &key_name).await
}

async fn update_user_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((username, key_name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let target = user_target(&state, &headers, &username).await?;
    authorize(&state, &auth, &target, Permission::Update).await?;
    update_key(&state, &auth, &target, &key_name, &body).await
}

async fn delete_user_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((username, key_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let target = user_target(&state, &headers, &username).await?;
    authorize(&state, &auth, &target, Permission::Update).await?;
    delete_key(&state, &auth, &target, &key_name).await
}

async fn list_client_keys(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let target = client_target(&state, &auth, &headers, &client_name).await?;
    authorize_public_read(&state, &auth).await?;
    list_keys(&state, &target).await
}

async fn create_client_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let target = client_target(&state, &auth, &headers, &client_name).await?;
    authorize(&state, &auth, &target, Permission::Update).await?;
    create_key(&state, &target, &body).await
}

async fn get_client_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name, key_name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let target = client_target(&state, &auth, &headers, &client_name).await?;
    authorize_public_read(&state, &auth).await?;
    get_key(&state, &target, &key_name).await
}

async fn update_client_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name, key_name)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let target = client_target(&state, &auth, &headers, &client_name).await?;
    authorize(&state, &auth, &target, Permission::Update).await?;
    update_key(&state, &auth, &target, &key_name, &body).await
}

async fn delete_client_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, client_name, key_name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let target = client_target(&state, &auth, &headers, &client_name).await?;
    authorize(&state, &auth, &target, Permission::Update).await?;
    delete_key(&state, &auth, &target, &key_name).await
}

async fn list_org_user_keys(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, username)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let target = org_user_target(&state, &auth, &headers, &username).await?;
    authorize_public_read(&state, &auth).await?;
    list_keys(&state, &target).await
}

async fn get_org_user_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, username, key_name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let target = org_user_target(&state, &auth, &headers, &username).await?;
    authorize_public_read(&state, &auth).await?;
    get_key(&state, &target, &key_name).await
}

async fn user_target(
    state: &AppState,
    headers: &HeaderMap,
    username: &str,
) -> Result<KeyTarget, ApiError> {
    let user = state
        .store
        .find_user(username)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))?;
    Ok(KeyTarget {
        actor_id: user.id,
        authz_id: user.authz_id,
        actor_type: ActorType::User,
        actor_name: user.username,
        validator: false,
        uri: format!("{}/users/{username}/keys", base_url(headers, state)),
    })
}

async fn org_user_target(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
    username: &str,
) -> Result<KeyTarget, ApiError> {
    let org = organization(auth)?;
    let user = state
        .store
        .find_user(username)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))?;
    Ok(KeyTarget {
        actor_id: user.id,
        authz_id: user.authz_id,
        actor_type: ActorType::User,
        actor_name: user.username,
        validator: false,
        uri: format!(
            "{}/organizations/{}/users/{username}/keys",
            base_url(headers, state),
            org.name
        ),
    })
}

async fn client_target(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
    client_name: &str,
) -> Result<KeyTarget, ApiError> {
    let org = organization(auth)?;
    let client = state
        .store
        .find_client(&org.id, client_name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("client {client_name}")))?;
    Ok(KeyTarget {
        actor_id: client.id,
        authz_id: client.authz_id,
        actor_type: ActorType::Client,
        actor_name: client.name,
        validator: client.validator,
        uri: format!(
            "{}/organizations/{}/clients/{client_name}/keys",
            base_url(headers, state),
            org.name
        ),
    })
}

/// Keys follow the ACL of their actor. The platform superuser may manage any
/// key, users may manage their own, and non-validator clients may manage their
/// own keys (validators have no self-service permissions).
async fn authorize(
    state: &AppState,
    auth: &AuthContext,
    target: &KeyTarget,
    permission: Permission,
) -> Result<(), ApiError> {
    if is_superuser(auth) {
        return Ok(());
    }
    if auth.principal.name == target.actor_name {
        match target.actor_type {
            ActorType::User => return Ok(()),
            ActorType::Client if !target.validator => return Ok(()),
            ActorType::Client => return Err(ApiError::forbidden()),
        }
    }
    require_permission(state, auth, &target.authz_id, permission).await
}

/// Reads of org-scoped actor keys are governed by membership in the org's
/// `public_key_read_access` group, not by the target actor's ACL.
async fn authorize_public_read(state: &AppState, auth: &AuthContext) -> Result<(), ApiError> {
    if is_superuser(auth) {
        return Ok(());
    }
    let org = organization(auth)?;
    let group = state
        .store
        .find_group(&org.id, "public_key_read_access")
        .await
        .map_err(ApiError::from)?
        .ok_or_else(ApiError::forbidden)?;
    if state
        .store
        .user_in_group(&group.authz_id, &auth.principal.authz_id)
        .await
        .map_err(ApiError::from)?
    {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

async fn list_keys(state: &AppState, target: &KeyTarget) -> Result<Json<Value>, ApiError> {
    let keys = state
        .store
        .list_actor_keys(&target.actor_id, target.actor_type)
        .await
        .map_err(ApiError::from)?;
    let values = keys
        .iter()
        .map(|key| {
            json!({
                "uri": format!("{}/{}", target.uri, key.key_name),
                "name": key.key_name,
                "expired": key.expired,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(Value::Array(values)))
}

async fn get_key(
    state: &AppState,
    target: &KeyTarget,
    key_name: &str,
) -> Result<Json<Value>, ApiError> {
    let key = state
        .store
        .find_actor_key(&target.actor_id, target.actor_type, key_name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("key {key_name}")))?;
    Ok(Json(key_json(target, &key, None)))
}

async fn create_key(
    state: &AppState,
    target: &KeyTarget,
    body: &Value,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let key_name = required_key_name(body)?;
    let expires_at = required_expiration(body)?;
    let (public_key, private_key) = key_material(state, body).await?;

    let key = state
        .store
        .create_actor_key(
            &target.actor_id,
            target.actor_type,
            &key_name,
            &public_key,
            &expires_at,
        )
        .await
        .map_err(ApiError::from)?;
    created_key_response(target, &key, private_key.as_deref())
}

async fn update_key(
    state: &AppState,
    auth: &AuthContext,
    target: &KeyTarget,
    key_name: &str,
    body: &Value,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let has_name = body.get("name").is_some();
    let has_expiration = body.get("expiration_date").is_some();
    let has_public_key = body.get("public_key").is_some();
    let has_create_key = body.get("create_key").is_some();
    if !has_name && !has_expiration && !has_public_key && !has_create_key {
        return Err(ApiError::bad_request(
            "At least one of 'name', 'expiration_date', or 'public_key' must be specified",
        ));
    }

    let new_key_name = match body.get("name") {
        None => None,
        Some(Value::String(name)) => {
            validate_key_name(name)?;
            Some(name.clone())
        }
        Some(_) => return Err(ApiError::bad_request("name must be a string")),
    };
    let expires_at = match body.get("expiration_date") {
        None => None,
        Some(Value::String(value)) => Some(parse_expiration(value)?),
        Some(_) => {
            return Err(ApiError::bad_request(
                "expiration_date must be \"infinity\" or an ISO-8601 timestamp",
            ))
        }
    };
    let (public_key, private_key) = update_key_material(state, body).await?;

    forbid_authenticating_key(auth, target, key_name)?;

    let key = state
        .store
        .update_actor_key(
            &target.actor_id,
            target.actor_type,
            key_name,
            new_key_name.as_deref(),
            public_key.as_deref(),
            expires_at.as_deref(),
        )
        .await
        .map_err(ApiError::from)?;

    let renamed = new_key_name.as_deref().is_some_and(|name| name != key_name);
    // Upstream echoes only the fields that were part of the request.
    let mut response = serde_json::Map::new();
    if has_name {
        response.insert("name".into(), json!(key.key_name));
    }
    if has_public_key || private_key.is_some() {
        response.insert("public_key".into(), json!(key.public_key));
    }
    if has_expiration {
        response.insert("expiration_date".into(), json!(key.expires_at));
    }
    if let Some(private_key) = private_key.as_deref() {
        response.insert("private_key".into(), json!(private_key));
    }
    let uri = format!("{}/{}", target.uri, key.key_name);
    let status = if renamed {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let headers = if renamed {
        location_headers(&uri)?
    } else {
        HeaderMap::new()
    };
    Ok((status, headers, Json(Value::Object(response))))
}

async fn delete_key(
    state: &AppState,
    auth: &AuthContext,
    target: &KeyTarget,
    key_name: &str,
) -> Result<Json<Value>, ApiError> {
    forbid_authenticating_key(auth, target, key_name)?;
    let key = state
        .store
        .find_actor_key(&target.actor_id, target.actor_type, key_name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("key {key_name}")))?;
    state
        .store
        .delete_actor_key(&target.actor_id, target.actor_type, key_name)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(key_json(target, &key, None)))
}

fn created_key_response(
    target: &KeyTarget,
    key: &ActorKey,
    private_key: Option<&str>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    // POST only reports the key's location plus generated private material.
    let uri = format!("{}/{}", target.uri, key.key_name);
    let mut response = json!({ "uri": uri });
    if let Some(private_key) = private_key {
        response["private_key"] = json!(private_key);
    }
    Ok((StatusCode::CREATED, location_headers(&uri)?, Json(response)))
}

/// A request may not modify or delete the key it authenticated with.
fn forbid_authenticating_key(
    auth: &AuthContext,
    target: &KeyTarget,
    key_name: &str,
) -> Result<(), ApiError> {
    if auth.principal.actor_id == target.actor_id && auth.key_name.as_deref() == Some(key_name) {
        return Err(ApiError::forbidden_string(format!(
            "The key '{key_name}' was used to authenticate this request and cannot be modified or deleted."
        )));
    }
    Ok(())
}

fn key_json(target: &KeyTarget, key: &ActorKey, private_key: Option<&str>) -> Value {
    let mut value = json!({
        "uri": format!("{}/{}", target.uri, key.key_name),
        "name": key.key_name,
        "public_key": key.public_key,
        "expiration_date": key.expires_at,
    });
    if let Some(private_key) = private_key {
        value["private_key"] = json!(private_key);
    }
    value
}

fn location_headers(uri: &str) -> Result<HeaderMap, ApiError> {
    let value = HeaderValue::from_str(uri).map_err(|_| ApiError::internal())?;
    let mut headers = HeaderMap::new();
    headers.insert(header::LOCATION, value);
    Ok(headers)
}

fn required_key_name(body: &Value) -> Result<String, ApiError> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("name is required"))?;
    validate_key_name(name)?;
    Ok(name.to_string())
}

fn validate_key_name(name: &str) -> Result<(), ApiError> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-:.".contains(character));
    if valid {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "Malformed key name. A key name can contain only A-Z, a-z, 0-9, _, -, :, or .",
        ))
    }
}

fn required_expiration(body: &Value) -> Result<String, ApiError> {
    let value = body
        .get("expiration_date")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("expiration_date is required"))?;
    parse_expiration(value)
}

fn parse_expiration(value: &str) -> Result<String, ApiError> {
    if value == "infinity" {
        return Ok("infinity".into());
    }
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| {
        ApiError::bad_request("expiration_date must be \"infinity\" or an ISO-8601 timestamp")
    })?;
    Ok(parsed
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Secs, true))
}

async fn key_material(
    state: &AppState,
    body: &Value,
) -> Result<(String, Option<String>), ApiError> {
    let create_key = match body.get("create_key") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(ApiError::bad_request("create_key must be a boolean")),
    };

    match body.get("public_key") {
        Some(_) => {
            if create_key {
                return Err(ApiError::bad_request(
                    "public_key and create_key cannot both be specified",
                ));
            }
            Ok((public_key(body)?, None))
        }
        None if create_key => {
            let (private_key, public_key) = state
                .generate_keypair()
                .await
                .map_err(|_| ApiError::internal())?;
            Ok((public_key, Some(private_key)))
        }
        None => Err(ApiError::bad_request(
            "public_key or create_key is required",
        )),
    }
}

async fn update_key_material(
    state: &AppState,
    body: &Value,
) -> Result<(Option<String>, Option<String>), ApiError> {
    let create_key = match body.get("create_key") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(ApiError::bad_request("create_key must be a boolean")),
    };

    match body.get("public_key") {
        Some(_) => {
            if create_key {
                return Err(ApiError::bad_request(
                    "public_key and create_key cannot both be specified",
                ));
            }
            Ok((Some(public_key(body)?), None))
        }
        None if create_key => {
            let (private_key, public_key) = state
                .generate_keypair()
                .await
                .map_err(|_| ApiError::internal())?;
            Ok((Some(public_key), Some(private_key)))
        }
        None => Ok((None, None)),
    }
}

fn public_key(body: &Value) -> Result<String, ApiError> {
    let public_key = body
        .get("public_key")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("public_key must be a non-empty string"))?;
    if public_key.trim().is_empty() {
        return Err(ApiError::bad_request(
            "public_key must be a non-empty string",
        ));
    }
    parse_public_key_pem(public_key.trim())
        .map_err(|_| ApiError::bad_request("public_key is not a valid PEM public key"))?;
    // Preserve the exact PEM supplied by the client; pedant compares the
    // stored key byte-for-byte, including the trailing newline.
    Ok(public_key.to_string())
}
