use crate::{ApiError, AppState};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use microwave_crypto::{generate_rsa_keypair, hash_password};
use microwave_store::{ActorType, User};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users", get(list_users).post(create_user))
        .route(
            "/users/{username}",
            get(get_user).put(update_user).delete(delete_user),
        )
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

    let base = base_url(&headers, &state);
    let mut response = json!({ "uri": format!("{base}/users/{username}") });

    if let Some(public_key) = body.get("public_key").and_then(Value::as_str) {
        state
            .store
            .add_actor_key(&user.id, ActorType::User, "default", public_key)
            .map_err(ApiError::from)?;
    } else if body
        .get("create_key")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let (private_key, public_key) = generate_rsa_keypair().map_err(|_| ApiError::internal())?;
        state
            .store
            .add_actor_key(&user.id, ActorType::User, "default", &public_key)
            .map_err(ApiError::from)?;
        response["chef_key"] = json!({
            "uri": format!("{base}/users/{username}/keys/default"),
            "name": "default",
            "private_key": private_key,
            "public_key": public_key,
            "expiration_date": "infinity"
        });
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
