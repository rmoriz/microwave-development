//! Per-organization routing decorator for [`ObjectStore`].
//!
//! The router holds an atomically swapped snapshot of per-organization stores.
//! An organization without a mapping falls back to the snapshot's default
//! store; with no default the call fails with
//! [`ObjectStoreError::NoOrgBucket`]. The router never builds keys or buckets
//! itself: it forwards the call unchanged, so the key layout stays with the
//! backend.

use super::{ObjectMetadata, ObjectStore, ObjectStoreError};
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::sync::Arc;

/// One immutable set of stores installed in a [`RoutingObjectStore`].
///
/// Snapshots are cheap to clone: the stores themselves stay shared.
#[derive(Clone, Default)]
pub struct TenantSnapshot {
    /// Store for organizations without their own mapping.
    pub default: Option<Arc<dyn ObjectStore + Send + Sync>>,
    /// Store the `/_status` probe talks to, independent of any organization.
    pub health: Option<Arc<dyn ObjectStore + Send + Sync>>,
    /// Per-organization stores keyed by organization id.
    pub by_org: HashMap<String, Arc<dyn ObjectStore + Send + Sync>>,
}

/// Routes every call to the store of the calling organization.
pub struct RoutingObjectStore {
    snapshot: ArcSwap<TenantSnapshot>,
}

impl RoutingObjectStore {
    pub fn new(snapshot: TenantSnapshot) -> Self {
        Self {
            snapshot: ArcSwap::from_pointee(snapshot),
        }
    }

    /// Replace the whole snapshot. In-flight calls keep the store they picked.
    pub fn install(&self, snapshot: TenantSnapshot) {
        self.snapshot.store(Arc::new(snapshot));
    }

    /// Pick the store for `organization_id`, dropping the snapshot guard so a
    /// concurrent [`install`](Self::install) never waits on a live call.
    fn pick(&self, organization_id: &str) -> Option<Arc<dyn ObjectStore + Send + Sync>> {
        let snapshot = self.snapshot.load();
        snapshot
            .by_org
            .get(organization_id)
            .cloned()
            .or_else(|| snapshot.default.clone())
    }
}

/// Log one failed operation. The org id, the operation and the bucket make the
/// line actionable; keys and checksums stay out because they are per blob.
fn log_failure(
    organization_id: &str,
    operation: &'static str,
    store: &dyn ObjectStore,
    error: &ObjectStoreError,
) {
    tracing::error!(
        organization_id,
        operation,
        bucket = store.bucket().unwrap_or("-"),
        error = %error,
        "object store operation failed"
    );
}

impl ObjectStore for RoutingObjectStore {
    fn put(
        &self,
        organization_id: &str,
        checksum: &str,
        data: &[u8],
    ) -> Result<(), ObjectStoreError> {
        let Some(store) = self.pick(organization_id) else {
            return Err(ObjectStoreError::NoOrgBucket);
        };
        let result = store.put(organization_id, checksum, data);
        if let Err(error) = &result {
            log_failure(organization_id, "put", store.as_ref(), error);
        }
        result
    }

    fn get(&self, organization_id: &str, checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        let Some(store) = self.pick(organization_id) else {
            return Err(ObjectStoreError::NoOrgBucket);
        };
        let result = store.get(organization_id, checksum);
        if let Err(error) = &result {
            log_failure(organization_id, "get", store.as_ref(), error);
        }
        result
    }

    fn exists(&self, organization_id: &str, checksum: &str) -> bool {
        match self.pick(organization_id) {
            Some(store) => store.exists(organization_id, checksum),
            None => false,
        }
    }

    fn delete(&self, organization_id: &str, checksum: &str) -> Result<bool, ObjectStoreError> {
        let Some(store) = self.pick(organization_id) else {
            return Err(ObjectStoreError::NoOrgBucket);
        };
        let result = store.delete(organization_id, checksum);
        if let Err(error) = &result {
            log_failure(organization_id, "delete", store.as_ref(), error);
        }
        result
    }

    fn metadata(
        &self,
        organization_id: &str,
        checksum: &str,
    ) -> Result<Option<ObjectMetadata>, ObjectStoreError> {
        let Some(store) = self.pick(organization_id) else {
            return Err(ObjectStoreError::NoOrgBucket);
        };
        let result = store.metadata(organization_id, checksum);
        if let Err(error) = &result {
            log_failure(organization_id, "metadata", store.as_ref(), error);
        }
        result
    }

    fn presigned_url(
        &self,
        method: &str,
        organization_id: &str,
        checksum: &str,
    ) -> Result<String, ObjectStoreError> {
        let Some(store) = self.pick(organization_id) else {
            return Err(ObjectStoreError::NoOrgBucket);
        };
        let result = store.presigned_url(method, organization_id, checksum);
        if let Err(error) = &result {
            log_failure(organization_id, "presigned_url", store.as_ref(), error);
        }
        result
    }

    fn health_check(&self) -> Result<(), ObjectStoreError> {
        let snapshot = self.snapshot.load();
        let store = snapshot.health.clone().or_else(|| snapshot.default.clone());
        drop(snapshot);
        let Some(store) = store else {
            return Err(ObjectStoreError::InvalidConfig(
                "health bucket is not configured".into(),
            ));
        };
        let result = store.health_check();
        if let Err(error) = &result {
            log_failure("-", "health_check", store.as_ref(), error);
        }
        result
    }
}
