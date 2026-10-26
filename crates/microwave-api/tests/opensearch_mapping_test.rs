mod support;

use microwave_api::search::{IndexKind, SearchRequest};
use microwave_api::search::{OpenSearchBackend, SearchBackend, SearchProvider};

#[tokio::test]
async fn a_search_provisions_the_chef_index_mapping() {
    let mock = support::mock_opensearch::start().await;
    let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();
    let request = SearchRequest {
        org_id: "org123".into(),
        kind: IndexKind::Node,
        query: microwave_api::search::SearchQuery::MatchAll,
        start: 0,
        rows: 10,
        actor_authz_id: None,
    };
    let _ = backend.search(&request).await;

    let requests = mock.requests();
    let create = requests
        .iter()
        .find(|request| request.method == "PUT" && request.uri == "/chef")
        .expect("search must create the chef index before querying it");
    let body: serde_json::Value = serde_json::from_str(&create.body).unwrap();
    assert_eq!(
        body["mappings"]["properties"]["X_CHEF_id_CHEF_X"]["type"],
        "keyword"
    );
    assert_eq!(body["mappings"]["properties"]["content"]["type"], "text");
    assert_eq!(
        body["settings"]["analysis"]["analyzer"]["default"]["type"],
        "whitespace"
    );
}
