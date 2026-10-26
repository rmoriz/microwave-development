mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use serde_json::Value;
use tower::ServiceExt;

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

fn add_to_users_group(
    state: &microwave_api::AppState,
    org: &microwave_store::Org,
    user: &microwave_store::User,
) {
    let group = state
        .sqlite()
        .find_group(&org.id, "users")
        .unwrap()
        .unwrap();
    state
        .sqlite()
        .add_group_member(&group.authz_id, &user.authz_id)
        .unwrap();
}

#[tokio::test]
async fn organization_routes_enforce_authorization() {
    let state = support::state_with_pivotal();
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    let bob = support::add_user(&state, "bob", "bob@example.com", false);
    let org = support::add_org(&state, "acme").await;
    support::associate(&state, &org, &alice);
    add_to_users_group(&state, &org, &alice);

    let server_admins = state.sqlite().ensure_global_group("server-admins").unwrap();
    state
        .sqlite()
        .add_group_member(&server_admins.authz_id, &bob.authz_id)
        .unwrap();
    let app = support::app(state);

    let (status, _) = send(&app, "GET", "/organizations", b"", "alice").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "normal users must not list orgs"
    );

    let (status, _) = send(&app, "GET", "/organizations", b"", "bob").await;
    assert_eq!(status, StatusCode::OK, "server-admins may list orgs");

    let (status, _) = send(&app, "GET", "/organizations/acme", b"", "alice").await;
    assert_eq!(status, StatusCode::OK, "org members may read their org");

    let (status, _) = send(&app, "DELETE", "/organizations/acme", b"", "alice").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "read-only members cannot delete the org"
    );

    let body = br#"{"name":"neworg","full_name":"New Org"}"#;
    let (status, _) = send(&app, "POST", "/organizations", body, "alice").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "normal users must not create orgs"
    );
}
