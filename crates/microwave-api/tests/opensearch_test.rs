mod support;

use axum::http::StatusCode;
use microwave_config::{Config, SearchBackend};
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

#[tokio::test]
async fn search_route_uses_the_configured_opensearch_backend() {
    let server = support::mock_opensearch::start().await;
    let mut config = Config::default();
    config.search.backend = SearchBackend::OpenSearch;
    config.search.url = Some(server.base_url.clone());
    let state = support::state_with_config(config);
    let app = support::app(state.clone());

    let (status, _) = request(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1","chef_environment":"production","normal":{"platform":"ubuntu"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let org = state.store.find_org("acme").unwrap().unwrap();
    let node = state.store.find_node(&org.id, "web1").unwrap().unwrap();

    // The node write indexed a document through `_bulk`.
    let bulk = server.bulk_bodies();
    assert!(
        bulk.iter()
            .any(|body| body.contains("name__=__web1") && body.contains("\"_id\":\"")),
        "no node bulk request captured: {bulk:?}"
    );
    assert!(
        bulk.iter()
            .any(|body| body.contains("\"X_CHEF_type_CHEF_X\":\"node\"")),
        "bulk document did not carry the object type: {bulk:?}"
    );

    // OpenSearch reports the matching document ID; the route hydrates it
    // from the store.
    server.set_hits(std::slice::from_ref(&node.id));
    let (status, result) = request(
        &app,
        "GET",
        "/organizations/acme/search/node?q=name:web1",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["total"], 1);
    assert_eq!(result["rows"][0]["name"], "web1");

    let requests = server.requests();
    let search = requests
        .iter()
        .find(|request| request.uri == "/chef/_search")
        .expect("search request");
    let body: Value = serde_json::from_str(&search.body).unwrap();
    assert_eq!(
        body["query"]["bool"]["must"][0]["query_string"]["query"],
        "content:name__=__web1"
    );
    assert_eq!(
        body["query"]["bool"]["filter"][0]["query_string"]["query"],
        format!(
            "+X_CHEF_database_CHEF_X:chef_{} +X_CHEF_type_CHEF_X:node",
            org.id
        )
    );
    assert_eq!(body["from"], 0);
    assert_eq!(body["size"], 1000);
    assert_eq!(
        body["sort"],
        json!([{ "X_CHEF_id_CHEF_X": { "order": "asc" } }])
    );

    // Deletes are propagated to the index as well.
    let (status, _) = request(&app, "DELETE", "/organizations/acme/nodes/web1", b"").await;
    assert_eq!(status, StatusCode::OK);
    let bulk = server.bulk_bodies();
    assert!(
        bulk.iter()
            .any(|body| body.contains("\"delete\"") && body.contains(&node.id)),
        "no delete bulk request captured: {bulk:?}"
    );
}

#[tokio::test]
async fn status_reports_the_selected_search_backend() {
    let server = support::mock_opensearch::start().await;
    let mut config = Config::default();
    config.search.backend = SearchBackend::OpenSearch;
    config.search.url = Some(server.base_url);
    let state = support::state_with_config(config);
    let app = support::app(state);

    let (status, body) = request(&app, "GET", "/_status", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["indexing"]["mode"], "opensearch");
}
