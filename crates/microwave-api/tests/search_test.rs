mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn create_org(app: &axum::Router) {
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
}

async fn request(app: &axum::Router, method: &str, path: &str, body: &[u8]) -> (StatusCode, Value) {
    request_as(app, method, path, body, "pivotal").await
}

async fn request_as(
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

fn encode(query: &str) -> String {
    let mut encoded = String::new();
    for character in query.chars() {
        match character {
            ' ' => encoded.push_str("%20"),
            '[' => encoded.push_str("%5B"),
            ']' => encoded.push_str("%5D"),
            '{' => encoded.push_str("%7B"),
            '}' => encoded.push_str("%7D"),
            '"' => encoded.push_str("%22"),
            '(' => encoded.push_str("%28"),
            ')' => encoded.push_str("%29"),
            '+' => encoded.push_str("%2B"),
            other => encoded.push(other),
        }
    }
    encoded
}

async fn search(app: &axum::Router, index: &str, query: &str) -> (StatusCode, Value) {
    request(
        app,
        "GET",
        &format!("/organizations/acme/search/{index}?q={}", encode(query)),
        b"",
    )
    .await
}

async fn create_node(app: &axum::Router, node: Value) {
    let body = serde_json::to_vec(&node).unwrap();
    let (status, response) = request(app, "POST", "/organizations/acme/nodes", &body).await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
}

fn names(rows: &Value) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn lists_builtin_and_data_bag_indexes() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, indexes) = request(&app, "GET", "/organizations/acme/search", b"").await;
    assert_eq!(status, StatusCode::OK);
    for index in ["client", "environment", "node", "role"] {
        assert_eq!(
            indexes[index],
            format!("http://0.0.0.0:8000/organizations/acme/search/{index}")
        );
    }
    assert!(indexes.get("secrets").is_none());

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/data",
        br#"{"name":"secrets"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, indexes) = request(&app, "GET", "/organizations/acme/search", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        indexes["secrets"],
        "http://0.0.0.0:8000/organizations/acme/search/secrets"
    );
}

