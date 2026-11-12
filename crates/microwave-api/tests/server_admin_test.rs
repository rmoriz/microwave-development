//! Server-admin parity: the global `users` container authorizes user
//! management exactly as erchef does.

mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use microwave_store::Permission;
use tower::ServiceExt;

async fn send(app: &axum::Router, method: &str, path: &str, body: &[u8], user: &str) -> StatusCode {
    app.clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
        ))
        .await
        .unwrap()
        .status()
}

const CREATE_BODY: &[u8] =
    br#"{"name":"dave","email":"dave@example.com","display_name":"Dave","password":"secret1"}"#;

/// Mirror what `ensure_superuser` does on a real boot: the bootstrap
/// superuser inherits no `server-admins` ACEs on its own account.
fn revoke_superuser_server_admin_aces(state: &microwave_api::AppState) {
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    let group = state.sqlite().ensure_global_group("server-admins").unwrap();
    state
        .sqlite()
        .revoke_group_acl(&pivotal.authz_id, &group.authz_id)
        .unwrap();
}

#[tokio::test]
async fn server_admin_can_manage_users() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "bob", "bob@example.com", true);
    let carol = support::add_user(&state, "carol", "carol@example.com", false);
    // Bob is not a member of this organization; the `users` container still
    // authorizes managing Carol, as upstream's ctl help text promises.
    let org = support::add_org(&state, "acme").await;
    support::associate(&state, &org, &carol);
    let app = support::app(state);

    assert_eq!(
        send(&app, "GET", "/users", b"", "bob").await,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, "POST", "/users", CREATE_BODY, "bob").await,
        StatusCode::CREATED
    );
    assert_eq!(
        send(&app, "GET", "/users/carol", b"", "bob").await,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            "PUT",
            "/users/carol",
            br#"{"name":"carol","display_name":"Carol","email":"carol@example.com"}"#,
            "bob"
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, "DELETE", "/users/carol", b"", "bob").await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn normal_user_cannot_manage_users() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "eve", "eve@example.com", false);
    support::add_user(&state, "carol", "carol@example.com", false);
    let app = support::app(state);

    assert_eq!(
        send(&app, "GET", "/users", b"", "eve").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, "POST", "/users", CREATE_BODY, "eve").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, "GET", "/users/carol", b"", "eve").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &app,
            "PUT",
            "/users/carol",
            br#"{"name":"carol","display_name":"C","email":"carol@example.com"}"#,
            "eve"
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, "DELETE", "/users/carol", b"", "eve").await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn server_admin_cannot_manage_the_superuser() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "bob", "bob@example.com", true);
    revoke_superuser_server_admin_aces(&state);
    let app = support::app(state.clone());

    assert_eq!(
        send(&app, "GET", "/users/pivotal", b"", "bob").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &app,
            "PUT",
            "/users/pivotal",
            br#"{"name":"pivotal","display_name":"P","email":"pivotal@example.com"}"#,
            "bob"
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, "DELETE", "/users/pivotal", b"", "bob").await,
        StatusCode::FORBIDDEN
    );

    // Defence in depth: even a drifted ACL that names server-admins must not
    // let a server-admin read or update the configured superuser.
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    let group = state.sqlite().ensure_global_group("server-admins").unwrap();
    for permission in [Permission::Read, Permission::Update] {
        state
            .sqlite()
            .set_acl(&pivotal.authz_id, permission, None, Some(&group.authz_id))
            .unwrap();
    }
    assert_eq!(
        send(&app, "GET", "/users/pivotal", b"", "bob").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &app,
            "PUT",
            "/users/pivotal",
            br#"{"name":"pivotal","display_name":"P","email":"pivotal@example.com"}"#,
            "bob"
        )
        .await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn admin_flag_is_superuser_only() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "bob", "bob@example.com", true);
    support::add_user(&state, "eve", "eve@example.com", false);
    support::add_user(&state, "carol", "carol@example.com", false);
    let app = support::app(state);

    // A normal user cannot promote themselves to server-admin.
    assert_eq!(
        send(
            &app,
            "PUT",
            "/users/eve",
            br#"{"name":"eve","display_name":"Eve","email":"eve@example.com","admin":true}"#,
            "eve"
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, "GET", "/users", b"", "eve").await,
        StatusCode::FORBIDDEN,
        "the failed promotion must not grant the users container"
    );

    // A server-admin cannot mint another server-admin or promote an account.
    assert_eq!(
        send(
            &app,
            "POST",
            "/users",
            br#"{"name":"dave","email":"dave@example.com","display_name":"Dave","password":"secret1","admin":true}"#,
            "bob"
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &app,
            "PUT",
            "/users/carol",
            br#"{"name":"carol","display_name":"Carol","email":"carol@example.com","admin":true}"#,
            "bob"
        )
        .await,
        StatusCode::FORBIDDEN
    );

    // The superuser may still set it.
    assert_eq!(
        send(
            &app,
            "PUT",
            "/users/eve",
            br#"{"name":"eve","display_name":"Eve","email":"eve@example.com","admin":true}"#,
            "pivotal"
        )
        .await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn sensitive_profile_fields_are_superuser_only() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "bob", "bob@example.com", true);
    support::add_user(&state, "carol", "carol@example.com", false);
    let app = support::app(state);

    let body = br#"{"name":"carol","display_name":"Carol","email":"carol@example.com","recovery_authentication_enabled":true}"#;
    assert_eq!(
        send(&app, "PUT", "/users/carol", body, "bob").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, "PUT", "/users/carol", body, "pivotal").await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn organization_members_can_read_each_others_profiles() {
    let state = support::state_with_pivotal();
    let org = support::add_org(&state, "acme").await;
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    let bob = support::add_user(&state, "bob", "bob@example.com", false);
    support::associate(&state, &org, &alice);
    support::associate(&state, &org, &bob);
    let app = support::app(state);

    assert_eq!(
        send(&app, "GET", "/users/bob", b"", "alice").await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn server_admin_can_grant_on_a_user_it_created() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "bob", "bob@example.com", true);
    let app = support::app(state);

    assert_eq!(
        send(&app, "POST", "/users", CREATE_BODY, "bob").await,
        StatusCode::CREATED
    );
    assert_eq!(
        send(
            &app,
            "PUT",
            "/users/dave/_acl/read",
            br#"{"actors":["dave"],"groups":[]}"#,
            "bob"
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, "GET", "/users/dave/_acl", b"", "bob").await,
        StatusCode::OK,
        "the creator holds grant, so it may also read the ACL"
    );
}
