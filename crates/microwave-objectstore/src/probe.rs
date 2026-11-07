//! Pre-flight probe for every bucket a routing snapshot would use.
//!
//! A snapshot only goes live after each configured bucket answered a single
//! `HEAD <bucket>`, so a typo or a key without `HeadBucket` rights shows up
//! before traffic moves. The probe never creates a bucket and speaks plain S3,
//! so it works against AWS S3, Garage and MinIO alike.
//!
//! The 1.5 s per-request bound lives in [`S3ObjectStore::health_check`]; this
//! module only bounds how many requests run at once.

use crate::{ObjectStore, ObjectStoreError, S3Config, S3ObjectStore, TenantSnapshot};
use microwave_config::ObjectStoreConfig;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Maximum number of bucket probes that may be in flight at once.
const MAX_CONCURRENT_PROBES: usize = 8;

/// Probes one bucket with one signed `HEAD <bucket>`.
///
/// The endpoint and bucket come from the caller, the scalar settings (region,
/// credentials, path style) from `config`. Built for `microwave-ctl
/// diagnostics --probe`, which has no store instance yet.
pub async fn probe_endpoint(
    endpoint: &str,
    bucket: &str,
    config: &S3Config,
) -> Result<(), ObjectStoreError> {
    let mut probe = config.clone();
    probe.endpoint = endpoint.trim().to_string();
    probe.bucket = bucket.trim().to_string();
    let bucket = probe.bucket.clone();
    let store = S3ObjectStore::new(probe)?;

    tokio::task::spawn_blocking(move || store.health_check())
        .await
        .map_err(|error| ObjectStoreError::Http(format!("bucket probe task failed: {error}")))?
        .map_err(|error| ObjectStoreError::BucketProbe {
            bucket,
            source: Box::new(error),
        })
}

/// Probe every bucket a snapshot would serve before it is installed.
///
/// Probes all `by_org` stores plus `default`; `health` is skipped because it is
/// the default bucket outside strict mode and absent in strict mode. At most
/// eight requests run at once, and the first failure rejects the whole
/// snapshot — so a typo shows up before traffic moves.
///
/// Returns `Ok` without touching the snapshot when `verify_buckets` is off.
pub async fn verify_buckets(
    config: &ObjectStoreConfig,
    snapshot: &TenantSnapshot,
) -> Result<(), ObjectStoreError> {
    if !config.verify_buckets {
        return Ok(());
    }

    let mut targets: Vec<(Option<String>, Arc<dyn ObjectStore + Send + Sync>)> = snapshot
        .by_org
        .iter()
        .map(|(organization_id, store)| (Some(organization_id.clone()), Arc::clone(store)))
        .collect();
    if let Some(default) = &snapshot.default {
        targets.push((None, Arc::clone(default)));
    }
    if targets.is_empty() {
        return Ok(());
    }

    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES));
    let mut tasks = JoinSet::new();
    for (organization_id, store) in targets {
        let permits = Arc::clone(&permits);
        tasks.spawn(async move {
            let permit = permits
                .acquire_owned()
                .await
                .expect("the probe semaphore is never closed");
            let bucket = store.bucket().unwrap_or("-").to_string();
            let result = tokio::task::spawn_blocking(move || store.health_check()).await;
            drop(permit);

            result
                .map_err(|error| {
                    ObjectStoreError::Http(format!("bucket probe task failed: {error}"))
                })?
                .map_err(|error| {
                    tracing::error!(
                        organization_id = organization_id.as_deref().unwrap_or("-"),
                        bucket = %bucket,
                        error = %error,
                        "object store bucket probe failed"
                    );
                    ObjectStoreError::BucketProbe {
                        bucket,
                        source: Box::new(error),
                    }
                })
        });
    }

    while let Some(joined) = tasks.join_next().await {
        let result = joined.map_err(|error| {
            ObjectStoreError::Http(format!("bucket probe task failed: {error}"))
        })?;
        if let Err(error) = result {
            // Stop the queued probes; already running ones are bounded by the
            // health-check timeout.
            tasks.shutdown().await;
            return Err(error);
        }
    }
    Ok(())
}
