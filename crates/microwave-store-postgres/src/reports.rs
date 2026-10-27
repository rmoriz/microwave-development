//! Compiling stub for the report repository.
//!
//! Task 3 replaces these methods with real PostgreSQL queries. Until then they
//! return a clear error so the backend satisfies
//! [`MetadataStore`](microwave_store::MetadataStore) without pretending to
//! persist node runs.

use async_trait::async_trait;
use microwave_store::{NewNodeRun, NodeRunFilter, ReportRepository, StoreError, StoredNodeRun};

use crate::store::PostgresStore;

const NOT_IMPLEMENTED: &str = "report repository not implemented yet";

#[async_trait]
impl ReportRepository for PostgresStore {
    async fn store_node_run(
        &self,
        _org_id: &str,
        _run: NewNodeRun<'_>,
        _keep_runs: usize,
    ) -> Result<StoredNodeRun, StoreError> {
        Err(StoreError::Invalid(NOT_IMPLEMENTED.into()))
    }

    async fn list_node_runs(
        &self,
        _org_id: &str,
        _filter: &NodeRunFilter,
    ) -> Result<Vec<StoredNodeRun>, StoreError> {
        Err(StoreError::Invalid(NOT_IMPLEMENTED.into()))
    }

    async fn find_node_run(
        &self,
        _org_id: &str,
        _node_name: &str,
        _run_id: &str,
    ) -> Result<Option<StoredNodeRun>, StoreError> {
        Err(StoreError::Invalid(NOT_IMPLEMENTED.into()))
    }

    async fn latest_node_run(
        &self,
        _org_id: &str,
        _node_name: &str,
    ) -> Result<Option<StoredNodeRun>, StoreError> {
        Err(StoreError::Invalid(NOT_IMPLEMENTED.into()))
    }
}
