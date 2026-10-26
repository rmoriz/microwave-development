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

async fn raw_request(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, String) {
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
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn create_org(app: &axum::Router) {
    let (status, body) = raw_request(
        app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn create_node(app: &axum::Router) {
    let (status, _) = request(
        app,
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1","run_list":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

fn add_user(state: &microwave_api::AppState, name: &str) -> microwave_store::User {
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
    user
}

#[tokio::test]
async fn reads_the_default_organization_acl_and_compatibility_route() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/organizations/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert!(acl[permission]["actors"].is_array());
        assert!(acl[permission]["groups"].is_array());
    }
    assert!(acl["create"]["actors"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("pivotal")));
    assert!(acl["create"]["groups"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("admins")));

    let (status, compatibility) = request(
        &app,
        "GET",
        "/organizations/acme/anything/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(compatibility, acl);

    let (status, granular) = request(
        &app,
        "GET",
        "/organizations/acme/organizations/_acl?detail=granular",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(granular["create"]["actors"].is_array());
    assert!(granular["create"]["actors"].as_array().unwrap().is_empty());
    assert!(granular["create"]["users"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("pivotal")));
    assert!(granular["create"]["clients"].as_array().unwrap().is_empty());
    assert!(granular["create"]["groups"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("admins")));
}

#[tokio::test]
async fn organization_acl_put_accepts_a_permission_envelope() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/organizations/_acl/read",
        br#"{"read":{"actors":["pivotal"],"groups":["admins"]}}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/organizations/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acl["read"]["actors"], serde_json::json!(["pivotal"]));
    assert_eq!(acl["read"]["groups"], serde_json::json!(["admins"]));
}

#[tokio::test]
async fn node_acl_grants_read_without_granting_update() {
    let state = support::state_with_pivotal();
    let second = add_user(&state, "second");
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    support::associate(&state, &org, &second);
    // Organization membership uses a per-user USAG. Deleting that USAG
    // detaches the member from the users group and revokes the group's
    // container ACL while preserving the user's own ACLs.
    state.sqlite().delete_group(&org.id, &second.id).unwrap();
    create_node(&app).await;

    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":["second"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({}));

    let (status, _) = request(&app, "GET", "/organizations/acme/nodes/web1", b"", "second").await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1",
        br#"{"name":"web1","run_list":["recipe[apache]"]}"#,
        "second",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Modifying the ACL itself requires grant, which second does not have.
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/update",
        br#"{"actors":["second"],"groups":[]}"#,
        "second",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/web1/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acl["read"]["actors"], serde_json::json!(["second"]));
    assert!(acl["update"]["actors"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("pivotal")));
}

#[tokio::test]
async fn granular_acl_splits_users_and_clients() {
    let state = support::state_with_pivotal();
    add_user(&state, "second");
    let app = support::app(state.clone());
    create_org(&app).await;
    create_node(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    state
        .sqlite()
        .create_client(&org.id, "robot", false, false)
        .unwrap();

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":[],"users":["second"],"clients":["robot"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, granular) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/web1/_acl?detail=granular",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(granular["read"]["users"], serde_json::json!(["second"]));
    assert_eq!(granular["read"]["clients"], serde_json::json!(["robot"]));
    assert_eq!(granular["read"]["groups"], serde_json::json!([]));
    assert!(granular["read"]["actors"].as_array().unwrap().is_empty());

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/web1/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let actors = acl["read"]["actors"].as_array().unwrap();
    assert!(actors.contains(&serde_json::json!("second")));
    assert!(actors.contains(&serde_json::json!("robot")));
}

#[tokio::test]
async fn rejects_invalid_acl_payloads_and_missing_targets() {
    let state = support::state_with_pivotal();
    add_user(&state, "second");
    let app = support::app(state);
    create_org(&app).await;
    create_node(&app).await;

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":["nobody"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":["second"],"users":["second"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":[],"groups":["missing"]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/not-a-permission",
        br#"{"actors":[],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/missing/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/groups/missing/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request(&app, "GET", "/users/missing/_acl", b"", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn user_acls_are_global_and_use_actor_names() {
    let state = support::state_with_pivotal();
    add_user(&state, "second");
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    state
        .sqlite()
        .create_client(&org.id, "robot", false, false)
        .unwrap();

    let (status, acl) = request(&app, "GET", "/users/second/_acl", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert!(
            acl[permission]["actors"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("second")),
            "user is missing from its own {permission} ACE: {acl}"
        );
    }
    assert_eq!(acl["grant"]["groups"], serde_json::json!([]));
    for permission in ["create", "read", "update", "delete"] {
        assert!(
            acl[permission]["groups"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("::server-admins")),
            "server-admins is missing from {permission}: {acl}"
        );
    }

    let (status, _) = request(
        &app,
        "PUT",
        "/users/second/_acl/read",
        br#"{"actors":["pivotal"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, acl) = request(&app, "GET", "/users/second/_acl", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acl["read"]["actors"], serde_json::json!(["pivotal"]));

    let (status, _) = request(
        &app,
        "PUT",
        "/users/second/_acl/read",
        br#"{"actors":["robot"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn add_org_admin(
    state: &microwave_api::AppState,
    org: &microwave_store::Org,
    username: &str,
) -> microwave_store::User {
    let user = support::add_user(state, username, &format!("{username}@example.com"), false);
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    state
        .sqlite()
        .grant_actor_all(&user.authz_id, &pivotal.authz_id)
        .unwrap();
    support::associate(state, org, &user);
    let admins = state
        .sqlite()
        .find_group(&org.id, "admins")
        .unwrap()
        .unwrap();
    state
        .sqlite()
        .add_group_member(&admins.authz_id, &user.authz_id)
        .unwrap();
    user
}

#[tokio::test]
async fn user_acl_has_creator_self_and_global_groups_and_admits_org_admins() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    add_org_admin(&state, &org, "orgadmin");

    let (status, acl) = request(&app, "GET", "/users/orgadmin/_acl", b"", "orgadmin").await;
    assert_eq!(status, StatusCode::OK);
    for permission in ["create", "read", "update", "delete"] {
        let actors = acl[permission]["actors"].as_array().unwrap();
        assert!(actors.contains(&serde_json::json!("pivotal")), "{acl}");
        assert!(actors.contains(&serde_json::json!("orgadmin")), "{acl}");
        assert!(
            acl[permission]["groups"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("::server-admins")),
            "{acl}"
        );
    }
    assert_eq!(acl["grant"]["groups"], serde_json::json!([]));
    assert!(acl["read"]["groups"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("::acme_read_access_group")));

    let (status, _) = request(
        &app,
        "PUT",
        "/users/orgadmin/_acl/read",
        br#"{"actors":["pivotal","orgadmin"],"groups":["::server-admins","::acme_read_access_group"]}"#,
        "orgadmin",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn non_superusers_cannot_remove_the_admins_group_from_a_grant_ace() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    create_node(&app).await;

    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    add_org_admin(&state, &org, "orgadmin");

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/grant",
        br#"{"actors":["pivotal"],"groups":[]}"#,
        "orgadmin",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/grant",
        br#"{"actors":["pivotal"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn new_objects_inherit_the_container_acl_and_creator_grants() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    add_org_admin(&state, &org, "orgadmin");

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1","run_list":[]}"#,
        "orgadmin",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/web1/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let create_actors = acl["create"]["actors"].as_array().unwrap();
    assert!(create_actors.contains(&serde_json::json!("pivotal")));
    assert!(create_actors.contains(&serde_json::json!("orgadmin")));
    assert_eq!(
        acl["create"]["groups"],
        serde_json::json!(["admins", "clients", "users"])
    );
    assert_eq!(
        acl["update"]["groups"],
        serde_json::json!(["admins", "users"])
    );
    assert_eq!(acl["grant"]["groups"], serde_json::json!(["admins"]));
}

#[tokio::test]
async fn data_bag_acl_route_is_not_shadowed_by_the_item_route() {
    let state = support::state_with_pivotal();
    let app = support::app(state);
    create_org(&app).await;

    let (status, body) = raw_request(
        &app,
        "POST",
        "/organizations/acme/data",
        br#"{"name":"secrets"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, acl_body) = raw_request(
        &app,
        "GET",
        "/organizations/acme/data/secrets/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{acl_body}");
    let acl: Value = serde_json::from_str(&acl_body).unwrap();
    assert!(acl["read"]["groups"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("clients")));
}

#[tokio::test]
async fn acl_updates_accept_global_and_cross_organization_groups() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    create_node(&app).await;
    support::add_org(&state, "other").await;

    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":["pivotal"],"groups":["::server-admins","other::admins"]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/web1/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let groups = acl["read"]["groups"].as_array().unwrap();
    assert!(groups.contains(&serde_json::json!("::server-admins")));
    assert!(groups.contains(&serde_json::json!("other::admins")));
}

#[tokio::test]
async fn ambiguous_client_and_user_names_require_split_actor_lists() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let user = support::add_user(&state, "shared", "shared@example.com", false);
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    state
        .sqlite()
        .create_client_with_acl(&org.id, "shared", false, false, Some(&pivotal.authz_id))
        .unwrap();

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/shared/_acl/read",
        br#"{"actors":["pivotal","shared"],"groups":["admins"]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    support::associate(&state, &org, &user);
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/shared/_acl/read",
        br#"{"actors":["pivotal","shared"],"groups":["admins"]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/shared/_acl/read",
        br#"{"actors":[],"users":["pivotal","shared"],"clients":["shared"],"groups":["admins"]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/clients/shared/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let actors = acl["read"]["actors"].as_array().unwrap();
    assert_eq!(
        actors
            .iter()
            .filter(|actor| *actor == &serde_json::json!("shared"))
            .count(),
        2
    );
}
