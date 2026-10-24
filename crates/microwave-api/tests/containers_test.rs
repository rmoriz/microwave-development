mod support;

use axum::http::StatusCode;
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

fn add_user(state: &microwave_api::AppState, name: &str) -> String {
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
    user.authz_id
}

fn associate_user(state: &microwave_api::AppState, username: &str) {
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let user = state.sqlite().find_user(username).unwrap().unwrap();
    state
        .sqlite()
        .associate_user_org(&org.id, &user.id)
        .unwrap();
}

fn add_member(state: &microwave_api::AppState, org_name: &str, group: &str, authz_id: &str) {
    let org = state.sqlite().find_org(org_name).unwrap().unwrap();
    let group = state.sqlite().find_group(&org.id, group).unwrap().unwrap();
    state
        .sqlite()
        .add_group_member(&group.authz_id, authz_id)
        .unwrap();
}

#[tokio::test]
async fn container_crud_and_authorization() {
    let state = support::state_with_pivotal();
    let admin = add_user(&state, "admin");
    let normal = add_user(&state, "normal");
    let _outside = add_user(&state, "outside");
    let app = support::app(state.clone());
    create_org(&app).await;
    associate_user(&state, "admin");
    associate_user(&state, "normal");
    add_member(&state, "acme", "admins", &admin);
    add_member(&state, "acme", "users", &normal);

    let (status, containers) =
        request(&app, "GET", "/organizations/acme/containers", b"", "normal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        containers["clients"],
        "http://0.0.0.0:8000/organizations/acme/containers/clients"
    );
    assert_eq!(containers.as_object().unwrap().len(), 12);

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/containers",
        b"",
        "outside",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, created) = request(
        &app,
        "POST",
        "/organizations/acme/containers",
        br#"{"id":"test-container","containerpath":"/"}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/containers/test-container"
    );
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/containers",
        br#"{"id":"test-container","containerpath":"/"}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/containers",
        br#"{"containerpath":"/"}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/containers",
        br#"{"id":"bad container","containerpath":"/"}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/containers",
        br#"{"id":"by-normal","containerpath":"/"}"#,
        "normal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, container) = request(
        &app,
        "GET",
        "/organizations/acme/containers/test-container",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        container,
        serde_json::json!({
            "containername": "test-container",
            "containerpath": "test-container",
        })
    );
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/containers/test-container",
        b"",
        "normal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/containers/clients",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/containers/test-container",
        b"",
        "normal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/containers/test-container",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/containers/test-container",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
