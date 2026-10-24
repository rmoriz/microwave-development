use crate::helpers::{base_url, is_superuser, require_superuser};
use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use microwave_crypto::{
    generate_rsa_keypair, hash_password, needs_upgrade, parse_public_key_pem, verify_password,
    PasswordHash,
};
use microwave_store::{ActorType, User, UserPatch, UserProfile};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

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
    Extension(auth): Extension<AuthContext>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_superuser(&auth)?;
    let base = base_url(&headers, &state);

    if params.get("verbose").map(String::as_str) == Some("true") {
        let mut users = Map::new();
        for user in state.store.list_users().await.map_err(ApiError::from)? {
            users.insert(
                user.username.clone(),
                json!({
                    "email": user.email,
                    "first_name": user.first_name.unwrap_or_default(),
                    "last_name": user.last_name.unwrap_or_default(),
                    "display_name": user.display_name.unwrap_or_default(),
                }),
            );
        }
        return Ok(Json(Value::Object(users)));
    }

    let email = params
        .get("email")
        .map(String::as_str)
        .filter(|value| !value.is_empty());
    let external_uid = params
        .get("external_authentication_uid")
        .map(String::as_str)
        .filter(|value| !value.is_empty());
    let users = state
        .store
        .list_users_filtered(email, external_uid)
        .await
        .map_err(ApiError::from)?;
    let mut body = Map::new();
    for user in users {
        body.insert(
            user.username.clone(),
            json!(format!("{base}/users/{}", user.username)),
        );
    }
    Ok(Json(Value::Object(body)))
}

async fn create_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_superuser(&auth)?;
    let body = body
        .as_object()
        .ok_or_else(|| ApiError::bad_request("request body must be a JSON object"))?;

    let username = username_from_body(body)?;
    validate_username(&username)?;
    let display_name = required_string(body, "display_name")?.to_string();
    let first_name = optional_string(body, "first_name")?;
    let middle_name = optional_string(body, "middle_name")?;
    let last_name = optional_string(body, "last_name")?;
    let external_authentication_uid =
        optional_string(body, "external_authentication_uid")?.filter(|value| !value.is_empty());
    let external_auth = external_authentication_uid.is_some();

    let email = match optional_string(body, "email")? {
        Some(email) if !email.is_empty() => {
            validate_email(&email)?;
            Some(email.to_lowercase())
        }
        _ if external_auth => None,
        _ => return Err(ApiError::bad_request("email is required")),
    };

    let password_hash = match optional_string(body, "password")? {
        Some(password) => hash_password(&password)
            .map_err(|_| ApiError::bad_request("Password must have at least 6 characters"))?,
        None if external_auth => PasswordHash {
            hash: String::new(),
            salt: String::new(),
            hash_type: "bcrypt".into(),
        },
        None => return Err(ApiError::bad_request("password is required")),
    };
    let admin = optional_bool(body, "admin")?.unwrap_or(false);

    let key = prepare_create_key(body, api_version)?;

    let profile = UserProfile {
        external_authentication_uid,
        display_name: Some(display_name),
        first_name,
        middle_name,
        last_name,
    };
    let user = state
        .store
        .create_user_with_profile(
            &username,
            email.as_deref(),
            &password_hash.hash_type,
            &password_hash.hash,
            &password_hash.salt,
            admin,
            &profile,
        )
        .await
        .map_err(ApiError::from)?;

    // The account service gives the creating superuser every permission on a
    // new user's actor ACL (the user itself is added by the store).
    state
        .store
        .grant_actor_all(&user.authz_id, &auth.principal.authz_id)
        .await
        .map_err(ApiError::from)?;

    if optional_bool(body, "recovery_authentication_enabled")?.unwrap_or(false) {
        state
            .store
            .set_recovery_authentication_enabled(&username, true)
            .await
            .map_err(ApiError::from)?;
    }

    let base = base_url(&headers, &state);
    let mut response = json!({ "uri": format!("{base}/users/{username}") });

    let (public_key, private_key) = match key {
        CreateKey::None => (None, None),
        CreateKey::Supplied(public_key) => (Some(public_key), None),
        CreateKey::Generated => {
            let (private_key, public_key) =
                generate_rsa_keypair().map_err(|_| ApiError::internal())?;
            (Some(public_key), Some(private_key))
        }
    };
    if let Some(public_key) = public_key.as_deref() {
        state
            .store
            .add_actor_key(&user.id, ActorType::User, "default", public_key)
            .await
            .map_err(ApiError::from)?;
    }

    if api_version == 0 {
        if let Some(private_key) = private_key {
            response["private_key"] = json!(private_key);
        }
    } else if let Some(public_key) = public_key {
        let mut chef_key = json!({
            "uri": format!("{base}/users/{username}/keys/default"),
            "name": "default",
            "public_key": public_key,
            "expiration_date": "infinity"
        });
        if let Some(private_key) = private_key {
            chef_key["private_key"] = json!(private_key);
        }
        response["chef_key"] = chef_key;
    }

    Ok((StatusCode::CREATED, Json(response)))
}

