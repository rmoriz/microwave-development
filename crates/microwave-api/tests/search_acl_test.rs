mod support;

use axum::http::StatusCode;
use microwave_config::Config;
use microwave_crypto::SignVersion;
use microwave_store::{NewNode, Permission};
use serde_json::Value;
use tower::ServiceExt;

fn strict_config(strict: bool) -> Config {
    Config::from_toml_str(&format!("[search]\nstrict_result_acls = {strict}\n")).unwrap()
}

async fn search(strict: bool) -> (usize, usize) {
    let state = support::state_with_config(strict_config(strict));
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    let org = support::add_org(&state, "acme").await;
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
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    let body = r#"{"name":"web1","chef_environment":"_default","run_list":[]}"#;
    let node = state
        .sqlite()
        .create_node(
            &org.id,
            NewNode {
                name: "web1",
                body,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &pivotal.authz_id,
            },
        )
        .unwrap();
    state
        .sqlite()
        .set_acl_for_permission_split(
            &node.authz_id,
            Permission::Read,
            &["pivotal".to_string()],
            &[],
            &[],
        )
        .unwrap();

    let response = support::app(state)
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/search/node?q=*:*",
            b"",
            SignVersion::V1_3,
            "alice",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = support::response_json(response).await;
    (
        body["total"].as_u64().unwrap() as usize,
        body["rows"].as_array().unwrap().len(),
    )
}

#[tokio::test]
async fn strict_result_acls_hide_unreadable_rows_but_keep_the_total() {
    let (total, rows) = search(true).await;
    assert_eq!(total, 1, "upstream reports the pre-filter total");
    assert_eq!(rows, 0, "strict mode must hide rows the actor cannot read");
}

#[tokio::test]
async fn non_strict_search_keeps_upstream_default_behavior() {
    let (total, rows) = search(false).await;
    assert_eq!(total, 1);
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn strict_filtering_happens_before_pagination() {
    let mut config = microwave_config::Config::default();
    config.search.strict_result_acls = true;
    let state = support::state_with_config(config);
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    let org = support::add_org(&state, "acme").await;
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
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    for name in ["web1", "web2"] {
        let body = format!(r#"{{"name":"{name}","chef_environment":"_default","run_list":[]}}"#);
        state
            .sqlite()
            .create_node(
                &org.id,
                NewNode {
                    name,
                    body: &body,
                    environment: "_default",
                    policy_name: None,
                    policy_group: None,
                    creator_authz_id: &pivotal.authz_id,
                },
            )
            .unwrap();
    }
    let web1 = state.sqlite().find_node(&org.id, "web1").unwrap().unwrap();
    state
        .sqlite()
        .set_acl_for_permission_split(
            &web1.authz_id,
            Permission::Read,
            &["pivotal".to_string()],
            &[],
            &[],
        )
        .unwrap();

    let response = support::app(state)
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/search/node?q=*:*&start=0&rows=1",
            b"",
            SignVersion::V1_3,
            "alice",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = support::response_json(response).await;
    assert_eq!(body["total"], 2);
    assert_eq!(body["rows"].as_array().unwrap().len(), 1);
    assert_eq!(body["rows"][0]["name"], "web2");
}
