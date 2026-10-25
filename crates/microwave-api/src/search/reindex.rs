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
//! - documents are written in backend batches, so remote backends can use
//!   `_bulk` for many documents at once;
//! - nodes, roles, environments, clients, and every data bag item are indexed;
//! - by default documents for deleted objects are **not** removed. Reindexing
//!   adds or replaces documents by store id, because an interrupted
//!   delete-then-rebuild would make the installation unsearchable. Passing
//!   `purge` deletes each visited organization's existing remote documents
//!   first, then rebuilds them;
//! - the backend is refreshed once the run completes so writes and purges are
//!   immediately visible to searches.

use super::backend::{IndexKind, SearchBackend, SearchDocument, SearchError};
use super::index;
use crate::ApiError;
use microwave_store::{MetadataStore, Org};

/// Summary of a completed reindex run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReindexReport {
    /// Number of organizations visited.
    pub organizations: usize,
    /// Total documents acknowledged by the backend. Documents are counted only
    /// after a successful write; remote backends that reject bulk items do not
    /// contribute to this number because the reindex fails instead.
    pub acknowledged_documents: usize,
    /// Organizations whose existing remote documents were deleted before
    /// reindexing. Always zero unless purge was requested.
    pub purged_organizations: usize,
    pub nodes: usize,
    pub roles: usize,
    pub environments: usize,
    pub clients: usize,
    pub data_bags: usize,
    pub data_bag_items: usize,
}

impl ReindexReport {
    fn record(&mut self, kind: &IndexKind, acknowledged: usize) {
        self.acknowledged_documents += acknowledged;
        match kind {
            IndexKind::Node => self.nodes += acknowledged,
            IndexKind::Role => self.roles += acknowledged,
            IndexKind::Environment => self.environments += acknowledged,
            IndexKind::Client => self.clients += acknowledged,
            IndexKind::DataBag(_) => self.data_bag_items += acknowledged,
        }
    }
}

/// Rebuild search documents for one organization or for every organization.
///
/// The caller chooses the backend. Indexing through the embedded backend is
/// harmless and reports the relational document counts, because the embedded
/// backend acknowledges each relational document and reads the store directly.
/// Documents are handed to [`SearchBackend::index_many`]; only acknowledged
/// documents are counted.
///
/// When `purge` is true, each visited organization's existing remote documents
/// are deleted before the rebuild. This is destructive: an interrupted purge
/// can leave an organization unsearchable until the command is run again.
pub async fn reindex(
    backend: &dyn SearchBackend,
    store: &dyn MetadataStore,
    organization: Option<&str>,
    purge: bool,
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
        reindex_org(backend, store, &org, purge, &mut report).await?;
    }
    if report.organizations > 0 {
        if let Err(error) = backend.refresh().await {
            let purge_detail = if purge {
                format!(
                    " and purged {} organization(s)",
                    report.purged_organizations
                )
            } else {
                String::new()
            };
            return Err(SearchError::Response(format!(
                "reindex wrote {} document(s) across {} organization(s){purge_detail}, but index refresh failed: {error}",
                report.acknowledged_documents, report.organizations
            )));
        }
    }
    Ok(report)
}

