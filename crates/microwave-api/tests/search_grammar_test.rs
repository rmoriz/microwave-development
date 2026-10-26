//! Verifies that Chef/Lucene grammar constructs are rendered into the
//! OpenSearch `content` query in the same form as upstream `chef_lucene.peg`.

mod support;

use axum::http::StatusCode;
use microwave_config::{Config, SearchBackend};
use microwave_crypto::SignVersion;
use serde_json::Value;
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

fn encode(query: &str) -> String {
    let mut encoded = String::new();
    for character in query.chars() {
        match character {
            ' ' => encoded.push_str("%20"),
            '"' => encoded.push_str("%22"),
            '(' => encoded.push_str("%28"),
            ')' => encoded.push_str("%29"),
            '&' => encoded.push_str("%26"),
            '|' => encoded.push_str("%7C"),
            '^' => encoded.push_str("%5E"),
            '?' => encoded.push_str("%3F"),
            other => encoded.push(other),
        }
    }
    encoded
}

#[tokio::test]
async fn renders_chef_grammar_for_opensearch_queries() {
    let server = support::mock_opensearch::start().await;
    let mut config = Config::default();
    config.search.backend = SearchBackend::OpenSearch;
    config.search.url = Some(server.base_url.clone());
    let app = support::app(support::state_with_config(config));

    let (status, _) = request(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let cases = [
        (
            "name:web1 name:web2",
            "(content:name__=__web1 OR content:name__=__web2)",
        ),
        (
            "name:web1 AND name:web2",
            "(content:name__=__web1 AND content:name__=__web2)",
        ),
        (
            "platform:ubuntu && role:web",
            "(content:platform__=__ubuntu AND content:role__=__web)",
        ),
        (
            "platform:ubuntu || platform:debian",
            "(content:platform__=__ubuntu OR content:platform__=__debian)",
        ),
        ("na*:web1", "content:na*__=__web1"),
        ("n?me:web1", "content:n?me__=__web1"),
        ("*:ubuntu", "content:*__=__ubuntu"),
        ("platform:*", "content:platform__=__*"),
        (
            "description:\"hello big world\"",
            "content:\"description__=__hello big world\"",
        ),
        ("name:wev1~1", "content:name__=__wev1~1"),
        ("name:web1^2", "content:name__=__web1^2"),
        ("name:wev1~1^2", "content:name__=__wev1~1^2"),
    ];

    for (query, expected) in cases {
        let (status, result) = request(
            &app,
            "GET",
            &format!("/organizations/acme/search/node?q={}", encode(query)),
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "query {query}: {result}");

        let search = server
            .requests()
            .into_iter()
            .rfind(|request| request.uri == "/chef/_search")
            .expect("no search request captured");
        let body: Value = serde_json::from_str(&search.body).unwrap();
        assert_eq!(
            body["query"]["bool"]["must"][0]["query_string"]["query"], expected,
            "query {query}"
        );
    }
}
