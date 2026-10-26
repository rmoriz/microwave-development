mod support;

use axum::http::StatusCode;
use microwave_config::{Config, SearchBackend};
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

const OTHER_PUBLIC_KEY: &str =
    include_str!("../../microwave-crypto/tests/fixtures/other_key_pub.pem");

async fn request(
    app: &axum::Router,
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
    let value = support::response_json(response).await;
    (status, value)
}

/// Source lines of every `_bulk` index request captured by the mock.
fn indexed_sources(bodies: &[String]) -> Vec<Value> {
    bodies
        .iter()
        .flat_map(|body| body.lines())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("content").is_some())
        .collect()
}

fn documents_for<'a>(sources: &'a [Value], id: &str) -> Vec<&'a Value> {
    sources
        .iter()
        .filter(|source| source["X_CHEF_id_CHEF_X"] == json!(id))
        .collect()
}

#[tokio::test]
async fn client_updates_reindex_the_changed_document() {
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
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let create = json!({"name": "web1", "public_key": support::PUBLIC_KEY});
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        create.to_string().as_bytes(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let client = state
        .sqlite()
        .find_client(&org.id, "web1")
        .unwrap()
        .unwrap();

    let sources = indexed_sources(&server.bulk_bodies());
    let created = documents_for(&sources, &client.id);
    assert_eq!(created.len(), 1, "create did not index the client");
    assert!(created[0]["content"]
        .as_str()
        .unwrap()
        .contains("name__=__web1"));

    // Rename and promote: the same document id must be re-indexed under the
    // new name so a remote search no longer returns the old name.
    let rename = json!({"name": "web2", "validator": true});
    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/web1",
        rename.to_string().as_bytes(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "web2");
    assert_eq!(body["validator"], true);

    let sources = indexed_sources(&server.bulk_bodies());
    let renamed = documents_for(&sources, &client.id);
    assert_eq!(renamed.len(), 2, "rename did not re-index the client");
    let content = renamed.last().unwrap()["content"].as_str().unwrap();
    assert!(content.contains("name__=__web2"), "{content}");
    assert!(content.contains("clientname__=__web2"), "{content}");
    assert!(content.contains("validator__=__true"), "{content}");
    assert!(!content.contains("name__=__web1"), "{content}");

    // Rotating the default key through the v0 update path is also searchable
    // state, so it must be re-indexed as well.
    let rotate = json!({"name": "web2", "public_key": OTHER_PUBLIC_KEY});
    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/web2",
        rotate.to_string().as_bytes(),
        "0",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let sources = indexed_sources(&server.bulk_bodies());
    let rotated = documents_for(&sources, &client.id);
    assert_eq!(rotated.len(), 3, "key rotation did not re-index the client");
    let content = rotated.last().unwrap()["content"].as_str().unwrap();
    assert!(
        content.contains("MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAqP5Z+5nMVSBD1D5ffkMb"),
        "rotated key missing from index document: {content}"
    );

    // Demoting the client updates the validator field without creating a new
    // document id.
    let demote = json!({"name": "web2", "validator": false});
    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/web2",
        demote.to_string().as_bytes(),
        "2",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let sources = indexed_sources(&server.bulk_bodies());
    let demoted = documents_for(&sources, &client.id);
    assert_eq!(
        demoted.len(),
        4,
        "validator change did not re-index the client"
    );
    let content = demoted.last().unwrap()["content"].as_str().unwrap();
    assert!(content.contains("validator__=__false"), "{content}");

    // The id is stable across renames, so replacement is correct and the
    // update path must not delete the freshly indexed document.
    assert!(
        !server
            .bulk_bodies()
            .iter()
            .any(|body| body.contains("\"delete\"") && body.contains(&client.id)),
        "client update deleted the stable document id"
    );
}
