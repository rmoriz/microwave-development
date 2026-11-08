//! PostgreSQL half of the per-organization quota guard.
//!
//! The counting SQL is shared with the generic store crate; only the
//! transaction type, the advisory lock and the existence probes are specific
//! to this backend.

use microwave_config::OrgQuotas;
use microwave_metrics::Metrics;
use microwave_store::{count_sql, reject_if_full, QuotaKind, QuotaPolicy, QuotaTarget, StoreError};
use sqlx::postgres::Postgres;
use sqlx::Transaction;
use std::sync::Arc;

use crate::store::{db, PostgresStore};

impl PostgresStore {
    pub(crate) fn quota_metrics(&self) -> Option<Arc<Metrics>> {
        self.metrics
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn quota_limit(&self, org_id: &str, kind: QuotaKind) -> Option<u64> {
        self.quotas
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .limit(org_id, kind)
    }

    /// Reject a creation that would exceed the organization's ceiling.
    ///
    /// The limit is read before the lock so unlimited organizations pay
    /// nothing. The advisory lock serializes creators per organization and
    /// kind across replicas until this transaction ends.
    pub(crate) async fn enforce_quota(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        org_id: &str,
        target: QuotaTarget<'_>,
    ) -> Result<(), StoreError> {
        if target.is_exempt() {
            return Ok(());
        }
        let kind = target.kind();
        let Some(limit) = self.quota_limit(org_id, kind) else {
            return Ok(());
        };
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{org_id}:{}", kind.label()))
            .execute(&mut **tx)
            .await
            .map_err(db)?;
        if exists_postgres(target, tx, org_id).await? {
            return Ok(());
        }
        let count = count_quota(tx, org_id, kind).await?;
        reject_if_full(self.quota_metrics().as_deref(), org_id, kind, limit, count)
    }
}

/// Whether the exact row already exists, so a re-upload keeps its `Conflict`
/// answer instead of a quota rejection.
async fn exists_postgres(
    target: QuotaTarget<'_>,
    tx: &mut Transaction<'_, Postgres>,
    org_id: &str,
) -> Result<bool, StoreError> {
    let exists: bool = match target {
        QuotaTarget::Node { name } => {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes WHERE org_id = $1 AND name = $2)")
                .bind(org_id)
                .bind(name)
                .fetch_one(&mut **tx)
                .await
                .map_err(db)?
        }
        QuotaTarget::CookbookVersion { name, version } => {
            let (major, minor, patch) = microwave_store::parse_version(version)?;
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM cookbook_versions
                               WHERE org_id = $1 AND name = $2 AND major = $3
                                 AND minor = $4 AND patch = $5)",
            )
            .bind(org_id)
            .bind(name)
            .bind(major)
            .bind(minor)
            .bind(patch)
            .fetch_one(&mut **tx)
            .await
            .map_err(db)?
        }
        QuotaTarget::Environment { name } => sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM environments WHERE org_id = $1 AND name = $2)",
        )
        .bind(org_id)
        .bind(name)
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?,
        QuotaTarget::Role { name } => {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM roles WHERE org_id = $1 AND name = $2)")
                .bind(org_id)
                .bind(name)
                .fetch_one(&mut **tx)
                .await
                .map_err(db)?
        }
        QuotaTarget::PolicyRevision { name, revision_id } => sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM policy_revisions
                           WHERE org_id = $1 AND name = $2 AND revision_id = $3)",
        )
        .bind(org_id)
        .bind(name)
        .bind(revision_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?,
        QuotaTarget::CookbookArtifactVersion { name, identifier } => sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM cookbook_artifact_versions
                           WHERE org_id = $1 AND cookbook_artifact_id =
                             (SELECT id FROM cookbook_artifacts WHERE org_id = $1 AND name = $2)
                           AND identifier = $3)",
        )
        .bind(org_id)
        .bind(name)
        .bind(identifier)
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?,
    };
    Ok(exists)
}

async fn count_quota(
    tx: &mut Transaction<'_, Postgres>,
    org_id: &str,
    kind: QuotaKind,
) -> Result<u64, StoreError> {
    let count: i64 = sqlx::query_scalar(count_sql(kind))
        .bind(org_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?;
    Ok(count as u64)
}

impl microwave_store::QuotaRepository for PostgresStore {
    fn set_quotas(&self, policy: QuotaPolicy) {
        self.quotas
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(policy);
    }

    fn set_org_quotas(&self, org_id: &str, quotas: Option<OrgQuotas>) {
        self.quotas
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set_org(org_id, quotas);
    }

    fn quota_limit(&self, org_id: &str, kind: QuotaKind) -> Option<u64> {
        PostgresStore::quota_limit(self, org_id, kind)
    }

    fn set_metrics(&self, metrics: Arc<Metrics>) {
        PostgresStore::set_metrics(self, metrics);
    }
}
