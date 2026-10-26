mod support;

use axum::http::StatusCode;
use microwave_api::AppState;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn request(
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
    let value = support::response_json(response).await;
    (status, value)
}

async fn create_org(state: &AppState) -> microwave_store::Org {
    let creator = state.sqlite().find_user("pivotal").unwrap().unwrap();
    microwave_authz::create_organization(
        state.sqlite(),
        "acme",
        "Acme Corp",
        &creator.id,
        &creator.authz_id,
    )
    .await
    .unwrap()
}

async fn create_invitation(app: &axum::Router, username: &str) -> (Value, String) {
    let body = json!({ "user": username }).to_string();
    let (status, response) = request(
        app,
        "POST",
        "/organizations/acme/association_requests",
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = response["uri"]
        .as_str()
        .expect("invitation uri")
        .rsplit('/')
        .next()
        .expect("invitation id")
        .to_string();
    (response, id)
}

#[tokio::test]
async fn org_user_list_uses_the_upstream_user_wrapper() {
    let state = support::state_with_pivotal();
    let org = create_org(&state).await;
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    support::associate(&state, &org, &alice);
    let app = support::app(state);

    let (status, body) = request(&app, "GET", "/organizations/acme/users", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!([
            { "user": { "username": "alice" } },
            { "user": { "username": "pivotal" } }
        ])
    );
}

#[tokio::test]
async fn user_org_list_uses_the_upstream_organization_wrapper() {
    let state = support::state_with_pivotal();
    let org = create_org(&state).await;
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    support::associate(&state, &org, &alice);
    let app = support::app(state);

    let (status, body) = request(&app, "GET", "/users/alice/organizations", b"", "alice").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!([{
            "organization": {
                "name": "acme",
                "full_name": "Acme Corp",
                "guid": org.id,
            }
        }])
    );
}

#[tokio::test]
async fn invitation_lists_use_the_upstream_field_names() {
    let state = support::state_with_pivotal();
    create_org(&state).await;
    support::add_user(&state, "bob", "bob@example.com", false);
    let app = support::app(state);

    let (created, id) = create_invitation(&app, "bob").await;
    assert_eq!(
        created,
        json!({
            "uri": format!("http://0.0.0.0:8000/organizations/acme/association_requests/{id}"),
            "organization_user": { "username": "pivotal" },
            "organization": { "name": "acme" },
            "user": {
                "email": "bob@example.com",
                "first_name": "bob",
            },
        })
    );

    let (status, org_invitations) = request(
        &app,
        "GET",
        "/organizations/acme/association_requests",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(org_invitations, json!([{ "id": id, "username": "bob" }]));

    let (status, user_invitations) =
        request(&app, "GET", "/users/bob/association_requests", b"", "bob").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(user_invitations, json!([{ "id": id, "orgname": "acme" }]));
}

#[tokio::test]
async fn accepting_an_invitation_returns_the_deprecated_invitation_document() {
    let state = support::state_with_pivotal();
    create_org(&state).await;
    support::add_user(&state, "bob", "bob@example.com", false);
    let app = support::app(state);

    let (_, id) = create_invitation(&app, "bob").await;
    let (status, accepted) = request(
        &app,
        "PUT",
        &format!("/users/bob/association_requests/{id}"),
        br#"{"response":"accept"}"#,
        "bob",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        accepted,
        json!({
            "id": id,
            "orgname": "acme",
            "username": "bob",
            "organization": {
                "deprecated": true,
                "name": "acme",
            },
            "user": "deprecated",
            "organization_admin_actor_id": "deprecated",
        })
    );
}

#[tokio::test]
async fn deleting_an_association_returns_the_removed_user_document() {
    let state = support::state_with_pivotal();
    let org = create_org(&state).await;
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    support::associate(&state, &org, &alice);
    let app = support::app(state);

    let (status, removed) = request(
        &app,
        "DELETE",
        "/organizations/acme/users/alice",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        removed,
        json!({
            "username": "alice",
            "email": "alice@example.com",
            "display_name": "alice",
            "first_name": "alice",
            "last_name": "alice",
        })
    );

    let (status, users) = request(&app, "GET", "/organizations/acme/users", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(users, json!([{ "user": { "username": "pivotal" } }]));
}

#[tokio::test]
async fn missing_organization_and_missing_member_are_not_found() {
    let state = support::state_with_pivotal();
    create_org(&state).await;
    let app = support::app(state);

    let (status, _) = request(&app, "GET", "/organizations/missing/users", b"", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/users/missing",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
