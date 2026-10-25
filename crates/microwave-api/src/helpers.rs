use crate::{ApiError, AppState, AuthContext};
use axum::http::HeaderMap;
use microwave_store::{Org, Permission};
use serde_json::Value;

pub fn organization(auth: &AuthContext) -> Result<&Org, ApiError> {
    auth.org
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("organization is required"))
}

pub fn base_url(headers: &HeaderMap, state: &AppState) -> String {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or(state.config.server.bind.as_str());
    // A reverse proxy that terminates TLS advertises the original scheme.
    // Without that header, follow the listener's own configuration so URLs
    // returned by the native TLS listener are https.
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| {
            if state.config.server.tls.is_enabled() {
                "https"
            } else {
                "http"
            }
        });
    format!("{proto}://{host}")
}

pub async fn container_authz(
    state: &AppState,
    org: &Org,
    container_name: &str,
) -> Result<String, ApiError> {
    Ok(state
        .store
        .find_container(&org.id, container_name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("container {container_name}")))?
        .authz_id)
}

pub async fn require_permission(
    state: &AppState,
    auth: &AuthContext,
    target_authz_id: &str,
    permission: Permission,
) -> Result<(), ApiError> {
    if is_superuser(auth) {
        return Ok(());
    }
    if state
        .store
        .check_permission(&auth.principal.authz_id, target_authz_id, permission)
        .await
        .map_err(ApiError::from)?
    {
        Ok(())
    } else {
        Err(ApiError::forbidden_with(format!(
            "missing {} permission",
            permission.as_str()
        )))
    }
}

pub fn is_superuser(auth: &AuthContext) -> bool {
    auth.principal.name == "pivotal"
}

pub fn require_superuser(auth: &AuthContext) -> Result<(), ApiError> {
    if is_superuser(auth) {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

/// Validate a `public_key` member. A missing or JSON null value means "no key
/// was supplied"; every other value must be a valid PEM public key.
pub fn parse_public_key(body: &Value) -> Result<Option<String>, ApiError> {
    match body.get("public_key") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            if value.trim().is_empty() {
                return Err(ApiError::bad_request("Field 'public_key' is invalid"));
            }
            microwave_crypto::parse_public_key_pem(value.trim())
                .map_err(|_| ApiError::bad_request("Field 'public_key' is invalid"))?;
            Ok(Some(value.clone()))
        }
        Some(_) => Err(ApiError::bad_request("Field 'public_key' is invalid")),
    }
}

#[cfg(test)]
mod tests {
    use super::base_url;
    use crate::AppState;
    use axum::http::{HeaderMap, HeaderValue};
    use microwave_config::Config;
    use microwave_objectstore::{LocalObjectStore, ObjectStore};
    use microwave_store::SqliteStore;
    use std::sync::Arc;

    fn state(tls_enabled: bool) -> AppState {
        let mut config = Config::default();
        if tls_enabled {
            config.server.tls.cert_path = Some("/tmp/tls.crt".into());
            config.server.tls.key_path = Some("/tmp/tls.key".into());
        }
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
            std::env::temp_dir().join("mw-helpers-test"),
            "secret",
            "http://0.0.0.0:8000",
        ));
        AppState::from_sqlite(store, Arc::new(config), object_store).unwrap()
    }

    fn headers(host: Option<&str>, proto: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(host) = host {
            headers.insert("host", HeaderValue::from_str(host).unwrap());
        }
        if let Some(proto) = proto {
            headers.insert("x-forwarded-proto", HeaderValue::from_str(proto).unwrap());
        }
        headers
    }

    #[test]
    fn base_url_uses_https_for_a_native_tls_listener() {
        assert_eq!(
            base_url(&headers(Some("chef.example.com"), None), &state(true)),
            "https://chef.example.com"
        );
    }

    #[test]
    fn base_url_falls_back_to_the_configured_bind() {
        assert_eq!(
            base_url(&headers(None, None), &state(true)),
            "https://0.0.0.0:8000"
        );
        assert_eq!(
            base_url(&headers(None, None), &state(false)),
            "http://0.0.0.0:8000"
        );
    }

    #[test]
    fn forwarded_proto_still_wins_behind_a_proxy() {
        assert_eq!(
            base_url(
                &headers(Some("chef.example.com"), Some("http")),
                &state(true)
            ),
            "http://chef.example.com"
        );
        assert_eq!(
            base_url(
                &headers(Some("chef.example.com"), Some("https")),
                &state(false)
            ),
            "https://chef.example.com"
        );
    }

    #[test]
    fn forwarded_host_still_wins() {
        let mut headers = headers(Some("internal:8000"), None);
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("chef.example.com"),
        );
        assert_eq!(base_url(&headers, &state(true)), "https://chef.example.com");
    }
}
