mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
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

async fn put_manifest(app: &Router, name: &str, version: &str, manifest: &Value) {
    let body = serde_json::to_vec(manifest).unwrap();
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
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "failed to upload {name} {version}"
    );
}

async fn put_cookbook(app: &Router, name: &str, version: &str, dependencies: Value) {
    let manifest = json!({
        "name": format!("{name}-{version}"),
        "cookbook_name": name,
        "version": version,
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "metadata": {
            "version": version,
            "dependencies": dependencies,
        },
        "all_files": [],
    });
    put_manifest(app, name, version, &manifest).await;
}

async fn create_environment(app: &Router, name: &str, cookbook_versions: Value) {
    let body = json!({
        "name": name,
        "cookbook_versions": cookbook_versions,
    });
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/environments",
            &serde_json::to_vec(&body).unwrap(),
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn solve(app: &Router, environment: &str, run_list: Value) -> (StatusCode, Value) {
    solve_with_api_version(app, environment, run_list, "2").await
}

async fn solve_with_api_version(
    app: &Router,
    environment: &str,
    run_list: Value,
    api_version: &str,
) -> (StatusCode, Value) {
    let body = json!({ "run_list": run_list });
    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            "POST",
            &format!("/organizations/acme/environments/{environment}/cookbook_versions"),
            &serde_json::to_vec(&body).unwrap(),
            SignVersion::V1_3,
            "pivotal",
            api_version,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

async fn solve_with_raw_body(app: &Router, raw_body: &[u8]) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/environments/_default/cookbook_versions",
            raw_body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

/// Upload "hello" through a sandbox and return its checksum.
async fn upload_hello(app: &Router) -> String {
    let checksum = "5d41402abc4b2a76b9719d911017c592";
    let body = format!(r#"{{"checksums":{{"{checksum}":null}}}}"#);
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/sandboxes",
            body.as_bytes(),
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    let url = created["checksums"][checksum]["url"].as_str().unwrap();
    let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            &format!("/organizations/acme/sandboxes/{sandbox_id}"),
            br#"{"is_completed":true}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    checksum.to_string()
}

#[tokio::test]
async fn resolves_dependencies_and_attaches_presigned_urls() {
    let app = setup().await;
    let checksum = upload_hello(&app).await;

    let manifest = json!({
        "name": "app-2.0.0",
        "cookbook_name": "app",
        "version": "2.0.0",
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "metadata": {
            "version": "2.0.0",
            "dependencies": { "lib": ">= 1.0.0" },
            "attributes": { "secret": true },
            "long_description": "not needed by chef-client",
        },
        "all_files": [{
            "name": "recipes/default.rb",
            "path": "recipes/default.rb",
            "checksum": checksum,
            "specificity": "default",
        }],
    });
    put_manifest(&app, "app", "2.0.0", &manifest).await;
    put_cookbook(&app, "lib", "1.0.0", json!({})).await;
    put_cookbook(&app, "lib", "2.0.0", json!({})).await;

    let (status, body) =
        solve_with_api_version(&app, "_default", json!(["recipe[app::default]"]), "2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["version"], "2.0.0");
    assert_eq!(body["lib"]["version"], "2.0.0");
    assert_eq!(body["app"]["metadata"]["dependencies"]["lib"], ">= 1.0.0");
    assert!(body["app"]["metadata"].get("attributes").is_none());
    assert!(body["app"]["metadata"].get("long_description").is_none());
    let url = body["app"]["all_files"][0]["url"].as_str().unwrap();
    assert!(url.contains("/bookshelf/organizations/"));
    assert!(url.contains("/checksums/"));
}

#[tokio::test]
async fn returns_v2_all_files_in_solve_responses() {
    let app = setup().await;
    let checksum = upload_hello(&app).await;

    let manifest = json!({
        "name": "app-2.0.0",
        "cookbook_name": "app",
        "version": "2.0.0",
        "metadata": { "version": "2.0.0" },
        "all_files": [{
            "name": "recipes/default.rb",
            "path": "recipes/default.rb",
            "checksum": checksum,
            "specificity": "default",
        }],
    });
    put_manifest(&app, "app", "2.0.0", &manifest).await;

    let (status, body) = solve(&app, "_default", json!(["recipe[app::default]"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["version"], "2.0.0");
    assert_eq!(body["app"]["cookbook_name"], "app");
    assert_eq!(body["app"]["name"], "app-2.0.0");
    assert!(body["app"].get("recipes").is_none());
    let files = body["app"]["all_files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["name"], "recipes/default.rb");
    assert_eq!(files[0]["path"], "recipes/default.rb");
    let url = files[0]["url"].as_str().unwrap();
    assert!(url.contains("/bookshelf/organizations/"));
    assert!(url.contains("/checksums/"));
}

#[tokio::test]
async fn selects_versions_using_pessimistic_constraints() {
    let app = setup().await;
    put_cookbook(&app, "app", "1.0.0", json!({ "lib": "~> 2.1" })).await;
    put_cookbook(&app, "lib", "2.1.5", json!({})).await;
    put_cookbook(&app, "lib", "2.2.0", json!({})).await;
    put_cookbook(&app, "lib", "3.0.0", json!({})).await;

    let (status, body) = solve(&app, "_default", json!(["app"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["version"], "1.0.0");
    assert_eq!(body["lib"]["version"], "2.2.0");
}

#[tokio::test]
async fn returns_structured_412_for_missing_cookbook() {
    let app = setup().await;

    let (status, body) = solve(&app, "_default", json!(["recipe[missing]"])).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        body["error"][0],
        json!({
            "message": "Run list contains invalid items: no such cookbook missing.",
            "non_existent_cookbooks": ["missing"],
            "cookbooks_with_no_versions": [],
        })
    );
}

#[tokio::test]
async fn returns_structured_412_for_conflicting_dependencies() {
    let app = setup().await;
    put_cookbook(&app, "foo", "1.2.3", json!({ "bar": "> 2.0.0" })).await;
    put_cookbook(&app, "bar", "2.0.0", json!({})).await;

    let (status, body) = solve(&app, "_default", json!(["foo"])).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    let error = &body["error"][0];
    assert_eq!(error["unsatisfiable_run_list_item"], "(foo >= 0.0.0)");
    assert_eq!(error["non_existent_cookbooks"], json!([]));
    assert_eq!(
        error["most_constrained_cookbooks"],
        json!(["bar = 2.0.0 -> []"])
    );
    assert_eq!(
        error["message"],
        "Unable to satisfy constraints on package bar due to solution constraint (foo >= 0.0.0). \
         Solution constraints that may result in a constraint on bar: [(foo = 1.2.3) -> (bar > 2.0.0)]"
    );
}

#[tokio::test]
async fn applies_environment_cookbook_version_constraints() {
    let app = setup().await;
    put_cookbook(&app, "app", "1.0.0", json!({})).await;
    put_cookbook(&app, "app", "2.0.0", json!({})).await;
    create_environment(&app, "filtered", json!({ "app": "= 1.0.0" })).await;

    let (status, body) = solve(&app, "filtered", json!(["app"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["version"], "1.0.0");

    create_environment(&app, "empty", json!({ "app": "= 9.0.0" })).await;
    let (status, body) = solve(&app, "empty", json!(["app"])).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        body["error"][0],
        json!({
            "message": "Run list contains invalid items: no versions match the constraints on cookbook (app >= 0.0.0).",
            "non_existent_cookbooks": [],
            "cookbooks_with_no_versions": ["(app >= 0.0.0)"],
        })
    );
}

#[tokio::test]
async fn chooses_highest_version_deterministically() {
    let app = setup().await;
    put_cookbook(&app, "app", "2.0.0", json!({})).await;
    put_cookbook(&app, "app", "9.0.0", json!({})).await;
    put_cookbook(&app, "app", "10.0.0", json!({})).await;

    for _ in 0..3 {
        let (status, body) = solve(&app, "_default", json!(["app"])).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["app"]["version"], "10.0.0");
    }
}

#[tokio::test]
async fn expands_roles_in_the_run_list() {
    let app = setup().await;
    put_cookbook(&app, "app", "1.0.0", json!({})).await;

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/roles",
            br#"{"name":"base","run_list":["recipe[app]"]}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let (status, body) = solve(&app, "_default", json!(["role[base]"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["version"], "1.0.0");
}

#[tokio::test]
async fn honors_versioned_run_list_items() {
    let app = setup().await;
    put_cookbook(&app, "app", "1.0.0", json!({})).await;
    put_cookbook(&app, "app", "2.0.0", json!({})).await;

    let (status, body) = solve(&app, "_default", json!(["app@1.0.0"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["version"], "1.0.0");

    let (status, body) = solve(&app, "_default", json!(["recipe[app::default@2.0.0]"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["version"], "2.0.0");
}

#[tokio::test]
async fn returns_empty_object_for_empty_run_list() {
    let app = setup().await;
    let (status, body) = solve(&app, "_default", json!([])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({}));
}

#[tokio::test]
async fn rejects_invalid_run_list_items() {
    let app = setup().await;

    let (status, body) = solve(&app, "_default", json!([12])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({ "error": ["Field 'run_list' is not a valid run list"] })
    );
}

#[tokio::test]
async fn returns_404_for_missing_environment() {
    let app = setup().await;
    let (status, body) = solve(&app, "does-not-exist", json!([])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({ "error": ["environment 'does-not-exist' not found"] })
    );
}

#[tokio::test]
async fn rejects_empty_request_body_as_invalid_json() {
    let app = setup().await;
    let (status, body) = solve_with_raw_body(&app, b"").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({ "error": ["invalid JSON"] }));
}

#[tokio::test]
async fn rejects_non_json_request_body_as_invalid_json() {
    let app = setup().await;
    let (status, body) = solve_with_raw_body(&app, b"this_is_not_json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({ "error": ["invalid JSON"] }));
}

#[tokio::test]
async fn rejects_malformed_json_request_body_as_invalid_json() {
    let app = setup().await;
    let (status, body) = solve_with_raw_body(&app, br#"{"run_list": "#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({ "error": ["invalid JSON"] }));
}
