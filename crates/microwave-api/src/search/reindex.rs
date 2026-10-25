//! Full organization reindexing for remote search backends.
//!
//! The relational store is authoritative. A remote OpenSearch/Elasticsearch
//! cluster holds derived documents, and switching to it (or losing and
//! restoring the cluster) leaves no documents for objects that were written
//! while the remote backend was not active. This module rebuilds those
//! documents from the store.
//!
//! Scope and limits:
//!
//! - every organization is visited, or just the requested one;
//! - nodes, roles, environments, clients, and every data bag item are indexed;
//! - documents for deleted objects are **not** removed. Reindexing adds or
//!   replaces documents by store id; it does not delete a remote index first,
//!   because an interrupted delete-then-rebuild would make the installation
//!   unsearchable. Purge stale documents by deleting the `chef` index while
//!   the server is stopped, then run the reindex.

use super::backend::{IndexKind, SearchBackend, SearchDocument, SearchError};
use super::index;
use crate::ApiError;
use microwave_store::{MetadataStore, Org};

/// Summary of a completed reindex run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReindexReport {
    /// Number of organizations visited.
    pub organizations: usize,
    /// Total documents submitted to the backend.
    pub documents: usize,
    pub nodes: usize,
    pub roles: usize,
    pub environments: usize,
    pub clients: usize,
    pub data_bags: usize,
    pub data_bag_items: usize,
}

impl ReindexReport {
    fn record(&mut self, kind: &IndexKind, count: usize) {
        self.documents += count;
        match kind {
            IndexKind::Node => self.nodes += count,
            IndexKind::Role => self.roles += count,
            IndexKind::Environment => self.environments += count,
            IndexKind::Client => self.clients += count,
            IndexKind::DataBag(_) => self.data_bag_items += count,
        }
    }
}

/// Rebuild search documents for one organization or for every organization.
///
/// The caller chooses the backend. Indexing through the embedded backend is
/// harmless and reports the relational document counts, because the embedded
/// backend ignores writes and reads the store directly. Remote backends are
/// sent one document at a time through [`SearchBackend::index`].
pub async fn reindex(
    backend: &dyn SearchBackend,
    store: &dyn MetadataStore,
    organization: Option<&str>,
) -> Result<ReindexReport, SearchError> {
    let organizations = match organization {
        Some(name) => {
            let org = store
                .find_org(name)
                .await
                .map_err(|error| SearchError::Response(error.to_string()))?
                .ok_or_else(|| {
                    SearchError::Response(format!("organization {name} does not exist"))
                })?;
            vec![org]
        }
        None => store
            .list_orgs()
            .await
            .map_err(|error| SearchError::Response(error.to_string()))?,
    };

    let mut report = ReindexReport {
        organizations: organizations.len(),
        ..ReindexReport::default()
    };

    for org in organizations {
        reindex_org(backend, store, &org, &mut report).await?;
    }
    Ok(report)
}

async fn reindex_org(
    backend: &dyn SearchBackend,
    store: &dyn MetadataStore,
    org: &Org,
    report: &mut ReindexReport,
) -> Result<(), SearchError> {
    for kind in [
        IndexKind::Node,
        IndexKind::Role,
        IndexKind::Environment,
        IndexKind::Client,
    ] {
        let documents = documents_for_kind(store, org, kind.clone()).await?;
        report.record(&kind, documents.len());
        index_documents(backend, documents).await?;
    }

    let data_bags = store
        .list_data_bags(&org.id)
        .await
        .map_err(|error| SearchError::Response(error.to_string()))?;
    report.data_bags += data_bags.len();
    for data_bag in data_bags {
        let kind = IndexKind::DataBag(data_bag.name);
        let documents = documents_for_kind(store, org, kind.clone()).await?;
        report.record(&kind, documents.len());
        index_documents(backend, documents).await?;
    }
    Ok(())
}

async fn documents_for_kind(
    store: &dyn MetadataStore,
    org: &Org,
    kind: IndexKind,
) -> Result<Vec<SearchDocument>, SearchError> {
    let data_bag = match &kind {
        IndexKind::DataBag(name) => Some(name.as_str()),
        _ => None,
    };
    let object_type = kind.object_type();
    let entries = index::load_entries(store, &org.id, &kind)
        .await
        .map_err(api_error)?;
    Ok(entries
        .into_iter()
        .map(|entry| index::document_for_entry(&org.id, &kind, object_type, data_bag, entry))
        .collect())
}

async fn index_documents(
    backend: &dyn SearchBackend,
    documents: Vec<SearchDocument>,
) -> Result<(), SearchError> {
    for document in documents {
        backend.index(&document).await?;
    }
    Ok(())
}

fn api_error(error: ApiError) -> SearchError {
    SearchError::Response(error.message)
}

#[cfg(test)]
mod tests {
    use super::{reindex, ReindexReport};
    use crate::search::{IndexKind, SearchBackend, SearchDocument, SearchError, SearchHits};
    use async_trait::async_trait;
    use microwave_store::{NewNode, SqliteStore};
    use std::sync::{Arc, Mutex};

