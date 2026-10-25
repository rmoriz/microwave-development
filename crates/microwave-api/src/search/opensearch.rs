//! OpenSearch/Elasticsearch HTTP search backend.
//!
//! Request shapes follow the upstream `chef_opensearch.erl`:
//!
//! - updates and deletes use `POST /_bulk` against the `chef` index
//! - searches use `GET /chef/_search` with `from`/`size`, sorted by
//!   `X_CHEF_id_CHEF_X asc`
//! - the rewritten Chef query is the `must` clause and the organization and
//!   object-type constraints are the `filter` clause

use super::backend::{IndexKind, SearchDocument, SearchError, SearchHits, SearchRequest};
use async_trait::async_trait;
use microwave_config::SearchConfig;
use serde::Deserialize;
use serde_json::{json, Value};

/// Index name used by the upstream Chef server.
const INDEX: &str = "chef";

/// Remote provider flavour. Elasticsearch 7+ shares the OpenSearch request
/// shape used here; the distinction is kept for `/_status` and future tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchProvider {
    OpenSearch,
    Elasticsearch,
}

impl SearchProvider {
    fn name(self) -> &'static str {
        match self {
            Self::OpenSearch => "opensearch",
            Self::Elasticsearch => "elasticsearch",
        }
    }
}

/// HTTP backend for OpenSearch and modern Elasticsearch clusters.
pub struct OpenSearchBackend {
    client: reqwest::Client,
    base_url: String,
    provider: SearchProvider,
}

impl OpenSearchBackend {
    /// Builds a backend from the `[search]` configuration section.
    pub fn from_config(
        provider: SearchProvider,
        config: &SearchConfig,
    ) -> Result<Self, SearchError> {
        let url = config
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| {
                SearchError::Configuration(format!(
                    "search.url is required for the {} backend",
                    provider.name()
                ))
            })?;

        let client = reqwest::Client::builder()
            .build()
            .map_err(SearchError::Transport)?;

        Ok(Self {
            client,
            base_url: url.trim_end_matches('/').to_string(),
            provider,
        })
    }

    /// Builds a backend pointed at an explicit URL. Primarily useful in tests.
    pub fn new(provider: SearchProvider, base_url: impl Into<String>) -> Result<Self, SearchError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(SearchError::Transport)?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            provider,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn filter_query(&self, request: &SearchRequest) -> String {
        format!(
            "+X_CHEF_database_CHEF_X:chef_{} {}",
            request.org_id,
            request.kind.type_filter()
        )
    }

    fn search_body(&self, request: &SearchRequest) -> Value {
        json!({
            "stored_fields": ["_id"],
            "from": request.start,
            "size": request.rows,
            "track_total_hits": true,
            "sort": [{ "X_CHEF_id_CHEF_X": { "order": "asc" } }],
            "query": {
                "bool": {
                    "must": [{
                        "query_string": { "query": request.query.to_upstream_query() }
                    }],
                    "filter": [{
                        "query_string": { "query": self.filter_query(request) }
                    }]
                }
            }
        })
    }

    fn index_body(&self, document: &SearchDocument) -> Result<String, SearchError> {
        let mut source = serde_json::Map::new();
        source.insert("content".into(), json!(document.content));
        source.insert("X_CHEF_id_CHEF_X".into(), json!(document.id));
        source.insert(
            "X_CHEF_database_CHEF_X".into(),
            json!(format!("chef_{}", document.org_id)),
        );
        source.insert(
            "X_CHEF_type_CHEF_X".into(),
            json!(document.kind.object_type()),
        );
        if let IndexKind::DataBag(data_bag) = &document.kind {
            source.insert("data_bag".into(), json!(data_bag));
        }

        let action = json!({
            "index": { "_index": INDEX, "_id": document.id }
        });
        Ok(format!(
            "{}\n{}\n",
            serialize(&action)?,
            serialize(&Value::Object(source))?
        ))
    }

    fn delete_body(id: &str) -> Result<String, SearchError> {
        let action = json!({
            "delete": { "_index": INDEX, "_id": id }
        });
        Ok(format!("{}\n", serialize(&action)?))
    }

    async fn bulk(&self, body: String) -> Result<(), SearchError> {
        let url = self.url("/_bulk");
        let response = self
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/x-ndjson")
            .body(body)
            .send()
            .await?;
        self.check_status(&url, response.status().as_u16())
    }

    fn check_status(&self, url: &str, status: u16) -> Result<(), SearchError> {
        if status == 400 {
            Err(SearchError::BadRequest)
        } else if !(200..300).contains(&status) {
            Err(SearchError::Backend {
                status,
                url: url.to_string(),
            })
        } else {
            Ok(())
        }
    }
}

