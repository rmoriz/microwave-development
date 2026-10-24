mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use microwave_store::{ActorType, SqliteStore};
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
        .store
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
        .store
        .add_actor_key(&user.id, ActorType::User, "default", support::PUBLIC_KEY)
        .unwrap();
    user.authz_id
}

fn associate_user(state: &microwave_api::AppState, username: &str) {
    let org = state.store.find_org("acme").unwrap().unwrap();
    let user = state.store.find_user(username).unwrap().unwrap();
    state.store.associate_user_org(&org.id, &user.id).unwrap();
}

fn add_member(state: &microwave_api::AppState, org_name: &str, group: &str, authz_id: &str) {
    let org = state.store.find_org(org_name).unwrap().unwrap();
    let group = state.store.find_group(&org.id, group).unwrap().unwrap();
    state
        .store
        .add_group_member(&group.authz_id, authz_id)
        .unwrap();
}

#[tokio::test]
async fn group_collection_and_named_crud_follow_upstream_shapes() {
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

    let (status, groups) = request(&app, "GET", "/organizations/acme/groups", b"", "admin").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        groups["admins"],
        "http://0.0.0.0:8000/organizations/acme/groups/admins"
    );

    let (status, _) = request(&app, "GET", "/organizations/acme/groups", b"", "normal").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(&app, "GET", "/organizations/acme/groups", b"", "outside").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Unrelated clients cannot read groups.
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"robot","public_key":"__KEY__"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        format!(
            r#"{{"name":"robot","public_key":{:?}}}"#,
            support::PUBLIC_KEY
        )
        .as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = request(&app, "GET", "/organizations/acme/groups", b"", "robot").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, created) = request(
        &app,
        "POST",
        "/organizations/acme/groups",
        br#"{"id":"test-group"}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/groups/test-group"
    );
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/groups",
        br#"{"id":"test-group"}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/groups",
        br#"{}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/groups",
        br#"{"id":"bad group"}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/groups",
        br#"{"id":"normal-group"}"#,
        "normal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, group) = request(
        &app,
        "GET",
        "/organizations/acme/groups/test-group",
        b"",
        "normal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        group,
        serde_json::json!({
            "actors": [],
            "users": [],
            "clients": [],
            "groups": [],
            "orgname": "acme",
            "name": "test-group",
            "groupname": "test-group",
        })
    );

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/groups/test-group",
        br#"{"groupname":"test-group","actors":{"users":["normal"],"clients":["robot"],"groups":["users"]}}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, group) = request(
        &app,
        "GET",
        "/organizations/acme/groups/test-group",
        b"",
        "normal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(group["users"], serde_json::json!(["normal"]));
    assert_eq!(group["clients"], serde_json::json!(["robot"]));
    assert_eq!(group["groups"], serde_json::json!(["users"]));
    assert_eq!(group["actors"], serde_json::json!(["robot", "normal"]),);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/groups/test-group",
        br#"{"groupname":"test-group","actors":{"users":["nobody"]}}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Renaming creates the new name and removes the old one.
    let (status, renamed) = request(
        &app,
        "PUT",
        "/organizations/acme/groups/test-group",
        br#"{"groupname":"renamed-group"}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(renamed["name"], "renamed-group");
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/groups/test-group",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/groups/renamed-group",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/groups/admins",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/groups/renamed-group",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/groups/renamed-group",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn replacement_group_members_are_validated_and_apply_per_bucket() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "h", "s", true)
        .unwrap();
    let org = microwave_authz::create_organization(
        &store,
        "acme",
        "Acme Corp",
        &creator.id,
        &creator.authz_id,
    )
    .unwrap();
    let group = store
        .create_group_for_org_with_acl(&org.id, "test", &creator.authz_id)
        .unwrap();
    let user = store
        .create_user("alice", "alice@example.com", "bcrypt", "h", "s", false)
        .unwrap();

    store
        .replace_group_members(
            &org.id,
            &group.authz_id,
            Some(std::slice::from_ref(&user.username)),
            None,
            None,
        )
        .unwrap();
    let members = store.list_group_members(&org.id, &group.authz_id).unwrap();
    assert_eq!(members.users, vec!["alice"]);
    assert!(members.clients.is_empty());

    let error = store
        .replace_group_members(
            &org.id,
            &group.authz_id,
            Some(&["missing".to_string()]),
            None,
            None,
        )
        .unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::Invalid(_)));

    // Clients calling the groups API send their combined actor list in the
    // `users` bucket as well as the typed `clients` bucket.
    store
        .create_client_with_acl(&org.id, "robot", false, false, Some(&creator.authz_id))
        .unwrap();
    store
        .replace_group_members(
            &org.id,
            &group.authz_id,
            Some(&["robot".to_string()]),
            Some(&["robot".to_string()]),
            None,
        )
        .unwrap();
    let members = store.list_group_members(&org.id, &group.authz_id).unwrap();
    assert!(members.users.is_empty());
    assert_eq!(members.clients, vec!["robot"]);
}