async fn get_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path(username): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = state
        .store
        .find_user(&username)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))?;

    authorize_read(&state, &auth, &user).await?;
    Ok(Json(user_json(&state, &user, api_version).await?))
}

async fn update_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path(username): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let current = state
        .store
        .find_user(&username)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))?;
    require_superuser(&auth)?;

    let body = body
        .as_object()
        .ok_or_else(|| ApiError::bad_request("request body must be a JSON object"))?;
    let new_username = username_from_body(body)?;
    validate_username(&new_username)?;
    let display_name = required_string(body, "display_name")?.to_string();
    let first_name = optional_patch_string(body, "first_name")?;
    let middle_name = optional_patch_string(body, "middle_name")?;
    let last_name = optional_patch_string(body, "last_name")?;

    let external_authentication_uid = match body.get("external_authentication_uid") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(value)) => Some(Some(value.clone()).filter(|value| !value.is_empty())),
        Some(_) => {
            return Err(ApiError::bad_request(
                "external_authentication_uid must be a string",
            ))
        }
    };
    let external_auth = external_authentication_uid
        .clone()
        .flatten()
        .or_else(|| current.external_authentication_uid.clone())
        .is_some();

    let email = match optional_string(body, "email")? {
        Some(email) if !email.is_empty() => {
            validate_email(&email)?;
            Some(email.to_lowercase())
        }
        Some(_) if external_auth => None,
        Some(_) => return Err(ApiError::bad_request("email must be valid")),
        None if external_auth => None,
        None => return Err(ApiError::bad_request("email is required")),
    };

    let password = match optional_string(body, "password")? {
        Some(password) => {
            let hash = hash_password(&password)
                .map_err(|_| ApiError::bad_request("Password must have at least 6 characters"))?;
            Some((hash.hash, hash.salt, hash.hash_type))
        }
        None => None,
    };
    let recovery = optional_bool(body, "recovery_authentication_enabled")?;

    let key = prepare_update_key(body, api_version)?;

    let patch = UserPatch {
        new_username: Some(new_username.clone()),
        email,
        password,
        admin: optional_bool(body, "admin")?,
        recovery_authentication_enabled: recovery,
        external_authentication_uid,
        display_name: Some(Some(display_name)),
        first_name,
        middle_name,
        last_name,
    };
    let user = state
        .store
        .update_user_patch(&username, &patch)
        .await
        .map_err(ApiError::from)?;

    let mut private_key = None;
    match key {
        UpdateKey::None => {}
        UpdateKey::Supplied(public_key) => {
            state
                .store
                .update_or_create_default_key(&user.id, ActorType::User, &public_key)
                .await
                .map_err(ApiError::from)?;
        }
        UpdateKey::Generated => {
            let (generated_private, public_key) =
                generate_rsa_keypair().map_err(|_| ApiError::internal())?;
            state
                .store
                .update_or_create_default_key(&user.id, ActorType::User, &public_key)
                .await
                .map_err(ApiError::from)?;
            private_key = Some(generated_private);
        }
    }

    let base = base_url(&headers, &state);
    let uri = format!("{base}/users/{}", user.username);
    let mut response = json!({ "uri": uri });
    if let Some(private_key) = private_key {
        response["private_key"] = json!(private_key);
    }

    let mut response_headers = HeaderMap::new();
    let renamed = user.username != username;
    if renamed {
        response_headers.insert(
            header::LOCATION,
            HeaderValue::from_str(&uri).map_err(|_| ApiError::internal())?,
        );
    }
    let status = if renamed {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, response_headers, Json(response)))
}