fn serialize(value: &Value) -> Result<String, SearchError> {
    serde_json::to_string(value).map_err(|error| SearchError::Response(error.to_string()))
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    hits: Hits,
}

#[derive(Debug, Deserialize)]
struct Hits {
    total: TotalHits,
    hits: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TotalHits {
    Exact(usize),
    Object { value: usize },
}

impl TotalHits {
    fn value(&self) -> usize {
        match self {
            Self::Exact(value) => *value,
            Self::Object { value } => *value,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    #[serde(rename = "_id")]
    id: String,
}

#[async_trait]
impl super::backend::SearchBackend for OpenSearchBackend {
    fn name(&self) -> &'static str {
        self.provider.name()
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchHits, SearchError> {
        let url = self.url(&format!("/{INDEX}/_search"));
        let response = self
            .client
            .get(&url)
            .json(&self.search_body(request))
            .send()
            .await?;
        let status = response.status().as_u16();
        self.check_status(&url, status)?;

        let response: SearchResponse = response.json().await.map_err(|error| {
            SearchError::Response(format!("could not decode search response: {error}"))
        })?;

        Ok(SearchHits {
            total: response.hits.total.value(),
            ids: response.hits.hits.into_iter().map(|hit| hit.id).collect(),
        })
    }

    async fn index(&self, document: &SearchDocument) -> Result<(), SearchError> {
        self.bulk(self.index_body(document)?).await
    }

    async fn delete(&self, _org_id: &str, id: &str) -> Result<(), SearchError> {
        self.bulk(Self::delete_body(id)?).await
    }

    async fn delete_index(&self, org_id: &str) -> Result<(), SearchError> {
        let url = self.url(&format!("/{INDEX}/_delete_by_query"));
        let body = json!({
            "query": {
                "query_string": {
                    "query": format!("X_CHEF_database_CHEF_X:chef_{org_id}")
                }
            }
        });
        let response = self.client.post(&url).json(&body).send().await?;
        self.check_status(&url, response.status().as_u16())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::backend::SearchBackend;
    use crate::search::query::Query;
    use axum::extract::{Request, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::Router;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    struct CapturedRequest {
        method: String,
        uri: String,
        body: String,
    }

    #[derive(Clone)]
    struct MockState {
        status: u16,
        body: Value,
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
    }

    struct MockServer {
        base_url: String,
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
    }

    impl MockServer {
        fn requests(&self) -> Vec<CapturedRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    async fn handler(State(state): State<MockState>, request: Request) -> Response {
        let method = request.method().to_string();
        let uri = request.uri().to_string();
        let body = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .unwrap();
        state.requests.lock().unwrap().push(CapturedRequest {
            method,
            uri,
            body: String::from_utf8_lossy(&body).to_string(),
        });

        let status = StatusCode::from_u16(state.status).unwrap();
        (status, axum::Json(state.body.clone())).into_response()
    }

    async fn mock(status: u16, body: Value) -> MockServer {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = MockState {
            status,
            body,
            requests: requests.clone(),
        };
        let app = Router::new().fallback(handler).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        MockServer {
            base_url: format!("http://{address}"),
            requests,
        }
    }

    fn request(query: &str) -> SearchRequest {
        SearchRequest {
            org_id: "acme".into(),
            kind: IndexKind::Node,
            query: Query::parse(query).unwrap(),
            start: 5,
            rows: 10,
            actor_authz_id: None,
        }
    }

    fn document() -> SearchDocument {
        SearchDocument {
            id: "node-guid".into(),
            org_id: "acme".into(),
            kind: IndexKind::Node,
            content: "name__=__web1 platform__=__ubuntu ".into(),
        }
    }

    #[tokio::test]
    async fn search_uses_chef_query_filters_pagination_and_sort() {
        let mock = mock(
            200,
            json!({
                "hits": {
                    "total": { "value": 2, "relation": "eq" },
                    "hits": [{ "_id": "one" }, { "_id": "two" }]
                }
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let hits = backend.search(&request("name:web1")).await.unwrap();
        assert_eq!(
            hits,
            SearchHits {
                total: 2,
                ids: vec!["one".into(), "two".into()]
            }
        );

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].uri, "/chef/_search");
        let body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["stored_fields"], json!(["_id"]));
        assert_eq!(body["from"], 5);
        assert_eq!(body["size"], 10);
        assert_eq!(body["track_total_hits"], true);
        assert_eq!(
            body["sort"],
            json!([{ "X_CHEF_id_CHEF_X": { "order": "asc" } }])
        );
        assert_eq!(
            body["query"]["bool"]["must"][0]["query_string"]["query"],
            "content:name__=__web1"
        );
        assert_eq!(
            body["query"]["bool"]["filter"][0]["query_string"]["query"],
            "+X_CHEF_database_CHEF_X:chef_acme +X_CHEF_type_CHEF_X:node"
        );
    }

    #[tokio::test]
    async fn data_bag_searches_add_the_data_bag_filter() {
        let mock = mock(200, json!({ "hits": { "total": 0, "hits": [] } })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();
        let mut request = request("engine:postgres");
        request.kind = IndexKind::DataBag("secrets".into());

        backend.search(&request).await.unwrap();

        let requests = mock.requests();
        let body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(
            body["query"]["bool"]["filter"][0]["query_string"]["query"],
            "+X_CHEF_database_CHEF_X:chef_acme +X_CHEF_type_CHEF_X:data_bag_item +data_bag:secrets"
        );
    }

    #[tokio::test]
    async fn indexing_sends_a_bulk_index_action() {
        let mock = mock(200, json!({ "errors": false })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        backend.index(&document()).await.unwrap();

        let requests = mock.requests();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].uri, "/_bulk");
        let lines: Vec<&str> = requests[0].body.lines().collect();
        assert_eq!(lines.len(), 2);
        let action: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(
            action,
            json!({ "index": { "_index": "chef", "_id": "node-guid" } })
        );
        let source: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(source["content"], "name__=__web1 platform__=__ubuntu ");
        assert_eq!(source["X_CHEF_id_CHEF_X"], "node-guid");
        assert_eq!(source["X_CHEF_database_CHEF_X"], "chef_acme");
        assert_eq!(source["X_CHEF_type_CHEF_X"], "node");
        assert!(source.get("data_bag").is_none());
    }

    #[tokio::test]
    async fn data_bag_indexing_sets_the_data_bag_field() {
        let mock = mock(200, json!({ "errors": false })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();
        let mut document = document();
        document.kind = IndexKind::DataBag("secrets".into());

        backend.index(&document).await.unwrap();

        let requests = mock.requests();
        let lines: Vec<&str> = requests[0].body.lines().collect();
        let source: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(source["X_CHEF_type_CHEF_X"], "data_bag_item");
        assert_eq!(source["data_bag"], "secrets");
    }

    #[tokio::test]
    async fn deleting_sends_a_bulk_delete_action() {
        let mock = mock(200, json!({ "errors": false })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        backend.delete("acme", "node-guid").await.unwrap();

        let requests = mock.requests();
        assert_eq!(requests[0].uri, "/_bulk");
        let action: Value = serde_json::from_str(requests[0].body.trim()).unwrap();
        assert_eq!(
            action,
            json!({ "delete": { "_index": "chef", "_id": "node-guid" } })
        );
    }

    #[tokio::test]
    async fn deleting_an_organization_uses_delete_by_query() {
        let mock = mock(200, json!({ "deleted": 3 })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        backend.delete_index("acme").await.unwrap();

        let requests = mock.requests();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].uri, "/chef/_delete_by_query");
        let body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(
            body["query"]["query_string"]["query"],
            "X_CHEF_database_CHEF_X:chef_acme"
        );
    }

    #[tokio::test]
    async fn maps_400_to_bad_request_and_other_errors_to_backend_errors() {
        let server = mock(400, json!({ "error": "bad query" })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &server.base_url).unwrap();
        assert!(matches!(
            backend.search(&request("name:web1")).await,
            Err(SearchError::BadRequest)
        ));

        let server = mock(500, json!({ "error": "boom" })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &server.base_url).unwrap();
        match backend.search(&request("name:web1")).await {
            Err(SearchError::Backend { status: 500, .. }) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn accepts_numeric_total_hits() {
        let mock = mock(
            200,
            json!({ "hits": { "total": 1, "hits": [{ "_id": "one" }] } }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let hits = backend.search(&request("name:web1")).await.unwrap();
        assert_eq!(hits.total, 1);
        assert_eq!(hits.ids, vec!["one".to_string()]);
    }
}
