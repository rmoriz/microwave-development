//! OpenSearch/Elasticsearch HTTP search backend.
//!
//! Request shapes follow the upstream `chef_opensearch.erl`:
//!
//! - updates and deletes use `POST /_bulk` against the `chef` index
//! - searches use `GET /chef/_search` with `from`/`size`, sorted by
//!   `X_CHEF_id_CHEF_X asc`
//! - the rewritten Chef query is the `must` clause and the organization and
//!   object-type constraints are the `filter` clause

use super::backend::{
    IndexKind, IndexOutcome, SearchDocument, SearchError, SearchHits, SearchRequest,
};
use async_trait::async_trait;
use microwave_config::SearchConfig;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Index name used by the upstream Chef server.
const INDEX: &str = "chef";

/// Maximum documents per `_bulk` request during reindexing.
const BULK_CHUNK_SIZE: usize = 500;

/// Fallback request timeout for backends built without a `SearchConfig`.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Upper bound for the `/_status` liveness probe. It is intentionally much
/// shorter than normal request deadlines so a wedged cluster cannot stall the
/// health endpoint.
const HEALTH_CHECK_TIMEOUT_MS: u64 = 1_500;

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

        if config.timeout_ms == 0 {
            return Err(SearchError::Configuration(
                "search.timeout_ms must be greater than zero".to_string(),
            ));
        }

        let client = client_with_timeout(config.timeout_ms)?;

        Ok(Self {
            client,
            base_url: url.trim_end_matches('/').to_string(),
            provider,
        })
    }

    /// Builds a backend pointed at an explicit URL. Primarily useful in tests.
    pub fn new(provider: SearchProvider, base_url: impl Into<String>) -> Result<Self, SearchError> {
        let client = client_with_timeout(DEFAULT_TIMEOUT_MS)?;
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

    async fn bulk(&self, body: String, allow_not_found: bool) -> Result<usize, SearchError> {
        let url = self.url("/_bulk");
        let response = self
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/x-ndjson")
            .body(body)
            .send()
            .await?;
        self.check_status(&url, response.status().as_u16())?;
        let response: BulkResponse = response.json().await.map_err(|error| {
            SearchError::Response(format!("could not decode bulk response: {error}"))
        })?;
        bulk_acknowledged(response, allow_not_found)
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

fn client_with_timeout(timeout_ms: u64) -> Result<reqwest::Client, SearchError> {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()
        .map_err(SearchError::Transport)
}

fn serialize(value: &Value) -> Result<String, SearchError> {
    serde_json::to_string(value).map_err(|error| SearchError::Response(error.to_string()))
}

#[derive(Debug, Deserialize)]
struct BulkResponse {
    /// OpenSearch sets this when any item failed.
    #[serde(default)]
    errors: bool,
    #[serde(default)]
    items: Vec<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct DeleteByQueryResponse {
    #[serde(default)]
    timed_out: bool,
    /// Number of documents matched by the query.
    #[serde(default)]
    total: Option<usize>,
    #[serde(default)]
    deleted: usize,
    #[serde(default)]
    version_conflicts: usize,
    #[serde(default)]
    noops: usize,
    #[serde(default)]
    failures: Vec<Value>,
}

impl DeleteByQueryResponse {
    /// Rejects a response that did not delete every matched document.
    ///
    /// `_delete_by_query` can return HTTP 200 while timing out, reporting
    /// failures, losing version conflicts, or deleting only part of the
    /// matched set. A purge must not be counted as complete unless the whole
    /// organization was removed.
    fn ensure_complete(&self) -> Result<(), SearchError> {
        if self.timed_out {
            return Err(SearchError::Response(format!(
                "delete-by-query timed out after deleting {} of {} documents",
                self.deleted,
                self.total.map_or_else(
                    || "an unknown number".to_string(),
                    |total| total.to_string()
                )
            )));
        }

        if !self.failures.is_empty() {
            let failures = self.failures.iter().map(Value::to_string).collect();
            return Err(SearchError::Response(format!(
                "delete-by-query failed for {} document(s): {}",
                self.failures.len(),
                failure_detail(failures)
            )));
        }

        if self.version_conflicts > 0 {
            return Err(SearchError::Response(format!(
                "delete-by-query reported {} version conflict(s); not every document was deleted",
                self.version_conflicts
            )));
        }

        if let Some(total) = self.total {
            let processed = self.deleted + self.noops;
            if processed < total {
                return Err(SearchError::Response(format!(
                    "delete-by-query deleted {} and skipped {} of {} documents; not every document was deleted",
                    self.deleted, self.noops, total
                )));
            }
        }

        Ok(())
    }
}

/// Returns true when a 404 body identifies a missing index rather than a
/// genuinely broken endpoint or query.
fn is_index_not_found(body: &str) -> bool {
    body.contains("index_not_found_exception")
}

/// Parse a `_bulk` response and return the number of acknowledged items.
///
/// `_bulk` can return HTTP 200 while individual items fail. Every item is
/// checked for an `error` member and a non-2xx `status`; any failure aborts the
/// write instead of silently reporting success. Only items with a 2xx status
/// and no error are counted.
///
/// `allow_not_found` is used for deletes: a missing document returns an item
/// status of 404 and should not produce a warning. Index writes pass `false`,
/// so a 404 there is still a failure.
fn bulk_acknowledged(response: BulkResponse, allow_not_found: bool) -> Result<usize, SearchError> {
    let mut acknowledged = 0;
    let mut failures = Vec::new();

    for (index, item) in response.items.iter().enumerate() {
        let Some(result) = item.as_object().and_then(|object| object.values().next()) else {
            failures.push(format!("item {index}: malformed bulk item"));
            continue;
        };
        let status = result.get("status").and_then(Value::as_u64);
        let id = result.get("_id").and_then(Value::as_str);
        if let Some(error) = result.get("error").filter(|error| !error.is_null()) {
            failures.push(format!(
                "item {index}{}: {}",
                id.map(|id| format!(" ({id})")).unwrap_or_default(),
                bulk_error_summary(error)
            ));
        } else if allow_not_found && status == Some(404) {
            // Deleting an already-missing document is an expected no-op.
        } else if !status.is_some_and(|status| (200..300).contains(&status)) {
            failures.push(format!(
                "item {index}{}: status {}",
                id.map(|id| format!(" ({id})")).unwrap_or_default(),
                status.map_or_else(|| "missing".to_string(), |status| status.to_string())
            ));
        } else {
            acknowledged += 1;
        }
    }

    if response.errors || !failures.is_empty() {
        let detail = failure_detail(failures);
        return Err(SearchError::Response(format!(
            "bulk request contained failed items: {detail}"
        )));
    }
    Ok(acknowledged)
}

fn bulk_error_summary(error: &Value) -> String {
    let Some(object) = error.as_object() else {
        return error.to_string();
    };
    let error_type = object.get("type").and_then(Value::as_str);
    let reason = object.get("reason").and_then(Value::as_str);
    match (error_type, reason) {
        (Some(error_type), Some(reason)) => format!("{error_type}: {reason}"),
        (Some(error_type), None) => error_type.to_string(),
        (None, Some(reason)) => reason.to_string(),
        (None, None) => error.to_string(),
    }
}

fn failure_detail(mut failures: Vec<String>) -> String {
    if failures.is_empty() {
        return "the response reported errors=true without item details".to_string();
    }
    const MAX_DETAILS: usize = 3;
    let remaining = failures.len().saturating_sub(MAX_DETAILS);
    failures.truncate(MAX_DETAILS);
    let mut detail = failures.join("; ");
    if remaining > 0 {
        detail.push_str(&format!("; and {remaining} more"));
    }
    detail
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

    async fn health_check(&self) -> Result<(), SearchError> {
        let url = self.url("/_cluster/health");
        let response = self
            .client
            .get(&url)
            .timeout(Duration::from_millis(HEALTH_CHECK_TIMEOUT_MS))
            .send()
            .await?;
        self.check_status(&url, response.status().as_u16())?;

        // A red cluster still answers with HTTP 200 by default. Yellow is
        // expected on a single-node cluster while replicas are unassigned, but
        // red means primary shards are unavailable and search is degraded.
        let health: Value = response.json().await.map_err(|error| {
            SearchError::Response(format!("invalid cluster health response: {error}"))
        })?;
        if health.get("status").and_then(Value::as_str) == Some("red") {
            return Err(SearchError::Response("cluster health is red".to_string()));
        }
        Ok(())
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

    async fn index(&self, document: &SearchDocument) -> Result<IndexOutcome, SearchError> {
        let acknowledged = self.bulk(self.index_body(document)?, false).await?;
        if acknowledged == 0 {
            return Err(SearchError::Response(
                "bulk response acknowledged no documents".to_string(),
            ));
        }
        Ok(IndexOutcome { acknowledged })
    }

    async fn index_many(&self, documents: &[SearchDocument]) -> Result<usize, SearchError> {
        let mut acknowledged = 0;
        for chunk in documents.chunks(BULK_CHUNK_SIZE) {
            let mut body = String::new();
            for document in chunk {
                body.push_str(&self.index_body(document)?);
            }

            let chunk_acknowledged = self.bulk(body, false).await?;
            if chunk_acknowledged != chunk.len() {
                return Err(SearchError::Response(format!(
                    "bulk request acknowledged {chunk_acknowledged} of {} documents",
                    chunk.len()
                )));
            }
            acknowledged += chunk_acknowledged;
        }
        Ok(acknowledged)
    }

    async fn refresh(&self) -> Result<(), SearchError> {
        let url = self.url(&format!("/{INDEX}/_refresh"));
        let response = self.client.post(&url).send().await?;
        let status = response.status().as_u16();
        if status == 404 && is_index_not_found(&response.text().await.unwrap_or_default()) {
            // Nothing has created the index yet, so there is nothing to
            // refresh. This happens after purging and reindexing an empty
            // organization on a fresh cluster.
            return Ok(());
        }
        self.check_status(&url, status)
    }

    async fn delete(&self, _org_id: &str, id: &str) -> Result<(), SearchError> {
        self.bulk(Self::delete_body(id)?, true).await.map(|_| ())
    }

    async fn delete_index(&self, org_id: &str) -> Result<(), SearchError> {
        let url = self.url(&format!("/{INDEX}/_delete_by_query"));
        // A term filter treats the organization id as an exact value. Using a
        // raw query_string would let punctuation or operator characters in an
        // id change the query.
        let body = json!({
            "query": {
                "term": {
                    "X_CHEF_database_CHEF_X": format!("chef_{org_id}")
                }
            }
        });
        let response = self.client.post(&url).json(&body).send().await?;
        let status = response.status().as_u16();
        if status == 404 {
            if is_index_not_found(&response.text().await.unwrap_or_default()) {
                return Ok(());
            }
            return Err(SearchError::Backend { status, url });
        }
        self.check_status(&url, status)?;
        let response: DeleteByQueryResponse = response.json().await.map_err(|error| {
            SearchError::Response(format!(
                "could not decode delete-by-query response: {error}"
            ))
        })?;
        response.ensure_complete()
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

    type Responder = Arc<dyn Fn(&CapturedRequest) -> (u16, Value) + Send + Sync>;

    #[derive(Clone)]
    struct MockState {
        responder: Responder,
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
        let captured = CapturedRequest {
            method,
            uri,
            body: String::from_utf8_lossy(&body).to_string(),
        };
        state.requests.lock().unwrap().push(captured.clone());

        let (status, body) = (state.responder)(&captured);
        let status = StatusCode::from_u16(status).unwrap();
        (status, axum::Json(body)).into_response()
    }

    async fn mock(status: u16, body: Value) -> MockServer {
        mock_with(move |_| (status, body.clone())).await
    }

    async fn mock_with<F>(responder: F) -> MockServer
    where
        F: Fn(&CapturedRequest) -> (u16, Value) + Send + Sync + 'static,
    {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = MockState {
            responder: Arc::new(responder),
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

    /// Builds an OpenSearch bulk response by echoing the ids in an NDJSON
    /// action/source request. `fail_id` marks one item as rejected.
    fn bulk_response(body: &str, fail_id: Option<&str>) -> Value {
        let mut errors = false;
        let mut items = Vec::new();
        for pair in body.lines().collect::<Vec<_>>().chunks(2) {
            let Some(action) = pair
                .first()
                .and_then(|line| serde_json::from_str::<Value>(line).ok())
            else {
                continue;
            };
            let Some((operation, metadata)) =
                action.as_object().and_then(|object| object.iter().next())
            else {
                continue;
            };
            let id = metadata
                .get("_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if fail_id == Some(id) {
                errors = true;
                items.push(json!({
                    operation.clone(): {
                        "_index": INDEX,
                        "_id": id,
                        "status": 400,
                        "error": {
                            "type": "mapper_parsing_exception",
                            "reason": "failed to parse field [content]"
                        }
                    }
                }));
            } else {
                items.push(json!({
                    operation.clone(): {
                        "_index": INDEX,
                        "_id": id,
                        "status": 201
                    }
                }));
            }
        }
        json!({ "errors": errors, "items": items })
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

    fn document_with_id(id: &str) -> SearchDocument {
        SearchDocument {
            id: id.into(),
            ..document()
        }
    }

    fn bulk_requests(mock: &MockServer) -> Vec<CapturedRequest> {
        mock.requests()
            .into_iter()
            .filter(|request| request.uri == "/_bulk")
            .collect()
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
        let mock = mock(
            200,
            json!({
                "errors": false,
                "items": [{ "index": { "_index": "chef", "_id": "node-guid", "status": 201 } }]
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let outcome = backend.index(&document()).await.unwrap();
        assert_eq!(outcome.acknowledged, 1);

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
    async fn bulk_item_errors_fail_indexing_even_with_http_200() {
        let mock = mock(
            200,
            json!({
                "errors": true,
                "items": [{
                    "index": {
                        "_index": "chef",
                        "_id": "node-guid",
                        "status": 400,
                        "error": {
                            "type": "mapper_parsing_exception",
                            "reason": "failed to parse field [content]"
                        }
                    }
                }]
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let error = backend.index(&document()).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("bulk request contained failed items"));
        assert!(message.contains("mapper_parsing_exception"));
        assert!(message.contains("failed to parse field"));
        assert!(message.contains("node-guid"));
    }

    #[tokio::test]
    async fn bulk_item_statuses_fail_indexing_even_when_errors_is_false() {
        let mock = mock(
            200,
            json!({
                "errors": false,
                "items": [{
                    "index": {
                        "_index": "chef",
                        "_id": "node-guid",
                        "status": 429
                    }
                }]
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let error = backend.index(&document()).await.unwrap_err();
        assert!(error.to_string().contains("status 429"));
    }

    #[tokio::test]
    async fn bulk_errors_flag_without_item_details_is_still_a_failure() {
        let mock = mock(200, json!({ "errors": true })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let error = backend.index(&document()).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("errors=true without item details"));
    }

    #[tokio::test]
    async fn a_bulk_response_with_no_items_is_not_an_acknowledged_write() {
        let mock = mock(200, json!({ "errors": false, "items": [] })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let error = backend.index(&document()).await.unwrap_err();
        assert!(error.to_string().contains("acknowledged no documents"));
    }

    #[tokio::test]
    async fn data_bag_indexing_sets_the_data_bag_field() {
        let mock = mock(
            200,
            json!({
                "errors": false,
                "items": [{ "index": { "_index": "chef", "_id": "node-guid", "status": 201 } }]
            }),
        )
        .await;
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
    async fn index_many_chunks_bulk_requests_and_counts_acknowledged_documents() {
        let mock = mock_with(|request| {
            if request.uri == "/_bulk" {
                (200, bulk_response(&request.body, None))
            } else {
                (200, json!({}))
            }
        })
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();
        let documents: Vec<SearchDocument> = (0..BULK_CHUNK_SIZE * 2 + 1)
            .map(|index| document_with_id(&format!("node-{index}")))
            .collect();

        let acknowledged = backend.index_many(&documents).await.unwrap();

        assert_eq!(acknowledged, documents.len());
        let requests = bulk_requests(&mock);
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].body.lines().count(), BULK_CHUNK_SIZE * 2);
        assert_eq!(requests[1].body.lines().count(), BULK_CHUNK_SIZE * 2);
        assert_eq!(requests[2].body.lines().count(), 2);
        assert!(requests[0].body.contains("\"node-0\""));
        assert!(requests[2]
            .body
            .contains(&format!("node-{}", documents.len() - 1)));
    }

    #[tokio::test]
    async fn index_many_aborts_on_a_rejected_bulk_item() {
        let mock = mock_with(|request| (200, bulk_response(&request.body, Some("node-1")))).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();
        let documents: Vec<SearchDocument> = (0..3)
            .map(|index| document_with_id(&format!("node-{index}")))
            .collect();

        let error = backend.index_many(&documents).await.unwrap_err();

        let message = error.to_string();
        assert!(message.contains("bulk request contained failed items"));
        assert!(message.contains("mapper_parsing_exception"));
        assert!(message.contains("node-1"));
        // The chunk fails as a whole, so no further bulk request is sent.
        assert_eq!(bulk_requests(&mock).len(), 1);
    }

    #[tokio::test]
    async fn index_many_stops_after_a_later_chunk_fails() {
        let mock = mock_with(|request| (200, bulk_response(&request.body, Some("node-600")))).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();
        let documents: Vec<SearchDocument> = (0..BULK_CHUNK_SIZE * 2 + 200)
            .map(|index| document_with_id(&format!("node-{index}")))
            .collect();

        let error = backend.index_many(&documents).await.unwrap_err();

        assert!(error
            .to_string()
            .contains("bulk request contained failed items"));
        // The first chunk was accepted, the second failed, and the third was
        // never attempted.
        assert_eq!(bulk_requests(&mock).len(), 2);
    }

    #[tokio::test]
    async fn refresh_posts_to_the_chef_refresh_endpoint() {
        let mock = mock(200, json!({ "_shards": { "total": 1 } })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        backend.refresh().await.unwrap();

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].uri, "/chef/_refresh");
    }

    #[tokio::test]
    async fn refresh_failures_are_reported() {
        let mock = mock(503, json!({ "error": "unavailable" })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        match backend.refresh().await {
            Err(SearchError::Backend { status: 503, .. }) => {}
            other => panic!("unexpected result: {other:?}"),
        }
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
    async fn deleting_a_missing_document_is_not_a_warning() {
        let mock = mock(
            200,
            json!({
                "errors": false,
                "items": [{
                    "delete": {
                        "_index": "chef",
                        "_id": "node-guid",
                        "result": "not_found",
                        "status": 404
                    }
                }]
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        backend.delete("acme", "node-guid").await.unwrap();
    }

    #[tokio::test]
    async fn an_index_item_with_status_404_is_still_a_failure() {
        let mock = mock(
            200,
            json!({
                "errors": false,
                "items": [{
                    "index": {
                        "_index": "chef",
                        "_id": "node-guid",
                        "status": 404
                    }
                }]
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let error = backend.index(&document()).await.unwrap_err();
        assert!(error.to_string().contains("status 404"));
    }

    #[tokio::test]
    async fn deleting_an_organization_uses_a_term_filter() {
        let mock = mock(
            200,
            json!({
                "timed_out": false,
                "total": 3,
                "deleted": 3,
                "version_conflicts": 0,
                "noops": 0,
                "failures": []
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        backend.delete_index("acme").await.unwrap();

        let requests = mock.requests();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].uri, "/chef/_delete_by_query");
        let body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["query"]["term"]["X_CHEF_database_CHEF_X"], "chef_acme");
        assert!(body["query"].get("query_string").is_none());
    }

    #[tokio::test]
    async fn purge_of_a_missing_index_is_a_no_op() {
        let mock = mock(
            404,
            json!({
                "error": {
                    "type": "index_not_found_exception",
                    "reason": "no such index [chef]"
                },
                "status": 404
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        backend.delete_index("acme").await.unwrap();

        let requests = mock.requests();
        assert_eq!(requests[0].uri, "/chef/_delete_by_query");
    }

    #[tokio::test]
    async fn a_generic_404_purge_is_still_a_backend_error() {
        let mock = mock(404, json!({ "error": "not found" })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        match backend.delete_index("acme").await {
            Err(SearchError::Backend { status: 404, .. }) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_by_query_failures_abort_the_purge() {
        let mock = mock(
            200,
            json!({
                "timed_out": false,
                "total": 2,
                "deleted": 1,
                "version_conflicts": 0,
                "noops": 0,
                "failures": [{
                    "id": "node-guid",
                    "cause": {
                        "type": "cluster_block_exception",
                        "reason": "index is read-only"
                    }
                }]
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let error = backend.delete_index("acme").await.unwrap_err().to_string();
        assert!(error.contains("delete-by-query failed for 1 document(s)"));
        assert!(error.contains("cluster_block_exception"));
        assert!(error.contains("index is read-only"));
    }

    #[tokio::test]
    async fn a_timed_out_delete_by_query_aborts_the_purge() {
        let mock = mock(
            200,
            json!({
                "timed_out": true,
                "total": 5,
                "deleted": 4,
                "version_conflicts": 0,
                "noops": 0,
                "failures": []
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let error = backend.delete_index("acme").await.unwrap_err().to_string();
        assert!(error.contains("timed out after deleting 4 of 5 documents"));
    }

    #[tokio::test]
    async fn an_incomplete_delete_by_query_aborts_the_purge() {
        let mock = mock(
            200,
            json!({
                "timed_out": false,
                "total": 5,
                "deleted": 3,
                "version_conflicts": 0,
                "noops": 1,
                "failures": []
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        let error = backend.delete_index("acme").await.unwrap_err().to_string();
        assert!(error.contains("deleted 3 and skipped 1 of 5 documents"));
    }

    #[tokio::test]
    async fn refresh_of_a_missing_index_is_a_no_op() {
        let mock = mock(
            404,
            json!({
                "error": {
                    "type": "index_not_found_exception",
                    "reason": "no such index [chef]"
                },
                "status": 404
            }),
        )
        .await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &mock.base_url).unwrap();

        backend.refresh().await.unwrap();
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_check_uses_cluster_health_and_reports_backend_failures() {
        let server = mock(200, json!({ "status": "green" })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &server.base_url).unwrap();
        backend.health_check().await.unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].uri, "/_cluster/health");

        let server = mock(200, json!({ "status": "yellow" })).await;
        let backend =
            OpenSearchBackend::new(SearchProvider::Elasticsearch, &server.base_url).unwrap();
        backend.health_check().await.unwrap();

        let server = mock(200, json!({ "status": "red" })).await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, &server.base_url).unwrap();
        match backend.health_check().await {
            Err(SearchError::Response(message)) => {
                assert!(message.contains("red"), "unexpected detail: {message}");
            }
            other => panic!("unexpected result: {other:?}"),
        }

        let server = mock(503, json!({ "status": "red" })).await;
        let backend =
            OpenSearchBackend::new(SearchProvider::Elasticsearch, &server.base_url).unwrap();
        match backend.health_check().await {
            Err(SearchError::Backend { status: 503, url }) => {
                assert_eq!(url, format!("{}/_cluster/health", server.base_url));
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    async fn raw_health_server(status: u16, body: &'static str) -> String {
        use axum::http::StatusCode;
        use axum::Router;

        let app = Router::new().fallback(move || async move {
            (
                StatusCode::from_u16(status).unwrap(),
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_check_rejects_a_malformed_cluster_health_body() {
        let base_url = raw_health_server(200, "not json").await;
        let backend = OpenSearchBackend::new(SearchProvider::OpenSearch, base_url).unwrap();
        match backend.health_check().await {
            Err(SearchError::Response(message)) => {
                assert!(
                    message.contains("invalid cluster health response"),
                    "unexpected detail: {message}"
                );
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
