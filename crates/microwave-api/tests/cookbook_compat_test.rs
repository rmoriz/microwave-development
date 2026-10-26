mod support;

use axum::http::StatusCode;
use axum::Router;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

const HELLO_CHECKSUM: &str = "5d41402abc4b2a76b9719d911017c592";

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

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: &[u8],
    api_version: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            method,
            path,
            body,
            SignVersion::V1_3,
            "pivotal",
            api_version,
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = support::response_json(response).await;
    (status, body)
}

async fn upload_hello(app: &Router) {
    let body = format!(r#"{{"checksums":{{"{HELLO_CHECKSUM}":null}}}}"#);
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
    let url = created["checksums"][HELLO_CHECKSUM]["url"]
        .as_str()
        .unwrap();
    let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri(path)
                .body(axum::body::Body::from("hello"))
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
            "description": "A fabulous new cookbook",
            "long_description": "",
            "maintainer": "Your Name",
            "maintainer_email": "youremail@example.com",
            "license": "Apache v2.0",
            "dependencies": {},
            "attributes": {},
            "recipes": {},
        },
    })
}

fn artifact_document(name: &str, identifier: &str) -> Value {
    json!({
        "name": name,
        "identifier": identifier,
        "version": "1.0.0",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "all_files": [],
        "metadata": {
            "version": "1.0.0",
            "name": name,
            "description": "A fabulous new cookbook",
            "long_description": "",
            "maintainer": "Your Name",
            "maintainer_email": "youremail@example.com",
            "license": "Apache v2.0",
            "dependencies": {},
            "attributes": {},
            "recipes": {},
        },
    })
}

#[tokio::test]
async fn cookbook_create_returns_the_document_and_validates_requests() {
    let app = setup().await;
    upload_hello(&app).await;

    let document = cookbook_document("apache", "1.0.0");
    let body = serde_json::to_vec(&document).unwrap();
    let (status, created) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/apache/1.0.0",
        &body,
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created, document);

    let (_, fetched) = request(
        &app,
        "GET",
        "/organizations/acme/cookbooks/apache/1.0.0",
        b"",
        "2",
    )
    .await;
    assert_eq!(fetched["name"], "apache-1.0.0");
    assert_eq!(fetched["all_files"], json!([]));

    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/first@second/1.0.0",
        b"{}",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"][0],
        "Invalid cookbook name 'first@second' using regex: 'Malformed cookbook name. Must only contain A-Z, a-z, 0-9, _, . or -'."
    );

    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/apache/1.2.-42",
        b"{}",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"][0], "Invalid cookbook version '1.2.-42'.");

    let mut invalid = cookbook_document("bad", "1.0.0");
    invalid["json_class"] = json!("Chef::Node");
    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/bad/1.0.0",
        &serde_json::to_vec(&invalid).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"][0], "Field 'json_class' invalid");

    let mut unknown = cookbook_document("unknown", "1.0.0");
    unknown["something_random"] = json!("x");
    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/unknown/1.0.0",
        &serde_json::to_vec(&unknown).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"][0],
        "Invalid key something_random in request body"
    );

    let mut missing = cookbook_document("missing", "1.0.0");
    missing["metadata"] = json!({});
    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/missing/1.0.0",
        &serde_json::to_vec(&missing).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"][0], "Field 'metadata.version' missing");

    let mut bad_checksum = cookbook_document("nofile", "1.0.0");
    bad_checksum["all_files"] = json!([{
        "name": "recipes/default.rb",
        "path": "recipes/default.rb",
        "checksum": "8288b67da0793b5abec709d6226e6b73",
        "specificity": "default",
    }]);
    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/nofile/1.0.0",
        &serde_json::to_vec(&bad_checksum).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"][0],
        "Manifest has a checksum that hasn't been uploaded."
    );

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/cookbooks/nofile/1.0.0",
        b"",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cookbook_update_handles_frozen_versions_and_checksum_errors() {
    let app = setup().await;
    upload_hello(&app).await;

    let mut document = cookbook_document("frozen", "1.0.0");
    document["frozen?"] = json!(true);
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/frozen/1.0.0",
        &serde_json::to_vec(&document).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let mut update = cookbook_document("frozen", "1.0.0");
    update["metadata"]["description"] = json!("changed");
    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/frozen/1.0.0",
        &serde_json::to_vec(&update).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"][0],
        "The cookbook frozen at version 1.0.0 is frozen. Use the 'force' option to override."
    );

    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/frozen/1.0.0?force=true",
        &serde_json::to_vec(&update).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["frozen?"], true);
    assert_eq!(body["metadata"]["description"], "changed");

    let mut bad_checksum = cookbook_document("frozen", "1.0.0");
    bad_checksum["all_files"] = json!([{
        "name": "recipes/default.rb",
        "path": "recipes/default.rb",
        "checksum": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "specificity": "default",
    }]);
    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/frozen/1.0.0?force=true",
        &serde_json::to_vec(&bad_checksum).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"][0],
        "Manifest has checksum aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa but it hasn't yet been uploaded"
    );
}

