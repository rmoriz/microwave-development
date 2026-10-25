//! Search backend abstraction.
//!
//! [`EmbeddedSearch`] evaluates the parsed query against the flattened
//! documents in the relational store. [`OpenSearchBackend`] sends the query to
//! a remote OpenSearch or Elasticsearch cluster and returns matching IDs,
//! which the API hydrates from the store.

use crate::search::index;
use crate::search::opensearch::{OpenSearchBackend, SearchProvider};
use crate::search::query::Query;
use async_trait::async_trait;
use microwave_config::{SearchBackend as ConfiguredSearchBackend, SearchConfig};
use microwave_store::{MetadataStore, Permission};
use std::sync::Arc;

/// The logical Chef search index being queried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexKind {
    Node,
    Role,
    Environment,
    Client,
    /// Any non-built-in index name is a data bag.
    DataBag(String),
}

impl IndexKind {
    /// Maps an API index name to its kind. Data bag names are validated by the
    /// caller against the store.
    pub fn from_index(index: &str) -> Self {
        match index {
            "node" => Self::Node,
            "role" => Self::Role,
            "environment" => Self::Environment,
            "client" => Self::Client,
            data_bag => Self::DataBag(data_bag.to_string()),
        }
    }

    /// Upstream `X_CHEF_type_CHEF_X` value.
    pub fn object_type(&self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Role => "role",
            Self::Environment => "environment",
            Self::Client => "client",
            Self::DataBag(_) => "data_bag_item",
        }
    }

    /// The upstream filter query fragment selecting one object type.
    pub fn type_filter(&self) -> String {
        match self {
            Self::DataBag(name) => format!("+X_CHEF_type_CHEF_X:data_bag_item +data_bag:{name}"),
            other => format!("+X_CHEF_type_CHEF_X:{}", other.object_type()),
        }
    }
}

/// A backend-agnostic search request.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// Store organization id, formatted as `chef_{org_id}` when sent to
    /// OpenSearch.
    pub org_id: String,
    pub kind: IndexKind,
    pub query: Query,
    pub start: usize,
    pub rows: usize,
    /// Actor whose read ACLs should be applied by backends that can filter
    /// results locally. `None` means the caller is a superuser.
    pub actor_authz_id: Option<String>,
}

/// Matching document IDs in backend order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHits {
    pub total: usize,
    pub ids: Vec<String>,
}

/// A flattened document ready to be sent to the index.
#[derive(Debug, Clone)]
pub struct SearchDocument {
    /// Store identifier, used as the OpenSearch `_id`.
    pub id: String,
    pub org_id: String,
    pub kind: IndexKind,
    /// Whitespace-tokenized `key__=__value` content.
    pub content: String,
}

/// Errors surfaced by a search backend.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("invalid search query")]
    BadRequest,
    #[error("search backend request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("search backend returned status {status} for {url}")]
    Backend { status: u16, url: String },
    #[error("invalid search backend response: {0}")]
    Response(String),
    #[error("invalid search configuration: {0}")]
    Configuration(String),
}

/// Pluggable search implementation.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// Short backend name used by `/_status`.
    fn name(&self) -> &'static str;

    /// Returns matching document IDs and the total hit count.
    async fn search(&self, request: &SearchRequest) -> Result<SearchHits, SearchError>;

    /// Adds or replaces a document.
    async fn index(&self, document: &SearchDocument) -> Result<(), SearchError>;

    /// Deletes a single document.
    async fn delete(&self, org_id: &str, id: &str) -> Result<(), SearchError>;

    /// Deletes every document belonging to an organization.
    async fn delete_index(&self, org_id: &str) -> Result<(), SearchError>;
}

/// In-process backend. The relational store remains the source of truth, so
/// index updates are no-ops and searches read the expanded store objects.
pub struct EmbeddedSearch {
    store: Arc<dyn MetadataStore>,
}

