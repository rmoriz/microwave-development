mod support;

use axum::http::StatusCode;
use axum::Router;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn setup() -> Router {
    let app = support::app(support::state_with_pivotal());
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
    app
}

fn cookbook_document(name: &str, version: &str) -> Value {
    json!({
        "name": format!("{name}-{version}"),
        "cookbook_name": name,
        "version": version,
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "all_files": [],
        "metadata": {
            "version": version,
            "name": name,
            "dependencies": {},
            "attributes": {},
        },
    })
}

async fn put_cookbook(app: &Router, name: &str, version: &str) {
    let body = serde_json::to_vec(&cookbook_document(name, version)).unwrap();
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            &format!("/organizations/acme/cookbooks/{name}/{version}"),
            &body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED, "{name} {version}");
}

async fn get_cookbook(app: &Router, name: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            &format!("/organizations/acme/cookbooks/{name}"),
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

#[tokio::test]
async fn cookbook_versions_are_listed_newest_first() {
    let app = setup().await;

    // Upload in a deliberately unsorted order so the response must sort by
    // major, minor, patch rather than insertion order or string order.
    for version in ["1.2.3", "2.0.0", "1.10.0"] {
        put_cookbook(&app, "apache", version).await;
    }

    let (status, body) = get_cookbook(&app, "apache").await;
    assert_eq!(status, StatusCode::OK);
    let versions: Vec<&str> = body["apache"]["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|version| version["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["2.0.0", "1.10.0", "1.2.3"]);
}
