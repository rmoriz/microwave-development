mod support;

use axum::http::StatusCode;
use microwave_api::AppState;
use microwave_crypto::SignVersion;
use microwave_store::ActorType;
use serde_json::Value;
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

async fn create_org(app: &axum::Router) {
    let (status, _) = request(
        app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

fn add_user(state: &AppState, name: &str) {
    let user = state
        .sqlite()
        .create_user(
            name,
            &format!("{name}@example.com"),
            "bcrypt",
            "h",
            "s",
            false,
        )
        .unwrap();
    state
        .sqlite()
        .add_actor_key(&user.id, ActorType::User, "default", support::PUBLIC_KEY)
        .unwrap();
}

async fn invite(app: &axum::Router, user: &str) -> String {
    let body = format!(r#"{{"user":"{user}"}}"#);
    let (status, invitation) = request(
        app,
        "POST",
        "/organizations/acme/association_requests",
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(invitation["organization"]["name"], "acme");
    assert_eq!(invitation["organization_user"]["username"], "pivotal");
    assert_eq!(invitation["user"]["email"], format!("{user}@example.com"));
    assert_eq!(invitation["user"]["first_name"], user);
    invitation["uri"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn adds_lists_and_removes_org_members() {
    let state = support::state_with_pivotal();
    add_user(&state, "alice");
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();

    let (status, created) = request(
        &app,
        "POST",
        "/organizations/acme/users",
        br#"{"username":"alice"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/users/alice"
    );

    let (status, users) = request(&app, "GET", "/organizations/acme/users", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        users,
        serde_json::json!([
            { "user": { "username": "alice" } },
            { "user": { "username": "pivotal" } }
        ])
    );

    let (status, user) = request(
        &app,
        "GET",
        "/organizations/acme/users/alice",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(user["username"], "alice");

    let (status, orgs) = request(&app, "GET", "/users/alice/organizations", b"", "alice").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        orgs,
        serde_json::json!([{
            "organization": {
                "name": "acme",
                "full_name": "Acme Corp",
                "guid": org.id
            }
        }])
    );

    let (status, deleted) = request(
        &app,
        "DELETE",
        "/organizations/acme/users/alice",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["username"], "alice");
    assert_eq!(deleted["email"], "alice@example.com");

    let (status, orgs) = request(&app, "GET", "/users/alice/organizations", b"", "alice").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(orgs, serde_json::json!([]));
}

#[tokio::test]
async fn creates_and_accepts_an_invitation() {
    let state = support::state_with_pivotal();
    add_user(&state, "bob");
    let org_id = {
        let app = support::app(state.clone());
        create_org(&app).await;
        state.sqlite().find_org("acme").unwrap().unwrap().id
    };
    let app = support::app(state.clone());

    let id = invite(&app, "bob").await;

    let (status, invitations) = request(
        &app,
        "GET",
        "/organizations/acme/association_requests",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        invitations,
        serde_json::json!([{ "id": id, "username": "bob" }])
    );

    let (status, invitations) =
        request(&app, "GET", "/users/bob/association_requests", b"", "bob").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        invitations,
        serde_json::json!([{ "id": id, "orgname": "acme" }])
    );

    let (status, count) = request(
        &app,
        "GET",
        "/users/bob/association_requests/count",
        b"",
        "bob",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count, serde_json::json!({ "value": 1 }));

    let accept = r#"{"response":"accept"}"#;
    let (status, accepted) = request(
        &app,
        "PUT",
        &format!("/users/bob/association_requests/{id}"),
        accept.as_bytes(),
        "bob",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(accepted["id"], id);
    assert_eq!(accepted["orgname"], "acme");
    assert_eq!(accepted["username"], "bob");
    assert_eq!(
        accepted["organization"],
        serde_json::json!({ "deprecated": true, "name": "acme" })
    );
    assert_eq!(accepted["user"], "deprecated");

    let (status, count) = request(
        &app,
        "GET",
        "/users/bob/association_requests/count",
        b"",
        "bob",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count, serde_json::json!({ "value": 0 }));

    let (status, users) = request(&app, "GET", "/organizations/acme/users", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert!(users
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["user"]["username"] == "bob"));

    let bob = state.sqlite().find_user("bob").unwrap().unwrap();
    let users_group = state
        .sqlite()
        .find_group(&org_id, "users")
        .unwrap()
        .unwrap();
    assert!(state
        .sqlite()
        .user_in_group(&users_group.authz_id, &bob.authz_id)
        .unwrap());
}

#[tokio::test]
async fn rejects_an_invitation() {
    let state = support::state_with_pivotal();
    add_user(&state, "carol");
    let org_id = {
        let app = support::app(state.clone());
        create_org(&app).await;
        state.sqlite().find_org("acme").unwrap().unwrap().id
    };
    let app = support::app(state.clone());

    let id = invite(&app, "carol").await;
    let (status, rejected) = request(
        &app,
        "PUT",
        &format!("/users/carol/association_requests/{id}"),
        br#"{"response":"reject"}"#,
        "carol",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rejected["id"], id);

    let (status, count) = request(
        &app,
        "GET",
        "/users/carol/association_requests/count",
        b"",
        "carol",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count, serde_json::json!({ "value": 0 }));

    let (status, users) = request(&app, "GET", "/organizations/acme/users", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert!(users
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["user"]["username"] != "carol"));

    let carol = state.sqlite().find_user("carol").unwrap().unwrap();
    assert!(!state.sqlite().is_user_in_org(&org_id, &carol.id).unwrap());

    let (status, _) = request(
        &app,
        "PUT",
        &format!("/users/carol/association_requests/{id}"),
        br#"{"response":"accept"}"#,
        "carol",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn refuses_to_remove_an_org_admin() {
    let state = support::state_with_pivotal();
    add_user(&state, "dave");
    let app = support::app(state.clone());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/users",
        br#"{"username":"dave"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let dave = state.sqlite().find_user("dave").unwrap().unwrap();
    let admins = state
        .sqlite()
        .find_group(&org.id, "admins")
        .unwrap()
        .unwrap();
    state
        .sqlite()
        .add_group_member(&admins.authz_id, &dave.authz_id)
        .unwrap();

    let (status, error) = request(
        &app,
        "DELETE",
        "/organizations/acme/users/dave",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        error["error"][0],
        "Please remove dave from this organization's admins group before removing him or her from the organization."
    );

    state
        .sqlite()
        .remove_group_member(&admins.authz_id, &dave.authz_id)
        .unwrap();
    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/users/dave",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn invitation_responses_must_be_accept_or_reject() {
    let state = support::state_with_pivotal();
    add_user(&state, "erin");
    let app = support::app(state);
    create_org(&app).await;
    let id = invite(&app, "erin").await;

    let (status, error) = request(
        &app,
        "PUT",
        &format!("/users/erin/association_requests/{id}"),
        br#"{"response":"blither"}"#,
        "erin",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error["error"][0],
        "Param response must be either 'accept' or 'reject'"
    );
}