impl EmbeddedSearch {
    pub fn new(store: Arc<dyn MetadataStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl SearchBackend for EmbeddedSearch {
    fn name(&self) -> &'static str {
        "embedded"
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchHits, SearchError> {
        let entries = index::load_entries(self.store.as_ref(), &request.org_id, &request.kind)
            .await
            .map_err(|error| SearchError::Response(error.message))?;
        let mut matched: Vec<&index::SearchEntry> = entries
            .iter()
            .filter(|entry| request.query.matches(&entry.fields))
            .collect();

        if let Some(actor_authz_id) = request.actor_authz_id.as_deref() {
            let mut allowed = Vec::with_capacity(matched.len());
            for entry in matched {
                let permitted = self
                    .store
                    .check_permission(actor_authz_id, &entry.authz_id, Permission::Read)
                    .await
                    .map_err(|error| SearchError::Response(error.to_string()))?;
                if permitted {
                    allowed.push(entry);
                }
            }
            matched = allowed;
        }

        Ok(SearchHits {
            total: matched.len(),
            ids: matched
                .into_iter()
                .skip(request.start)
                .take(request.rows)
                .map(|entry| entry.id.clone())
                .collect(),
        })
    }

    async fn index(&self, _document: &SearchDocument) -> Result<(), SearchError> {
        // The embedded backend reads the store directly.
        Ok(())
    }

    async fn delete(&self, _org_id: &str, _id: &str) -> Result<(), SearchError> {
        // The embedded backend reads the store directly.
        Ok(())
    }

    async fn delete_index(&self, _org_id: &str) -> Result<(), SearchError> {
        // The embedded backend reads the store directly.
        Ok(())
    }
}

/// Selects the configured backend.
///
/// The embedded backend is the default. Remote backends require
/// `search.url`, for example `http://localhost:9200`.
pub fn build_search_backend(
    config: &SearchConfig,
    store: Arc<dyn MetadataStore>,
) -> Result<Arc<dyn SearchBackend>, SearchError> {
    match config.backend {
        ConfiguredSearchBackend::Embedded => Ok(Arc::new(EmbeddedSearch::new(store))),
        ConfiguredSearchBackend::OpenSearch => {
            let backend = OpenSearchBackend::from_config(SearchProvider::OpenSearch, config)?;
            Ok(Arc::new(backend))
        }
        ConfiguredSearchBackend::Elasticsearch => {
            let backend = OpenSearchBackend::from_config(SearchProvider::Elasticsearch, config)?;
            Ok(Arc::new(backend))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use microwave_config::SearchBackend as Configured;

    fn store() -> Arc<dyn MetadataStore> {
        Arc::new(microwave_store::SqliteStore::open_in_memory().unwrap())
    }

    #[test]
    fn defaults_to_the_embedded_backend() {
        let backend = build_search_backend(&SearchConfig::default(), store()).unwrap();
        assert_eq!(backend.name(), "embedded");
        assert_eq!(SearchConfig::default().backend, Configured::Embedded);
    }

    #[test]
    fn selects_opensearch_from_config() {
        let config = SearchConfig {
            backend: Configured::OpenSearch,
            url: Some("http://127.0.0.1:9200/".into()),
        };
        let backend = build_search_backend(&config, store()).unwrap();
        assert_eq!(backend.name(), "opensearch");
    }

    #[test]
    fn selects_elasticsearch_from_config() {
        let config = SearchConfig {
            backend: Configured::Elasticsearch,
            url: Some("http://127.0.0.1:9200".into()),
        };
        let backend = build_search_backend(&config, store()).unwrap();
        assert_eq!(backend.name(), "elasticsearch");
    }

    #[test]
    fn remote_backends_require_a_url() {
        let config = SearchConfig {
            backend: Configured::OpenSearch,
            url: None,
        };
        assert!(matches!(
            build_search_backend(&config, store()),
            Err(SearchError::Configuration(_))
        ));
    }

    #[test]
    fn maps_index_names_and_type_filters() {
        assert_eq!(IndexKind::from_index("node"), IndexKind::Node);
        assert_eq!(
            IndexKind::from_index("secrets"),
            IndexKind::DataBag("secrets".into())
        );
        assert_eq!(IndexKind::Node.type_filter(), "+X_CHEF_type_CHEF_X:node");
        assert_eq!(
            IndexKind::DataBag("secrets".into()).type_filter(),
            "+X_CHEF_type_CHEF_X:data_bag_item +data_bag:secrets"
        );
    }
}