#[tokio::test]
async fn searches_nodes_with_the_supported_query_subset() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    create_node(
        &app,
        json!({
            "name": "web1",
            "chef_environment": "production",
            "normal": {
                "platform": "ubuntu",
                "role": "web",
                "cpu": 4,
                "description": "hello world",
                "nested": { "leaf": "deep" }
            },
            "automatic": { "os": "linux" }
        }),
    )
    .await;
    create_node(
        &app,
        json!({
            "name": "web2",
            "chef_environment": "staging",
            "default": { "platform": "debian", "role": "db", "cpu": 8 },
            "automatic": { "os": "linux" },
            "run_list": ["recipe[apache]", "role[base]"]
        }),
    )
    .await;

    let (status, result) = search(&app, "node", "*:*").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 2);
    assert_eq!(result["start"], 0);
    assert_eq!(names(&result["rows"]), vec!["web1", "web2"]);

    let (status, result) = request(&app, "GET", "/organizations/acme/search/node", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 2);

    let (status, result) = search(&app, "node", "name:web1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 1);
    assert_eq!(names(&result["rows"]), vec!["web1"]);

    let (status, result) = search(&app, "node", "platform:ubuntu").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1"]);

    let (status, result) = search(&app, "node", "nested_leaf:deep").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1"]);

    let (status, result) = search(&app, "node", "leaf:deep").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1"]);

    let (status, result) = search(&app, "node", "description:\"hello world\"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1"]);

    let (status, result) = search(&app, "node", "platform:ubuntu AND role:web").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1"]);

    let (status, result) = search(&app, "node", "platform:ubuntu AND NOT name:web1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 0);

    let (status, result) = search(&app, "node", "name:web*").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1", "web2"]);

    let (status, result) = search(&app, "node", "cpu:[5 TO 9]").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web2"]);

    let (status, result) = search(&app, "node", "chef_environment:production").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1"]);

    let (status, result) = search(&app, "node", "recipe:apache").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web2"]);

    let (status, result) = search(&app, "node", "role:base").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web2"]);

    let (status, result) = search(&app, "node", "name:web1 OR name:web2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 2);

    let (status, result) =
        search(&app, "node", "(name:web1 OR name:web2) AND platform:ubuntu").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1"]);

    let (status, result) = search(&app, "node", "+name:web1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 1);

    let (status, result) = request(
        &app,
        "GET",
        "/organizations/acme/search/node?q=name:web*&start=1&rows=1",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 2);
    assert_eq!(result["start"], 1);
    assert_eq!(result["rows"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn rejects_empty_or_invalid_queries_and_unknown_indexes() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, error) = request(&app, "GET", "/organizations/acme/search/node?q=", b"").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error["error"].as_array().unwrap().len() == 1);

    let (status, error) = search(&app, "node", "name:").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error["error"].as_array().unwrap().len() == 1);

    let (status, error) = search(&app, "node", "(name:web1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error["error"].as_array().unwrap().len() == 1);

    let (status, error) = search(&app, "no_such_index", "*:*").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error["error"][0],
        "I don't know how to search for no_such_index data objects."
    );
}

#[tokio::test]
async fn searches_data_bag_items() {
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

    for item in [
        br#"{"id":"database","engine":"postgres","password":"one","port":5432}"#.as_slice(),
        br#"{"id":"cache","engine":"redis","port":6379}"#.as_slice(),
    ] {
        let (status, _) = request(&app, "POST", "/organizations/acme/data/secrets", item).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (status, result) = search(&app, "secrets", "*:*").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 2);
    let rows = result["rows"].as_array().unwrap();
    assert_eq!(rows[0]["name"], "data_bag_item_secrets_cache");
    assert_eq!(rows[0]["json_class"], "Chef::DataBagItem");
    assert_eq!(rows[0]["chef_type"], "data_bag_item");
    assert_eq!(rows[0]["data_bag"], "secrets");
    assert_eq!(rows[0]["raw_data"]["engine"], "redis");

    let (status, result) = search(&app, "secrets", "engine:postgres").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 1);
    assert_eq!(result["rows"][0]["name"], "data_bag_item_secrets_database");
    assert_eq!(result["rows"][0]["raw_data"]["password"], "one");

    let (status, result) = search(&app, "secrets", "port:[6000 TO 7000]").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["rows"][0]["name"], "data_bag_item_secrets_cache");

    let (status, result) = search(&app, "secrets", "raw_data_engine:redis").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 0);
}