async fn delete_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_superuser(&auth)?;
    state
        .store
        .delete_user(&username)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

/// Key work that needs to happen before or after the user row is written.
enum CreateKey {
    None,
    Supplied(String),
    Generated,
}

enum UpdateKey {
    None,
    Supplied(String),
    Generated,
}

fn prepare_create_key(
    body: &serde_json::Map<String, Value>,
    api_version: u8,
) -> Result<CreateKey, ApiError> {
    let private_key = optional_bool(body, "private_key")?;
    let create_key = optional_bool(body, "create_key")?;
    let public_key = optional_public_key(body)?;

    if api_version > 0 {
        if body.contains_key("private_key") {
            return Err(ApiError::bad_request(
                "private_key is not supported in this API version",
            ));
        }
        if create_key == Some(true) && public_key.is_some() {
            return Err(ApiError::bad_request(
                "public_key and create_key cannot both be specified",
            ));
        }
        return Ok(if create_key == Some(true) {
            CreateKey::Generated
        } else if let Some(public_key) = public_key {
            CreateKey::Supplied(public_key)
        } else {
            CreateKey::None
        });
    }

    // API v0 always provisions a default key. An explicit private_key request
    // discards any public key/certificate in the payload.
    if private_key == Some(true) {
        return Ok(CreateKey::Generated);
    }
    Ok(match public_key {
        Some(public_key) => CreateKey::Supplied(public_key),
        None => CreateKey::Generated,
    })
}

fn prepare_update_key(
    body: &serde_json::Map<String, Value>,
    api_version: u8,
) -> Result<UpdateKey, ApiError> {
    if api_version > 0 {
        for field in ["create_key", "public_key", "private_key"] {
            if body.contains_key(field) {
                return Err(ApiError::bad_request(format!(
                    "{field} is not supported in this API version"
                )));
            }
        }
        return Ok(UpdateKey::None);
    }

    let private_key = optional_bool(body, "private_key")?;
    let create_key = optional_bool(body, "create_key")?;
    if private_key == Some(true) || create_key == Some(true) {
        return Ok(UpdateKey::Generated);
    }
    Ok(match optional_public_key(body)? {
        Some(public_key) => UpdateKey::Supplied(public_key),
        None => UpdateKey::None,
    })
}

async fn authorize_read(state: &AppState, auth: &AuthContext, user: &User) -> Result<(), ApiError> {
    if is_superuser(auth) || auth.principal.actor_id == user.id {
        return Ok(());
    }
    if state
        .store
        .users_share_org(&auth.principal.actor_id, &user.id)
        .await
        .map_err(ApiError::from)?
    {
        return Ok(());
    }
    Err(ApiError::forbidden())
}

fn username_from_body(body: &serde_json::Map<String, Value>) -> Result<String, ApiError> {
    let username = body
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| body.get("username").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("username is required"))?;
    Ok(username.to_string())
}

fn validate_username(username: &str) -> Result<(), ApiError> {
    let valid = !username.is_empty()
        && username.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || "._-".contains(character)
        });
    if valid {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "Malformed user name. Must only contain a-z, 0-9, _, or -",
        ))
    }
}

