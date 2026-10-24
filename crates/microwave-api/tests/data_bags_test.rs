mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn request(app: &axum::Router, method: &str, path: &str, body: &[u8]) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            "pivotal",
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
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn validates_data_bag_names() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    for name in [
        "pedant",
        "pedant-bag",
        "pedant_bag",
        "pedant:with:colons",
        "pedant.with.dots",
        "1234567890",
    ] {
        let body = format!(r#"{{"name":"{name}"}}"#);
        let (status, created) =
            request(&app, "POST", "/organizations/acme/data", body.as_bytes()).await;
        assert_eq!(status, StatusCode::CREATED, "name {name}");
        assert_eq!(
            created["uri"],
            format!("http://0.0.0.0:8000/organizations/acme/data/{name}")
        );
    }

    for name in [
        "pedant_badName!!$$$$_oh_very+bad",
        "pedant-does-not-like-punctuation!!!!",
    ] {
        let body = format!(r#"{{"name":"{name}"}}"#);
        let (status, error) =
            request(&app, "POST", "/organizations/acme/data", body.as_bytes()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "name {name}");
        assert_eq!(error, json!({"error": ["Field 'name' invalid"]}));

        let (status, error) = request(
            &app,
            "GET",
            &format!("/organizations/acme/data/{name}"),
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            error,
            json!({"error": [format!("Cannot load data bag {name}")]})
        );
    }

    let (status, error) = request(
        &app,
        "POST",
        "/organizations/acme/data",
        br#"{"id":"pedant_testbag"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error, json!({"error": ["Field 'name' missing"]}));

    let (status, error) =
        request(&app, "GET", "/organizations/acme/data/pedant_testbag", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error,
        json!({"error": ["Cannot load data bag pedant_testbag"]})
    );
}

#[tokio::test]
async fn item_creation_returns_the_item_document() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/data",
        br#"{"name":"secrets"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, item) = request(
        &app,
        "POST",
        "/organizations/acme/data/secrets",
        br#"{"id":"database","password":"one"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        item,
        json!({
            "id": "database",
            "password": "one",
            "chef_type": "data_bag_item",
            "data_bag": "secrets"
        })
    );

    // The stored document is returned as-is by GET.
    let (status, item) = request(
        &app,
        "GET",
        "/organizations/acme/data/secrets/database",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item, json!({"id": "database", "password": "one"}));

    let (status, item) = request(
        &app,
        "PUT",
        "/organizations/acme/data/secrets/database",
        br#"{"password":"two"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        item,
        json!({
            "id": "database",
            "password": "two",
            "chef_type": "data_bag_item",
            "data_bag": "secrets"
        })
    );

    let (status, error) = request(
        &app,
        "POST",
        "/organizations/acme/data/secrets",
        br#"{"id":"database","password":"three"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        error,
        json!({"error": ["Data Bag Item 'database' already exists in Data Bag 'secrets'."]})
    );

    let (status, error) = request(
        &app,
        "PUT",
        "/organizations/acme/data/secrets/database",
        br#"{"id":"other","password":"three"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error, json!({"error": ["DataBagItem name mismatch."]}));
}

#[tokio::test]
async fn validates_item_ids_and_missing_bags() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, error) = request(
        &app,
        "POST",
        "/organizations/acme/data",
        br#"{"name":"secrets"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        error["uri"],
        "http://0.0.0.0:8000/organizations/acme/data/secrets"
    );

    let (status, error) = request(
        &app,
        "POST",
        "/organizations/acme/data/secrets",
        br#"{"answer":42}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error, json!({"error": ["Field 'id' missing"]}));

    for id in ["pedant_badId!!", "^$@^*  pedant"] {
        let body = format!(r#"{{"id":"{id}","answer":42}}"#);
        let (status, error) = request(
            &app,
            "POST",
            "/organizations/acme/data/secrets",
            body.as_bytes(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "id {id}");
        assert_eq!(error, json!({"error": ["Field 'id' invalid"]}));
    }

    let (status, error) = request(
        &app,
        "POST",
        "/organizations/acme/data/missing",
        br#"{"id":"blah"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error,
        json!({"error": ["No data bag 'missing' could be found. Please create this data bag before adding items to it."]})
    );

    let (status, error) = request(
        &app,
        "PUT",
        "/organizations/acme/data/secrets/absent",
        br#"{"id":"absent"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error,
        json!({"error": ["Cannot load data bag item absent for data bag secrets"]})
    );

    let (status, error) =
        request(&app, "GET", "/organizations/acme/data/secrets/absent", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error,
        json!({"error": ["Cannot load data bag item absent for data bag secrets"]})
    );

    let (status, error) = request(
        &app,
        "DELETE",
        "/organizations/acme/data/secrets/absent",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error,
        json!({"error": ["Cannot load data bag secrets item absent"]})
    );

    let (status, error) =
        request(&app, "GET", "/organizations/acme/data/missing/absent", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error,
        json!({"error": ["Cannot load data bag item absent for data bag missing"]})
    );

    let (status, error) = request(
        &app,
        "DELETE",
        "/organizations/acme/data/missing/absent",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error,
        json!({"error": ["Cannot load data bag missing item absent"]})
    );
}

#[tokio::test]
async fn deletes_bag_and_item_documents() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/data",
        br#"{"name":"secrets"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/data/secrets",
        br#"{"id":"database","password":"one"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, deleted) = request(
        &app,
        "DELETE",
        "/organizations/acme/data/secrets/database",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        deleted,
        json!({
            "name": "data_bag_item_secrets_database",
            "json_class": "Chef::DataBagItem",
            "chef_type": "data_bag_item",
            "data_bag": "secrets",
            "raw_data": {"id": "database", "password": "one"}
        })
    );

    let (status, error) = request(
        &app,
        "GET",
        "/organizations/acme/data/secrets/database",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error,
        json!({"error": ["Cannot load data bag item database for data bag secrets"]})
    );

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/data/secrets",
        br#"{"id":"database","password":"one"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, deleted) = request(&app, "DELETE", "/organizations/acme/data/secrets", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        deleted,
        json!({
            "name": "secrets",
            "json_class": "Chef::DataBag",
            "chef_type": "data_bag"
        })
    );

    let (status, error) = request(&app, "GET", "/organizations/acme/data/secrets", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error, json!({"error": ["Cannot load data bag secrets"]}));
}

#[tokio::test]
async fn method_not_allowed_allow_headers_match_upstream() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/data",
        br#"{"name":"secrets"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            "/organizations/acme/data",
            br#"{"fake":"value"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET, POST");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            "/organizations/acme/data/secrets",
            br#"{"fake":"value"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET, POST, DELETE");

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/data/secrets/database",
            br#"{"fake":"value"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET, PUT, DELETE");
}
