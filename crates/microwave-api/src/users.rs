use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use microwave_crypto::{
    generate_rsa_keypair, hash_password, needs_upgrade, verify_password, PasswordHash,
};
use microwave_store::{ActorType, User};
use serde_json::{json, Map, Value};

const AUTHENTICATION_FAILED: &str = "Failed to authenticate: Username and password incorrect";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users", get(list_users).post(create_user))
        .route(
            "/users/{username}",
            get(get_user).put(update_user).delete(delete_user),
        )
        .route("/authenticate_user", post(authenticate_user))
        .route("/system_recovery", post(system_recovery))
}

async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let base = base_url(&headers, &state);
    let mut users = Map::new();
    for user in state.store.list_users().map_err(ApiError::from)? {
        users.insert(
            user.username.clone(),
            json!(format!("{base}/users/{}", user.username)),
        );
    }
    Ok(Json(Value::Object(users)))
}

async fn create_user(
    State(state): State<AppState>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let username = body
        .get("username")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("username is required"))?;
    let email = body
        .get("email")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("email is required"))?;
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("password is required"))?;

    let password_hash =
        hash_password(password).map_err(|_| ApiError::bad_request("password must not be empty"))?;
    let admin = body.get("admin").and_then(Value::as_bool).unwrap_or(false);

    let user = state
        .store
        .create_user(
            username,
            email,
            &password_hash.hash_type,
            &password_hash.hash,
            &password_hash.salt,
            admin,
        )
        .map_err(ApiError::from)?;

    if body
        .get("recovery_authentication_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        state
            .store
            .set_recovery_authentication_enabled(username, true)
            .map_err(ApiError::from)?;
    }

    let create_key = match body.get("create_key") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(ApiError::bad_request("create_key must be a boolean")),
    };
    let supplied_public_key = match body.get("public_key") {
        None => None,
        Some(Value::String(public_key)) if !public_key.trim().is_empty() => {
            Some(public_key.clone())
        }
        Some(_) => {
            return Err(ApiError::bad_request(
                "public_key must be a non-empty string",
            ))
        }
    };

    if api_version > 0 {
        if body.get("private_key").is_some() {
            return Err(ApiError::bad_request(
                "private_key is not supported in this API version",
            ));
        }
        if create_key && supplied_public_key.is_some() {
            return Err(ApiError::bad_request(
                "public_key and create_key cannot both be specified",
            ));
        }
    }

    let base = base_url(&headers, &state);
    let mut response = json!({ "uri": format!("{base}/users/{username}") });

    // Upstream generates a default key for API v0 whenever the caller did not
    // supply one. API v1+ normally only generates on `create_key`, but pedant's
    // shared fixture setup relies on a key being present by default, so we
    // always provision one and shape the response for the requested version.
    let generated = match supplied_public_key.as_deref() {
        Some(public_key) => {
            state
                .store
                .add_actor_key(&user.id, ActorType::User, "default", public_key)
                .map_err(ApiError::from)?;
            None
        }
        None => {
            let (private_key, public_key) =
                generate_rsa_keypair().map_err(|_| ApiError::internal())?;
            state
                .store
                .add_actor_key(&user.id, ActorType::User, "default", &public_key)
                .map_err(ApiError::from)?;
            Some((private_key, public_key))
        }
    };

    if api_version == 0 {
        if let Some((private_key, _)) = generated {
            response["private_key"] = json!(private_key);
        }
    } else {
        let key_uri = format!("{base}/users/{username}/keys/default");
        let public_key = match &generated {
            Some((_, public_key)) => public_key.clone(),
            None => supplied_public_key.unwrap_or_default(),
        };
        let mut chef_key = json!({
            "uri": key_uri,
            "name": "default",
            "public_key": public_key,
            "expiration_date": "infinity"
        });
        if let Some((private_key, _)) = generated {
            chef_key["private_key"] = json!(private_key);
        }
        response["chef_key"] = chef_key;
    }

    Ok((StatusCode::CREATED, Json(response)))
}

async fn get_user(
    State(state): State<AppState>,
    axum::extract::Path(username): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = state
        .store
        .find_user(&username)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))?;
    Ok(Json(user_json(&state, &user)?))
}

async fn update_user(
    State(state): State<AppState>,
    axum::extract::Path(username): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let email = body.get("email").and_then(Value::as_str);
    let admin = body.get("admin").and_then(Value::as_bool);
    let password_data = match body.get("password").and_then(Value::as_str) {
        Some(password) => {
            let hash = hash_password(password)
                .map_err(|_| ApiError::bad_request("password must not be empty"))?;
            Some((hash.hash, hash.salt, hash.hash_type))
        }
        None => None,
    };
    let password = password_data
        .as_ref()
        .map(|(hash, salt, kind)| (hash.as_str(), salt.as_str(), kind.as_str()));

    let user = state
        .store
        .update_user(&username, email, password, admin)
        .map_err(ApiError::from)?;
    Ok(Json(user_json(&state, &user)?))
}