#[tokio::test]
async fn environment_cookbooks_filter_by_constraints() {
    let app = setup().await;
    for version in ["1.0.0", "2.0.0", "3.0.0"] {
        let document = cookbook_document("cb", version);
        let (status, _) = request(
            &app,
            "PUT",
            &format!("/organizations/acme/cookbooks/cb/{version}"),
            &serde_json::to_vec(&document).unwrap(),
            "2",
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let environment = json!({
        "name": "prod",
        "cookbook_versions": { "cb": "> 1.0.0" },
    });
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/environments",
        &serde_json::to_vec(&environment).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/environments/prod/cookbooks",
        b"",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cb"]["versions"].as_array().unwrap().len(), 1);
    assert_eq!(body["cb"]["versions"][0]["version"], "3.0.0");

    let (_, body) = request(
        &app,
        "GET",
        "/organizations/acme/environments/prod/cookbooks?num_versions=2",
        b"",
        "2",
    )
    .await;
    assert_eq!(body["cb"]["versions"][0]["version"], "3.0.0");
    assert_eq!(body["cb"]["versions"][1]["version"], "2.0.0");

    let (_, body) = request(
        &app,
        "GET",
        "/organizations/acme/environments/prod/cookbooks/cb",
        b"",
        "2",
    )
    .await;
    assert_eq!(body["cb"]["versions"][0]["version"], "3.0.0");
    assert_eq!(body["cb"]["versions"][1]["version"], "2.0.0");

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/environments/missing/cookbooks",
        b"",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"][0], "Cannot load environment missing");

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/environments/prod/cookbooks/absent",
        b"",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"][0], "Cannot find a cookbook named absent");
}

#[tokio::test]
async fn cookbook_artifacts_crud_and_validation() {
    let app = setup().await;
    upload_hello(&app).await;

    let first = artifact_document("pedant_basic", "1111111111111111111111111111111111111111");
    let second = artifact_document("pedant_basic", "2222222222222222222222222222222222222222");
    let first_url =
        "/organizations/acme/cookbook_artifacts/pedant_basic/1111111111111111111111111111111111111111";
    let second_url =
        "/organizations/acme/cookbook_artifacts/pedant_basic/2222222222222222222222222222222222222222";

    for (url, document) in [(first_url, &first), (second_url, &second)] {
        let (status, _) = request(
            &app,
            "PUT",
            url,
            &serde_json::to_vec(document).unwrap(),
            "2",
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/cookbook_artifacts",
        b"",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["pedant_basic"]["versions"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        body["pedant_basic"]["versions"][0]["identifier"],
        "1111111111111111111111111111111111111111"
    );

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/cookbook_artifacts/pedant_basic",
        b"",
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["pedant_basic"]["versions"].as_array().unwrap().len(),
        2
    );

    let (status, body) = request(&app, "GET", first_url, b"", "2").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("json_class").is_none());
    assert_eq!(
        body["identifier"],
        "1111111111111111111111111111111111111111"
    );
    assert_eq!(body["name"], "pedant_basic");

    let (status, body) = request(
        &app,
        "PUT",
        first_url,
        &serde_json::to_vec(&first).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "Cookbook artifact already exists");

    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbook_artifacts/bad@name/1111111111111111111111111111111111111111",
        &serde_json::to_vec(&artifact_document(
            "bad@name",
            "1111111111111111111111111111111111111111",
        ))
        .unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"][0], "Field 'name' invalid");

    let mismatch = artifact_document("pedant_basic", "ffffffffffffffffffffffffffffffffffffffff");
    let (status, body) = request(
        &app,
        "PUT",
        first_url,
        &serde_json::to_vec(&mismatch).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"][0],
        "Field 'identifier' invalid : 1111111111111111111111111111111111111111 does not match ffffffffffffffffffffffffffffffffffffffff"
    );

    let (status, body) = request(&app, "DELETE", first_url, b"", "2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["identifier"],
        "1111111111111111111111111111111111111111"
    );

    let (status, body) = request(&app, "GET", first_url, b"", "2").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"][0], "not_found");
}

#[tokio::test]
async fn cookbook_artifacts_convert_between_api_versions() {
    let app = setup().await;
    upload_hello(&app).await;

    let mut document = artifact_document("convert", "3333333333333333333333333333333333333333");
    document["all_files"] = json!([{
        "name": "recipes/default.rb",
        "path": "recipes/default.rb",
        "checksum": HELLO_CHECKSUM,
        "specificity": "default",
    }]);
    let url =
        "/organizations/acme/cookbook_artifacts/convert/3333333333333333333333333333333333333333";
    let (status, _) = request(
        &app,
        "PUT",
        url,
        &serde_json::to_vec(&document).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(&app, "GET", url, b"", "0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("all_files").is_none());
    assert_eq!(body["recipes"][0]["name"], "default.rb");
    assert!(body["recipes"][0]["url"]
        .as_str()
        .unwrap()
        .contains("/bookshelf/"));
    assert_eq!(body["files"], json!([]));
}

#[tokio::test]
async fn universe_reports_dependencies_and_locations() {
    let app = setup().await;

    let mut foo = cookbook_document("foo", "1.2.3");
    foo["metadata"]["dependencies"] = json!({ "bar": ">= 1.1.1" });
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/foo/1.2.3",
        &serde_json::to_vec(&foo).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let bar = cookbook_document("bar", "1.2.3");
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/bar/1.2.3",
        &serde_json::to_vec(&bar).unwrap(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(&app, "GET", "/organizations/acme/universe", b"", "2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["foo"]["1.2.3"]["dependencies"]["bar"], ">= 1.1.1");
    assert_eq!(body["foo"]["1.2.3"]["location_type"], "chef_server");
    assert_eq!(
        body["foo"]["1.2.3"]["location_path"],
        "http://0.0.0.0:8000/organizations/acme/cookbooks/foo/1.2.3"
    );
    assert_eq!(body["bar"]["1.2.3"]["dependencies"], json!({}));
}
