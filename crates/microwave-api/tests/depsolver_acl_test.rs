mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use microwave_store::Permission;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn put_cookbook(app: &axum::Router, name: &str, version: &str) {
    let manifest = json!({
        "name": format!("{name}-{version}"),
        "cookbook_name": name,
        "version": version,
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "metadata": { "version": version, "dependencies": {} },
        "all_files": [],
    });
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            &format!("/organizations/acme/cookbooks/{name}/{version}"),
            &serde_json::to_vec(&manifest).unwrap(),
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn depsolver_rejects_cookbooks_without_read_permission() {
    let state = support::state_with_pivotal();
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state.clone());

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations",
            br#"{"name":"acme","full_name":"Acme Corp"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    support::associate(&state, &org, &alice);
    let users = state
        .sqlite()
        .find_group(&org.id, "users")
        .unwrap()
        .unwrap();
    state
        .sqlite()
        .add_group_member(&users.authz_id, &alice.authz_id)
        .unwrap();

    put_cookbook(&app, "app", "1.0.0").await;
    let versions = state.sqlite().list_cookbook_versions(&org.id).unwrap();
    let app_version = versions
        .iter()
        .find(|version| version.name == "app")
        .unwrap();
    state
        .sqlite()
        .set_acl_for_permission_split(
            &app_version.authz_id,
            Permission::Read,
            &["pivotal".to_string()],
            &[],
            &[],
        )
        .unwrap();

    let body = br#"{"run_list":["recipe[app]"]}"#;
    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/environments/_default/cookbook_versions",
            body,
            SignVersion::V1_3,
            "alice",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = support::response_json(response).await;
    assert_eq!(
        body["unauthorized_cookbooks"],
        json!([{ "name": "app", "version": "1.0.0" }])
    );
}
