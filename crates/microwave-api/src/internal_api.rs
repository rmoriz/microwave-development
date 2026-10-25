//! Internal and compatibility endpoints that are not part of the main
//! converge object surface:
//!
//! * `GET /organizations/:org/:type/:name/_identifiers`
//! * `ANY /organizations/:org/validate/*`
//! * `ANY /organizations/:org/controls`

use crate::helpers::organization;
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::ActorType;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/{object_type}/{object_name}/_identifiers",
            get(get_identifiers),
        )
        // The wildcard is the path the request was signed for. Upstream's
        // `chef_wm_validate` verifies the signature against that suffix, not
        // against the `/validate/...` request path itself.
        .route(
            "/organizations/{organization}/validate/{*signed_path}",
            get(validate).post(validate).put(validate).delete(validate),
        )
        // Every method is answered before resource-specific authorization
        // runs, so the analytics deprecation notice is the same for GET, POST,
        // and anything else.
        .route(
            "/organizations/{organization}/controls",
            axum::routing::any(controls),
        )
}

/// Upstream only implements identifiers for node objects. Other types report
/// `Unsupported object type`, and the response never carries ACL checks: an
/// authenticated member of the organization may read an object's identifiers.
async fn get_identifiers(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, object_type, object_name)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    if object_type != "nodes" {
        return Err(ApiError::not_found(format!(
            "Unsupported object type: '{object_type}'."
        )));
    }

    let node = state
        .store
        .find_node(&org.id, &object_name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("No such 'nodes': '{object_name}'.")))?;

    Ok(Json(json!({
        "id": node.id,
        "authz_id": node.authz_id,
        "org_id": org.id,
    })))
}

/// Report the verified requestor. Signature verification (including the
/// signed-path override in the authentication middleware) and the organization
/// membership check have already happened by the time this runs.
async fn validate(Extension(auth): Extension<AuthContext>) -> Json<Value> {
    let requestor_type = match auth.principal.actor_type {
        ActorType::User => "user",
        ActorType::Client => "client",
    };
    Json(json!({
        "requestor_name": auth.principal.name,
        "requestor_type": requestor_type,
    }))
}

/// Contacts/analytics uploads were removed from Chef Server. The upstream
/// resource halts in `service_available/2`, before authentication or resource
/// authorization, with this exact message.
async fn controls() -> ApiError {
    ApiError::gone("Resource gone, analytics is no longer supported by Chef Server")
}