async fn reindex_org(
    backend: &dyn SearchBackend,
    store: &dyn MetadataStore,
    org: &Org,
    purge: bool,
    report: &mut ReindexReport,
) -> Result<(), SearchError> {
    if purge && backend.supports_purge() {
        backend.delete_index(&org.id).await?;
        report.purged_organizations += 1;
    }

    for kind in [
        IndexKind::Node,
        IndexKind::Role,
        IndexKind::Environment,
        IndexKind::Client,
    ] {
        let documents = documents_for_kind(store, org, kind.clone()).await?;
        let acknowledged = index_documents(backend, documents).await?;
        report.record(&kind, acknowledged);
    }

    let data_bags = store
        .list_data_bags(&org.id)
        .await
        .map_err(|error| SearchError::Response(error.to_string()))?;
    report.data_bags += data_bags.len();
    for data_bag in data_bags {
        let kind = IndexKind::DataBag(data_bag.name);
        let documents = documents_for_kind(store, org, kind.clone()).await?;
        let acknowledged = index_documents(backend, documents).await?;
        report.record(&kind, acknowledged);
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
) -> Result<usize, SearchError> {
    backend.index_many(&documents).await
}

fn api_error(error: ApiError) -> SearchError {
    SearchError::Response(error.message)
}

#[cfg(test)]
mod tests {
    use super::{reindex, ReindexReport};
    use crate::search::{
        EmbeddedSearch, IndexKind, IndexOutcome, SearchBackend, SearchDocument, SearchError,
        SearchHits,
    };
    use async_trait::async_trait;
    use microwave_store::{NewNode, SqliteStore};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingBackend {
        documents: Mutex<Vec<SearchDocument>>,
        acknowledged: usize,
        index_calls: AtomicUsize,
        index_many_calls: AtomicUsize,
        deleted_orgs: Mutex<Vec<String>>,
        refreshes: AtomicUsize,
        fail_refresh: bool,
        fail_delete: bool,
    }

    impl RecordingBackend {
        fn new() -> Self {
            Self {
                acknowledged: 1,
                ..Self::default()
            }
        }

        fn acknowledging(acknowledged: usize) -> Self {
            Self {
                acknowledged,
                ..Self::default()
            }
        }

        fn failing_refresh() -> Self {
            Self {
                acknowledged: 1,
                fail_refresh: true,
                ..Self::default()
            }
        }

        fn failing_delete() -> Self {
            Self {
                acknowledged: 1,
                fail_delete: true,
                ..Self::default()
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

        async fn index(&self, document: &SearchDocument) -> Result<IndexOutcome, SearchError> {
            self.index_calls.fetch_add(1, Ordering::SeqCst);
            self.documents.lock().unwrap().push(document.clone());
            Ok(IndexOutcome {
                acknowledged: self.acknowledged,
            })
        }

        async fn index_many(&self, documents: &[SearchDocument]) -> Result<usize, SearchError> {
            self.index_many_calls.fetch_add(1, Ordering::SeqCst);
            self.documents.lock().unwrap().extend_from_slice(documents);
            Ok(documents.len() * self.acknowledged)
        }

        async fn refresh(&self) -> Result<(), SearchError> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            if self.fail_refresh {
                return Err(SearchError::Response("refresh exploded".to_string()));
            }
            Ok(())
        }

        async fn delete(&self, _org_id: &str, _id: &str) -> Result<(), SearchError> {
            Ok(())
        }

        async fn delete_index(&self, org_id: &str) -> Result<(), SearchError> {
            if self.fail_delete {
                return Err(SearchError::Response("purge exploded".to_string()));
            }
            self.deleted_orgs.lock().unwrap().push(org_id.to_string());
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
        let report = reindex(&backend, store.as_ref(), None, false)
            .await
            .unwrap();

        assert_eq!(
            report,
            ReindexReport {
                organizations: 1,
                acknowledged_documents: 5,
                purged_organizations: 0,
                nodes: 1,
                roles: 1,
                environments: 1,
                clients: 1,
                data_bags: 1,
                data_bag_items: 1,
            }
        );
        assert_eq!(backend.index_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.index_many_calls.load(Ordering::SeqCst), 5);
        assert_eq!(backend.refreshes.load(Ordering::SeqCst), 1);
        assert!(backend.deleted_orgs.lock().unwrap().is_empty());
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

        let report = reindex(&backend, store.as_ref(), Some("other"), false)
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
    async fn counts_only_documents_the_backend_acknowledges() {
        let store = populated_store();
        let backend = RecordingBackend::acknowledging(0);
        let report = reindex(&backend, store.as_ref(), None, false)
            .await
            .unwrap();

        assert_eq!(backend.documents.lock().unwrap().len(), 5);
        assert_eq!(report.acknowledged_documents, 0);
        assert_eq!(report.nodes, 0);
        assert_eq!(report.roles, 0);
        assert_eq!(report.environments, 0);
        assert_eq!(report.clients, 0);
        assert_eq!(report.data_bag_items, 0);
    }

    #[tokio::test]
    async fn reports_a_missing_organization() {
        let store = populated_store();
        let backend = RecordingBackend::new();
        let error = reindex(&backend, store.as_ref(), Some("missing"), false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not exist"));
        assert!(backend.documents.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_empty_store_is_a_successful_no_op() {
        let store = SqliteStore::open_in_memory().unwrap();
        let backend = RecordingBackend::new();
        let report = reindex(&backend, &store, None, true).await.unwrap();
        assert_eq!(report.organizations, 0);
        assert_eq!(report.acknowledged_documents, 0);
        assert_eq!(report.purged_organizations, 0);
        assert_eq!(backend.refreshes.load(Ordering::SeqCst), 0);
        assert!(backend.deleted_orgs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn purge_deletes_the_selected_organization_before_reindexing() {
        let store = populated_store();
        let other = store.create_org("other", "Other").unwrap();
        store
            .create_client(&other.id, "other-client", false, false)
            .unwrap();
        let backend = RecordingBackend::new();

        let report = reindex(&backend, store.as_ref(), Some("other"), true)
            .await
            .unwrap();

        assert_eq!(report.purged_organizations, 1);
        assert_eq!(report.organizations, 1);
        assert_eq!(report.clients, 1);
        assert_eq!(
            *backend.deleted_orgs.lock().unwrap(),
            vec![other.id.clone()]
        );
        assert!(backend
            .documents
            .lock()
            .unwrap()
            .iter()
            .all(|document| document.org_id == other.id));
        assert_eq!(backend.refreshes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn purge_visits_every_organization_and_deletes_its_documents() {
        let store = populated_store();
        let other = store.create_org("other", "Other").unwrap();
        store
            .create_client(&other.id, "other-client", false, false)
            .unwrap();
        let backend = RecordingBackend::new();

        let report = reindex(&backend, store.as_ref(), None, true).await.unwrap();

        assert_eq!(report.purged_organizations, 2);
        let deleted = backend.deleted_orgs.lock().unwrap().clone();
        assert_eq!(deleted.len(), 2);
        let acme = store.find_org("acme").unwrap().unwrap();
        assert!(deleted.contains(&acme.id));
        assert!(deleted.contains(&other.id));
    }

    #[tokio::test]
    async fn embedded_reindex_does_not_claim_a_purge() {
        let store = populated_store();
        let backend = EmbeddedSearch::new(store.clone());

        let report = reindex(&backend, store.as_ref(), None, true).await.unwrap();

        assert_eq!(report.organizations, 1);
        assert_eq!(report.acknowledged_documents, 5);
        assert_eq!(report.purged_organizations, 0);
    }

    #[tokio::test]
    async fn a_failed_purge_stops_before_indexing_or_refreshing() {
        let store = populated_store();
        let backend = RecordingBackend::failing_delete();

        let error = reindex(&backend, store.as_ref(), None, true)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("purge exploded"));
        assert_eq!(backend.index_many_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.refreshes.load(Ordering::SeqCst), 0);
        assert!(backend.deleted_orgs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_failure_reports_the_completed_indexing_work() {
        let store = populated_store();
        let backend = RecordingBackend::failing_refresh();

        let error = reindex(&backend, store.as_ref(), None, true)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("reindex wrote 5 document(s) across 1 organization(s)"));
        assert!(error.contains("purged 1 organization(s)"));
        assert!(error.contains("refresh failed"));
        assert!(error.contains("refresh exploded"));
        assert_eq!(backend.index_many_calls.load(Ordering::SeqCst), 5);
        assert_eq!(backend.refreshes.load(Ordering::SeqCst), 1);
    }
}