#[tokio::test]
async fn performs_partial_searches() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    create_node(
        &app,
        json!({
            "name": "web1",
            "normal": { "platform": "ubuntu", "cpu": 4 },
            "default": { "description": "hello world" }
        }),
    )
    .await;

    let body = json!({
        "name": ["name"],
        "platform": ["platform"],
        "description": ["default", "description"],
        "missing": ["no", "such", "field"]
    });
    let (status, result) = request(
        &app,
        "POST",
        "/organizations/acme/search/node?q=name:web1",
        serde_json::to_vec(&body).unwrap().as_slice(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["total"], 1);
    assert_eq!(
        result["rows"][0]["url"],
        "http://0.0.0.0:8000/organizations/acme/nodes/web1"
    );
    assert_eq!(result["rows"][0]["data"]["name"], "web1");
    assert_eq!(result["rows"][0]["data"]["platform"], "ubuntu");
    assert_eq!(result["rows"][0]["data"]["description"], "hello world");
    assert!(result["rows"][0]["data"]["missing"].is_null());

    for invalid in [
        br#""z[$blah""#.as_slice(),
        br#"{"a_string":"blah"}"#.as_slice(),
        br#"{"a_number":1}"#.as_slice(),
        br#"{"an_array":[1,2]}"#.as_slice(),
    ] {
        let (status, _) = request(
            &app,
            "POST",
            "/organizations/acme/search/node?q=name:web1",
            invalid,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn client_search_rows_include_upstream_fields_by_api_version() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, created) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"web1","create_key":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    for (api_version, expects_public_key) in [("0", true), ("2", false)] {
        let response = app
            .clone()
            .oneshot(support::signed_request_with_api_version(
                "GET",
                "/organizations/acme/search/client?q=name:web1",
                b"",
                SignVersion::V1_3,
                "pivotal",
                api_version,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result = support::response_json(response).await;
        assert_eq!(result["total"], 1, "api version {api_version}");
        let row = &result["rows"][0];
        assert_eq!(row["name"], "web1");
        assert_eq!(row["clientname"], "web1");
        assert_eq!(row["orgname"], "acme");
        if expects_public_key {
            assert!(
                row["public_key"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("BEGIN PUBLIC KEY"),
                "api version {api_version}: {row}"
            );
        } else {
            assert!(
                row.get("public_key").is_none(),
                "api version {api_version}: {row}"
            );
        }
    }
}

#[tokio::test]
async fn search_rows_are_filtered_by_object_read_acl() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "second", "second@example.com", false);
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let second = state.sqlite().find_user("second").unwrap().unwrap();
    support::associate(&state, &org, &second);

    create_node(&app, json!({ "name": "web1" })).await;
    create_node(&app, json!({ "name": "web2" })).await;

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web2/_acl/read",
        br#"{"actors":["pivotal"],"groups":[]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, result) = request_as(
        &app,
        "GET",
        "/organizations/acme/search/node?q=name:*",
        b"",
        "second",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1"]);

    let payload = json!({ "name": ["name"] });
    let (status, result) = request_as(
        &app,
        "POST",
        "/organizations/acme/search/node?q=name:*",
        serde_json::to_vec(&payload).unwrap().as_slice(),
        "second",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        result["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["data"]["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["web1"]
    );

    let (status, result) =
        request(&app, "GET", "/organizations/acme/search/node?q=name:*", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1", "web2"]);

    // Upstream accepts a trailing slash on the index name.
    let (status, result) = request(
        &app,
        "GET",
        "/organizations/acme/search/node/?q=name:web1",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&result["rows"]), vec!["web1"]);
}

#[tokio::test]
async fn data_bag_search_rows_use_the_bag_read_acl() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "second", "second@example.com", false);
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let second = state.sqlite().find_user("second").unwrap().unwrap();
    support::associate(&state, &org, &second);

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
        br#"{"id":"database","engine":"postgres"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/data/secrets/_acl/read",
        br#"{"actors":["pivotal"],"groups":[]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, result) = request_as(
        &app,
        "GET",
        "/organizations/acme/search/secrets?q=id:*",
        b"",
        "second",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["total"], 0);
    assert!(result["rows"].as_array().unwrap().is_empty());

    let (status, result) = request(
        &app,
        "GET",
        "/organizations/acme/search/secrets?q=id:*",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        names(&result["rows"]),
        vec!["data_bag_item_secrets_database"]
    );
}

#[tokio::test]
async fn partial_search_of_data_bag_items_wraps_url_and_data() {
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
        br#"{"id":"database","engine":"postgres","password":"one"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let body = json!({ "engine": ["engine"], "password": ["password"] });
    let (status, result) = request(
        &app,
        "POST",
        "/organizations/acme/search/secrets?q=engine:postgres",
        serde_json::to_vec(&body).unwrap().as_slice(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["total"], 1);
    assert_eq!(
        result["rows"][0]["url"],
        "http://0.0.0.0:8000/organizations/acme/data/secrets/database"
    );
    assert_eq!(result["rows"][0]["data"]["engine"], "postgres");
    assert_eq!(result["rows"][0]["data"]["password"], "one");
}