    struct RecordingBackend {
        documents: Mutex<Vec<SearchDocument>>,
    }

    impl RecordingBackend {
        fn new() -> Self {
            Self {
                documents: Mutex::new(Vec::new()),
            }
        }

        fn ids_for(&self, kind: IndexKind) -> Vec<String> {
            self.documents
                .lock()
                .unwrap()
                .iter()
                .filter(|document| document.kind == kind)
                .map(|document| document.id.clone())
                .collect()
        }
    }

    #[async_trait]
    impl SearchBackend for RecordingBackend {
        fn name(&self) -> &'static str {
            "recording"
        }

        async fn search(
            &self,
            _request: &crate::search::SearchRequest,
        ) -> Result<SearchHits, SearchError> {
            Ok(SearchHits {
                total: 0,
                ids: Vec::new(),
            })
        }

        async fn index(&self, document: &SearchDocument) -> Result<(), SearchError> {
            self.documents.lock().unwrap().push(document.clone());
            Ok(())
        }

        async fn delete(&self, _org_id: &str, _id: &str) -> Result<(), SearchError> {
            Ok(())
        }

        async fn delete_index(&self, _org_id: &str) -> Result<(), SearchError> {
            Ok(())
        }
    }

    fn populated_store() -> Arc<SqliteStore> {
        let store = SqliteStore::open_in_memory().unwrap();
        let org = store.create_org("acme", "Acme").unwrap();
        for container in ["nodes", "roles", "environments", "clients", "data"] {
            store.create_container_for_org(&org.id, container).unwrap();
        }
        store
            .create_node(
                &org.id,
                NewNode {
                    name: "node1",
                    body: r#"{"name":"node1","chef_environment":"_default","run_list":[]}"#,
                    environment: "_default",
                    policy_name: None,
                    policy_group: None,
                    creator_authz_id: &org.authz_id,
                },
            )
            .unwrap();
        store
            .create_role(
                &org.id,
                "web",
                r#"{"name":"web","run_list":[],"json_class":"Chef::Role","chef_type":"role"}"#,
                &org.authz_id,
            )
            .unwrap();
        store
            .create_environment(
                &org.id,
                "production",
                r#"{"name":"production","json_class":"Chef::Environment","chef_type":"environment","cookbook_versions":{}}"#,
                &org.authz_id,
            )
            .unwrap();
        store
            .create_client(&org.id, "client1", false, false)
            .unwrap();
        store
            .create_data_bag(&org.id, "secrets", &org.authz_id)
            .unwrap();
        store
            .create_data_bag_item(
                &org.id,
                "secrets",
                "db",
                r#"{"id":"db","password":"not-a-real-secret"}"#,
            )
            .unwrap();
        Arc::new(store)
    }

    #[tokio::test]
    async fn indexes_every_document_type_in_every_organization() {
        let store = populated_store();
        let backend = RecordingBackend::new();
        let report = reindex(&backend, store.as_ref(), None).await.unwrap();

        assert_eq!(
            report,
            ReindexReport {
                organizations: 1,
                documents: 5,
                nodes: 1,
                roles: 1,
                environments: 1,
                clients: 1,
                data_bags: 1,
                data_bag_items: 1,
            }
        );
        assert_eq!(backend.ids_for(IndexKind::Node).len(), 1);
        assert_eq!(backend.ids_for(IndexKind::Role).len(), 1);
        assert_eq!(backend.ids_for(IndexKind::Environment).len(), 1);
        assert_eq!(backend.ids_for(IndexKind::Client).len(), 1);
        assert_eq!(
            backend.ids_for(IndexKind::DataBag("secrets".into())).len(),
            1
        );
    }

    #[tokio::test]
    async fn limits_the_reindex_to_the_requested_organization() {
        let store = populated_store();
        let other = store.create_org("other", "Other").unwrap();
        store
            .create_client(&other.id, "other-client", false, false)
            .unwrap();
        let backend = RecordingBackend::new();

        let report = reindex(&backend, store.as_ref(), Some("other"))
            .await
            .unwrap();
        assert_eq!(report.organizations, 1);
        assert_eq!(report.clients, 1);
        assert_eq!(report.nodes, 0);
        assert!(backend
            .documents
            .lock()
            .unwrap()
            .iter()
            .all(|document| document.org_id == other.id));
    }

    #[tokio::test]
    async fn reports_a_missing_organization() {
        let store = populated_store();
        let backend = RecordingBackend::new();
        let error = reindex(&backend, store.as_ref(), Some("missing"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not exist"));
        assert!(backend.documents.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_empty_store_is_a_successful_no_op() {
        let store = SqliteStore::open_in_memory().unwrap();
        let backend = RecordingBackend::new();
        let report = reindex(&backend, &store, None).await.unwrap();
        assert_eq!(report.organizations, 0);
        assert_eq!(report.documents, 0);
    }
}