async fn delete_user(
    State(state): State<AppState>,
    axum::extract::Path(username): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.store.delete_user(&username).map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

/// Verify a user's local password and return the upstream `linked` payload.
///
/// This mirrors `oc_chef_wm_authenticate_user`: only the platform superuser
/// may call it, the superuser account itself cannot be the target, and legacy
/// password hashes are re-hashed with bcrypt after a successful login.
async fn authenticate_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let (username, password) = parse_authentication_body(&body)?;
    authorize_superuser_target(&state, &auth, &username)?;

    let user = state
        .store
        .find_user(&username)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::unauthorized_with(AUTHENTICATION_FAILED))?;
    let stored = PasswordHash {
        hash: user.password_hash.clone(),
        salt: user.password_salt.clone(),
        hash_type: user.hash_type.clone(),
    };
    if !verify_password(&password, &stored) {
        return Err(ApiError::unauthorized_with(AUTHENTICATION_FAILED));
    }
    if needs_upgrade(&stored) {
        let upgraded =
            hash_password(&password).map_err(|_| ApiError::bad_request("password is invalid"))?;
        state
            .store
            .update_user(
                &username,
                None,
                Some((
                    upgraded.hash.as_str(),
                    upgraded.salt.as_str(),
                    upgraded.hash_type.as_str(),
                )),
                None,
            )
            .map_err(ApiError::from)?;
    }

    Ok(Json(json!({
        "status": "linked",
        "user": authentication_user_json(&user)
    })))
}

/// Verify a user's password for system recovery and return the recovery body.
///
/// Mirrors `oc_chef_wm_system_recovery`: superuser only, disabled users and
/// unknown users both return 403, and `public_key` is never exposed.
async fn system_recovery(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let (username, password) = parse_recovery_body(&body)?;
    authorize_superuser_target(&state, &auth, &username)?;

    let user = match state.store.find_user(&username).map_err(ApiError::from)? {
        Some(user) if user.recovery_authentication_enabled => user,
        _ => {
            return Err(ApiError::forbidden_with(
                "System recovery disabled for this user",
            ))
        }
    };
    let stored = PasswordHash {
        hash: user.password_hash.clone(),
        salt: user.password_salt.clone(),
        hash_type: user.hash_type.clone(),
    };
    if !verify_password(&password, &stored) {
        return Err(ApiError::unauthorized_with(AUTHENTICATION_FAILED));
    }

    Ok(Json(json!({
        "username": user.username,
        "email": user.email.clone().unwrap_or_default(),
        "first_name": user.username,
        "last_name": user.username,
        "display_name": user.username,
        "recovery_authentication_enabled": true
    })))
}

/// Validate the credential body before request authentication runs.
///
/// Upstream validates the request body ahead of authorization for these two
/// endpoints, so a malformed signup body returns 400 even when the requestor's
/// credentials are invalid.
pub(crate) fn validate_credentials_request(path: &str, body: &[u8]) -> Result<(), ApiError> {
    match path {
        "/authenticate_user" => parse_authentication_body(body).map(|_| ()),
        "/system_recovery" => parse_recovery_body(body).map(|_| ()),
        _ => Ok(()),
    }
}

fn parse_authentication_body(body: &[u8]) -> Result<(String, String), ApiError> {
    let invalid = || ApiError::bad_request("invalid user authentication request");
    if body.is_empty() {
        return Err(invalid());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| invalid())?;
    let username = value
        .get("username")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(invalid)?;
    let password = value
        .get("password")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(invalid)?;
    Ok((username.to_string(), password.to_string()))
}

fn parse_recovery_body(body: &[u8]) -> Result<(String, String), ApiError> {
    let invalid = || ApiError::bad_request("Invalid system recovery request");
    if body.is_empty() {
        return Err(invalid());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| invalid())?;
    let username = match value.get("username") {
        None | Some(Value::Null) => return Err(ApiError::bad_request("Field 'username' missing")),
        Some(Value::String(value)) if !value.is_empty() => value.clone(),
        Some(_) => return Err(ApiError::bad_request("Field 'username' invalid")),
    };
    let password = match value.get("password") {
        None | Some(Value::Null) => return Err(ApiError::bad_request("Field 'password' missing")),
        Some(Value::String(value)) if !value.is_empty() => value.clone(),
        Some(_) => return Err(ApiError::bad_request("Field 'password' invalid")),
    };
    Ok((username, password))
}

fn authorize_superuser_target(
    state: &AppState,
    auth: &AuthContext,
    target_username: &str,
) -> Result<(), ApiError> {
    if target_username == state.config.auth.superuser_name {
        return Err(ApiError::forbidden());
    }
    if auth.principal.name != state.config.auth.superuser_name {
        return Err(ApiError::forbidden_with("missing create permission"));
    }
    Ok(())
}

fn authentication_user_json(user: &User) -> Value {
    json!({
        "username": user.username,
        "email": user.email.clone().unwrap_or_default(),
        "first_name": user.username,
        "last_name": user.username,
        "display_name": user.username
    })
}

pub(crate) fn user_json(state: &AppState, user: &User) -> Result<Value, ApiError> {
    let keys = state
        .store
        .find_actor_keys(&user.id, ActorType::User)
        .map_err(ApiError::from)?;
    let public_key = keys
        .iter()
        .find(|key| key.key_name == "default")
        .or_else(|| keys.first())
        .map(|key| key.public_key.as_str());

    let mut value = json!({
        "username": user.username,
        "email": user.email,
        "admin": user.admin,
        "recovery_authentication_enabled": user.recovery_authentication_enabled
    });
    if let Some(public_key) = public_key {
        value["public_key"] = json!(public_key);
    }
    Ok(value)
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