fn validate_email(email: &str) -> Result<(), ApiError> {
    if email.chars().any(char::is_whitespace) || email.len() > 254 {
        return Err(ApiError::bad_request("email must be valid"));
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(ApiError::bad_request("email must be valid"));
    };
    if local.is_empty() || local.len() > 64 || domain.is_empty() {
        return Err(ApiError::bad_request("email must be valid"));
    }
    let local_valid = local.chars().all(|character| {
        character.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~.-".contains(character)
    });
    if !local_valid || local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return Err(ApiError::bad_request("email must be valid"));
    }
    let domain_valid = domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        });
    if domain_valid {
        Ok(())
    } else {
        Err(ApiError::bad_request("email must be valid"))
    }
}

fn required_string<'a>(
    body: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str, ApiError> {
    match body.get(field) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(ApiError::bad_request(format!("{field} must be a string"))),
        None => Err(ApiError::bad_request(format!("{field} is required"))),
    }
}

fn optional_string(
    body: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<String>, ApiError> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ApiError::bad_request(format!("{field} must be a string"))),
    }
}

/// Distinguish an absent update field from an explicit JSON null. Absent
/// fields are left alone; null clears the stored value.
fn optional_patch_string(
    body: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<Option<String>>, ApiError> {
    match body.get(field) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(value)) => Ok(Some(Some(value.clone()))),
        Some(_) => Err(ApiError::bad_request(format!("{field} must be a string"))),
    }
}

fn optional_bool(
    body: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<bool>, ApiError> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ApiError::bad_request(format!("{field} must be a boolean"))),
    }
}

fn optional_public_key(body: &serde_json::Map<String, Value>) -> Result<Option<String>, ApiError> {
    match body.get("public_key") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(ApiError::bad_request(
                    "public_key must be a non-empty string",
                ));
            }
            parse_public_key_pem(trimmed)
                .map_err(|_| ApiError::bad_request("Public Key must be a valid key."))?;
            // Preserve the caller's exact PEM text (including the trailing
            // newline); pedant compares it byte-for-byte on a later GET.
            Ok(Some(value.clone()))
        }
        Some(_) => Err(ApiError::bad_request(
            "public_key must be a non-empty string",
        )),
    }
}

pub(crate) async fn user_json(
    state: &AppState,
    user: &User,
    api_version: u8,
) -> Result<Value, ApiError> {
    let mut value = json!({
        "username": user.username,
        "email": user.email.clone().unwrap_or_default(),
    });
    if let Some(display_name) = &user.display_name {
        value["display_name"] = json!(display_name);
    }
    if let Some(first_name) = &user.first_name {
        value["first_name"] = json!(first_name);
    }
    if let Some(middle_name) = &user.middle_name {
        value["middle_name"] = json!(middle_name);
    }
    if let Some(last_name) = &user.last_name {
        value["last_name"] = json!(last_name);
    }
    if let Some(external_uid) = &user.external_authentication_uid {
        value["external_authentication_uid"] = json!(external_uid);
    }
    if user.recovery_authentication_enabled {
        value["recovery_authentication_enabled"] = json!(true);
    }

    if api_version == 0 {
        let keys = state
            .store
            .find_actor_keys(&user.id, ActorType::User)
            .await
            .map_err(ApiError::from)?;
        if let Some(public_key) = keys
            .iter()
            .find(|key| key.key_name == "default")
            .or_else(|| keys.first())
            .map(|key| key.public_key.as_str())
        {
            value["public_key"] = json!(public_key);
        }
    }
    Ok(value)
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
        .await
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
            .await
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

    let user = match state
        .store
        .find_user(&username)
        .await
        .map_err(ApiError::from)?
    {
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
        "first_name": user.first_name.clone().unwrap_or_else(|| user.username.clone()),
        "last_name": user.last_name.clone().unwrap_or_else(|| user.username.clone()),
        "display_name": user.display_name.clone().unwrap_or_else(|| user.username.clone()),
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
        "first_name": user.first_name.clone().unwrap_or_else(|| user.username.clone()),
        "last_name": user.last_name.clone().unwrap_or_else(|| user.username.clone()),
        "display_name": user.display_name.clone().unwrap_or_else(|| user.username.clone())
    })
}
