use crate::helpers::{base_url, require_permission};
use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_authz::create_organization;
use microwave_crypto::generate_rsa_keypair;
use microwave_store::{ActorType, Client, Permission};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/organizations", get(list_orgs).post(create_org))
        .route(
            "/organizations/{organization}",
            get(get_org).put(update_org).delete(delete_org),
        )
}

async fn list_orgs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let base = base_url(&headers, &state);
    let mut orgs = Map::new();
    for org in state.store.list_orgs().await.map_err(ApiError::from)? {
        orgs.insert(
            org.name.clone(),
            json!(format!("{base}/organizations/{}", org.name)),
        );
    }
    Ok(Json(Value::Object(orgs)))
}

async fn create_org(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(api_version): Extension<ApiVersion>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let name = required_string(&body, "name")?;
    if !valid_org_name(&name) {
        return Err(ApiError::bad_request("Field 'name' is invalid"));
    }
    let full_name = required_string(&body, "full_name")?;
    if !valid_full_name(&full_name) {
        return Err(ApiError::bad_request("Field 'full_name' is invalid"));
    }

    // Generate the validator key before creating any rows so a key generation
    // failure cannot leave a half-created organization behind.
    let (private_key, public_key) = generate_rsa_keypair().map_err(|_| ApiError::internal())?;

    let org = create_organization(
        state.store.as_ref(),
        &name,
        &full_name,
        &auth.principal.actor_id,
        &auth.principal.authz_id,
    )
    .await
    .map_err(ApiError::from)?;

    let client =
        match provision_validator(&state, &org, &auth.principal.authz_id, &public_key).await {
            Ok(client) => client,
            Err(error) => {
                let _ = state.store.delete_org(&org.id).await;
                return Err(error);
            }
        };

    for environment in state
        .store
        .list_environments(&org.id)
        .await
        .map_err(ApiError::from)?
    {
        crate::search::index::index_object(
            &state,
            &org,
            crate::search::IndexKind::Environment,
            &environment,
        )
        .await;
    }
    crate::search::index::index_client(&state, &org, &client).await;

    let base = base_url(&headers, &state);
    let mut response = json!({
        "uri": format!("{base}/organizations/{}", org.name),
        "name": org.name,
        "guid": org.id,
        "clientname": client.name,
        "private_key": private_key,
    });
    if api_version.0 >= 1 {
        response["chef_key"] = json!({
            "uri": format!(
                "{base}/organizations/{}/clients/{}/keys/default",
                org.name, client.name
            ),
            "name": "default",
            "private_key": private_key,
            "public_key": public_key,
            "expiration_date": "infinity"
        });
    }

    Ok((StatusCode::CREATED, Json(response)))
}

async fn provision_validator(
    state: &AppState,
    org: &microwave_store::Org,
    creator_authz_id: &str,
    public_key: &str,
) -> Result<Client, ApiError> {
    let name = format!("{}-validator", org.name);
    let client = state
        .store
        .create_client_with_acl(&org.id, &name, true, false, Some(creator_authz_id))
        .await
        .map_err(ApiError::from)?;
    state
        .store
        .add_actor_key(&client.id, ActorType::Client, "default", public_key)
        .await
        .map_err(ApiError::from)?;
    if let Some(group) = state
        .store
        .find_group(&org.id, "clients")
        .await
        .map_err(ApiError::from)?
    {
        state
            .store
            .add_group_member(&group.authz_id, &client.authz_id)
            .await
            .map_err(ApiError::from)?;
    }
    Ok(client)
}

async fn get_org(
    State(state): State<AppState>,
    Path(organization): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let org = state
        .store
        .find_org(&organization)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("organization {organization}")))?;
    Ok(Json(json!({
        "name": org.name,
        "full_name": org.full_name,
        "guid": org.id
    })))
}

async fn update_org(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(organization): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let org = state
        .store
        .find_org(&organization)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("organization {organization}")))?;
    require_permission(&state, &auth, &org.authz_id, Permission::Update).await?;

    if body.get("private_key").is_some() {
        return Err(ApiError::bad_request("Field 'private_key' is invalid"));
    }
    let name = required_string(&body, "name")?;
    if name != organization {
        return Err(ApiError::bad_request("Field 'name' invalid"));
    }
    if !valid_org_name(&name) {
        return Err(ApiError::bad_request("Field 'name' is invalid"));
    }
    let full_name = required_string(&body, "full_name")?;
    if !valid_full_name(&full_name) {
        return Err(ApiError::bad_request("Field 'full_name' is invalid"));
    }

    state
        .store
        .update_org(&organization, &full_name)
        .await
        .map_err(ApiError::from)?;

    // Upstream echoes the posted representation, including fields such as
    // org_type which are not persisted.
    Ok(Json(body))
}

async fn delete_org(
    State(state): State<AppState>,
    Path(organization): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let org = state
        .store
        .find_org(&organization)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("organization {organization}")))?;
    let org_id = org.id.clone();
    state
        .store
        .delete_org(&org_id)
        .await
        .map_err(ApiError::from)?;
    crate::search::index::delete_organization(&state, &org_id).await;
    Ok(Json(json!({})))
}

fn required_string(body: &Value, field: &str) -> Result<String, ApiError> {
    body.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| ApiError::bad_request(format!("Field '{field}' is missing")))
}

fn valid_org_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && name.len() <= 255
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '-'
        })
}

fn valid_full_name(full_name: &str) -> bool {
    let mut characters = full_name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    !first.is_whitespace() && full_name.chars().count() <= 1023
}
