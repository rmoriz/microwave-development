use crate::error::OAuthError;
use crate::state::OAuthState;
use axum::extract::Request;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use chrono::{DateTime, Utc};
use microwave_store::{OAuthAccessToken, User};
use serde_json::Value;
use std::collections::HashMap;

pub const SESSION_COOKIE: &str = "_oc_id_session";
pub const ACCESS_TOKEN_TTL_SECONDS: i64 = 7_200;
pub const AUTHORIZATION_CODE_TTL_SECONDS: i64 = 600;

/// Parsed form or JSON request body.
pub struct Params {
    form: HashMap<String, String>,
    json: Option<Value>,
}

impl Params {
    pub fn is_json(&self) -> bool {
        self.json.is_some()
    }

    pub fn get(&self, key: &str) -> Option<String> {
        if let Some(value) = self.form.get(key).filter(|value| !value.is_empty()) {
            return Some(value.clone());
        }
        self.json
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(value_to_string)
    }

    pub fn get_path(&self, path: &[&str]) -> Option<String> {
        let mut form_key = path.first()?.to_string();
        for part in &path[1..] {
            form_key.push('[');
            form_key.push_str(part);
            form_key.push(']');
        }
        if let Some(value) = self.form.get(&form_key).filter(|value| !value.is_empty()) {
            return Some(value.clone());
        }
        let mut current = self.json.as_ref()?;
        for key in path {
            current = current.get(*key)?;
        }
        value_to_string(current)
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(boolean) => Some(boolean.to_string()),
        _ => None,
    }
}

/// Parse a form-encoded or JSON body. Empty bodies yield empty params.
pub async fn parse_params(request: Request) -> Result<Params, OAuthError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|_| OAuthError::invalid_request("request body is too large"))?;
    if bytes.is_empty() {
        return Ok(Params {
            form: HashMap::new(),
            json: None,
        });
    }

    if content_type.contains("application/json") {
        let json: Value = serde_json::from_slice(&bytes)
            .map_err(|_| OAuthError::invalid_request("invalid JSON body"))?;
        Ok(Params {
            form: HashMap::new(),
            json: Some(json),
        })
    } else {
        let form = serde_urlencoded::from_bytes::<HashMap<String, String>>(&bytes)
            .map_err(|_| OAuthError::invalid_request("invalid form body"))?;
        Ok(Params { form, json: None })
    }
}

pub fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("application/json"))
}

pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|pair| {
                let (name, value) = pair.trim().split_once('=')?;
                (name == SESSION_COOKIE).then(|| value.to_string())
            })
        })
}

pub fn current_user(state: &OAuthState, headers: &HeaderMap) -> Result<Option<User>, OAuthError> {
    if let Some(token) = session_token(headers) {
        if let Some(username) = state.store.find_id_session(&token)? {
            return state.store.find_user(&username).map_err(OAuthError::from);
        }
    }
    if let Some(token) = bearer_token(headers) {
        return bearer_user(state, &token);
    }
    Ok(None)
}

pub fn bearer_user(state: &OAuthState, token: &str) -> Result<Option<User>, OAuthError> {
    let Some(access_token) = state.store.find_oauth_access_token(token)? else {
        return Ok(None);
    };
    if !token_is_valid(&access_token) {
        return Ok(None);
    }
    let Some(resource_owner_id) = access_token.resource_owner_id else {
        return Ok(None);
    };
    state
        .store
        .find_user_by_id(&resource_owner_id)
        .map_err(OAuthError::from)
}

pub fn require_user(state: &OAuthState, headers: &HeaderMap) -> Result<User, OAuthError> {
    current_user(state, headers)?
        .ok_or_else(|| OAuthError::unauthorized("authentication is required"))
}

pub fn require_admin(state: &OAuthState, headers: &HeaderMap) -> Result<User, OAuthError> {
    let user = require_user(state, headers)?;
    if user.admin || user.username == state.config.auth.superuser_name {
        Ok(user)
    } else {
        Err(OAuthError::forbidden("administrator access is required"))
    }
}

pub fn token_is_valid(token: &OAuthAccessToken) -> bool {
    if token.revoked_at.is_some() {
        return false;
    }
    let Some(expires_in) = token.expires_in else {
        return true;
    };
    let Ok(created_at) = DateTime::parse_from_rfc3339(&token.created_at) else {
        return false;
    };
    let expires_at = created_at.with_timezone(&Utc) + chrono::Duration::seconds(expires_in);
    expires_at > Utc::now()
}

pub fn timestamp_is_fresh(created_at: &str, expires_in: i64) -> bool {
    let Ok(created_at) = DateTime::parse_from_rfc3339(created_at) else {
        return false;
    };
    let expires_at = created_at.with_timezone(&Utc) + chrono::Duration::seconds(expires_in);
    expires_at > Utc::now()
}

pub fn cookie_header(token: &str) -> String {
    format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax")
}

pub fn delete_cookie_header() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

pub fn redirect(location: &str) -> Response {
    match header::HeaderValue::from_str(location) {
        Ok(value) => {
            let mut response = StatusCode::FOUND.into_response();
            response.headers_mut().insert(header::LOCATION, value);
            response
        }
        Err(_) => OAuthError::invalid_request("invalid redirect location").into_response(),
    }
}

/// Percent-encode a value for use in a query string.
pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

pub fn append_query(location: &str, query: &[(&str, &str)]) -> String {
    let separator = if location.contains('?') { '&' } else { '?' };
    let query = query
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{location}{separator}{query}")
}

pub fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

pub fn page(title: &str, body: &str) -> Html<String> {
    let csrf = uuid::Uuid::new_v4().simple().to_string();
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"csrf-token\" content=\"{csrf}\">\
         <title>{title}</title></head><body>{body}</body></html>",
        title = escape_html(title)
    ))
}

pub fn user_json(state: &OAuthState, user: &User) -> Value {
    let public_key = state
        .store
        .find_actor_keys(&user.id, microwave_store::ActorType::User)
        .ok()
        .and_then(|keys| {
            keys.iter()
                .find(|key| key.key_name == "default")
                .or_else(|| keys.first())
                .map(|key| key.public_key.clone())
        });
    serde_json::json!({
        "id": user.username,
        "username": user.username,
        "first_name": Value::Null,
        "middle_name": Value::Null,
        "last_name": Value::Null,
        "display_name": user.username,
        "email": user.email,
        "public_key": public_key,
        "admin": user.admin,
        "recovery_authentication_enabled": user.recovery_authentication_enabled
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn escapes_query_and_html_values() {
        assert_eq!(
            percent_encode("https://example.test/a b?x=1&y=2"),
            "https%3A%2F%2Fexample.test%2Fa%20b%3Fx%3D1%26y%3D2"
        );
        assert_eq!(
            escape_html("<script>\"x\" & 'y'</script>"),
            "&lt;script&gt;&quot;x&quot; &amp; &#39;y&#39;&lt;/script&gt;"
        );
        assert_eq!(
            append_query("https://example.test/cb?x=1", &[("code", "a b")]),
            "https://example.test/cb?x=1&code=a%20b"
        );
    }

    #[test]
    fn reads_nested_json_and_form_values() {
        let params = Params {
            form: HashMap::new(),
            json: Some(json!({ "user": { "email": "alice@example.com" } })),
        };
        assert_eq!(
            params.get_path(&["user", "email"]).as_deref(),
            Some("alice@example.com")
        );

        let mut form = HashMap::new();
        form.insert("user[email]".to_string(), "bob@example.com".to_string());
        let params = Params { form, json: None };
        assert_eq!(
            params.get_path(&["user", "email"]).as_deref(),
            Some("bob@example.com")
        );
    }
}
