//! Per-organization object quotas.
//!
//! A quota is a hard ceiling on the number of rows of one kind an organization
//! may own. Creation is the only guarded operation: reaching a ceiling rejects
//! the create with [`StoreError::Quota`], while updates and deletes stay
//! allowed. An organization without an entry, or a kind without a ceiling,
//! stays unlimited.
//!
//! The SQL here is backend-neutral (`$1` placeholders, no dialect features) and
//! is executed by both the SQLite and the PostgreSQL store.

use crate::cookbooks::parse_version;
use crate::sqlite::StoreError;
use microwave_config::OrgQuotas;
use microwave_metrics::Metrics;
use rusqlite::{params, Transaction};
use std::collections::BTreeMap;

/// The five object kinds a quota can cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaKind {
    Node,
    CookbookVersion,
    Environment,
    Role,
    PolicyRevision,
}

impl QuotaKind {
    /// Stable label used in messages, metrics and advisory lock keys.
    pub fn label(self) -> &'static str {
        match self {
            QuotaKind::Node => "node",
            QuotaKind::CookbookVersion => "cookbook version",
            QuotaKind::Environment => "environment",
            QuotaKind::Role => "role",
            QuotaKind::PolicyRevision => "policy revision",
        }
    }

    /// The ceiling configured for this kind, or `None` when unlimited.
    pub fn limit(self, quotas: &OrgQuotas) -> Option<u64> {
        match self {
            QuotaKind::Node => quotas.max_nodes,
            QuotaKind::CookbookVersion => quotas.max_cookbook_versions,
            QuotaKind::Environment => quotas.max_environments,
            QuotaKind::Role => quotas.max_roles,
            QuotaKind::PolicyRevision => quotas.max_policy_revisions,
        }
    }
}

impl std::fmt::Display for QuotaKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The row a guarded creation is about to write.
#[derive(Debug, Clone, Copy)]
pub enum QuotaTarget<'a> {
    Node { name: &'a str },
    CookbookVersion { name: &'a str, version: &'a str },
    Environment { name: &'a str },
    Role { name: &'a str },
    PolicyRevision { name: &'a str, revision_id: &'a str },
    CookbookArtifactVersion { name: &'a str, identifier: &'a str },
}

impl<'a> QuotaTarget<'a> {
    /// Map a generic named-object table to its quota kind, if it has one.
    pub fn for_named_table(table: &'a str, name: &'a str) -> Option<QuotaTarget<'a>> {
        match table {
            "environments" => Some(QuotaTarget::Environment { name }),
            "roles" => Some(QuotaTarget::Role { name }),
            _ => None,
        }
    }
}

impl QuotaTarget<'_> {
    pub fn kind(self) -> QuotaKind {
        match self {
            QuotaTarget::Node { .. } => QuotaKind::Node,
            QuotaTarget::CookbookVersion { .. } | QuotaTarget::CookbookArtifactVersion { .. } => {
                QuotaKind::CookbookVersion
            }
            QuotaTarget::Environment { .. } => QuotaKind::Environment,
            QuotaTarget::Role { .. } => QuotaKind::Role,
            QuotaTarget::PolicyRevision { .. } => QuotaKind::PolicyRevision,
        }
    }

    /// `_default` is auto-created and cannot be deleted, so it is exempt
    /// entirely and never rejected, not even at a ceiling of zero.
    pub fn is_exempt(&self) -> bool {
        matches!(self, QuotaTarget::Environment { name: "_default" })
    }
}

/// The installed limits, keyed by organization id.
#[derive(Debug, Clone, Default)]
pub struct QuotaPolicy {
    quotas: BTreeMap<String, OrgQuotas>,
}

impl QuotaPolicy {
    pub fn limit(&self, org_id: &str, kind: QuotaKind) -> Option<u64> {
        self.quotas
            .get(org_id)
            .and_then(|quotas| kind.limit(quotas))
    }

    pub fn set_org(&mut self, org_id: &str, quotas: Option<OrgQuotas>) {
        match quotas {
            Some(quotas) => {
                self.quotas.insert(org_id.to_string(), quotas);
            }
            None => {
                self.quotas.remove(org_id);
            }
        }
    }

    pub fn replace(&mut self, other: QuotaPolicy) {
        self.quotas = other.quotas;
    }
}

/// `COUNT(*)` for one kind inside one organization.
///
/// Cookbook versions and cookbook artifact versions share a kind and their
/// counts are summed. `_default` never counts as an environment.
pub fn count_sql(kind: QuotaKind) -> &'static str {
    match kind {
        QuotaKind::Node => "SELECT COUNT(*) FROM nodes WHERE org_id = $1",
        QuotaKind::CookbookVersion => {
            "SELECT (SELECT COUNT(*) FROM cookbook_versions WHERE org_id = $1) \
                    + (SELECT COUNT(*) FROM cookbook_artifact_versions WHERE org_id = $1)"
        }
        QuotaKind::Environment => {
            "SELECT COUNT(*) FROM environments WHERE org_id = $1 AND name <> '_default'"
        }
        QuotaKind::Role => "SELECT COUNT(*) FROM roles WHERE org_id = $1",
        QuotaKind::PolicyRevision => "SELECT COUNT(*) FROM policy_revisions WHERE org_id = $1",
    }
}

/// Reject when `count` already reached `limit`, otherwise allow.
pub fn reject_if_full(
    metrics: Option<&Metrics>,
    org_id: &str,
    kind: QuotaKind,
    limit: u64,
    count: u64,
) -> Result<(), StoreError> {
    if count < limit {
        return Ok(());
    }
    tracing::warn!(org_id, kind = kind.label(), limit, "quota reached");
    if let Some(metrics) = metrics {
        metrics.record_limit(&format!("quota:{}", kind.label()), org_id, "rejected");
    }
    Err(StoreError::Quota { kind, limit })
}

impl QuotaTarget<'_> {
    /// Whether the exact row already exists, so a re-upload keeps its
    /// `Conflict` answer instead of a quota rejection.
    pub(crate) fn exists_sqlite(
        &self,
        tx: &Transaction<'_>,
        org_id: &str,
    ) -> Result<bool, StoreError> {
        let exists = match self {
            QuotaTarget::Node { name } => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE org_id = ?1 AND name = ?2)",
                params![org_id, name],
                |row| row.get::<_, i64>(0),
            )?,
            QuotaTarget::CookbookVersion { name, version } => {
                let (major, minor, patch) = parse_version(version)?;
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM cookbook_versions
                                   WHERE org_id = ?1 AND name = ?2 AND major = ?3
                                     AND minor = ?4 AND patch = ?5)",
                    params![org_id, name, major, minor, patch],
                    |row| row.get::<_, i64>(0),
                )?
            }
            QuotaTarget::Environment { name } => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM environments WHERE org_id = ?1 AND name = ?2)",
                params![org_id, name],
                |row| row.get::<_, i64>(0),
            )?,
            QuotaTarget::Role { name } => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM roles WHERE org_id = ?1 AND name = ?2)",
                params![org_id, name],
                |row| row.get::<_, i64>(0),
            )?,
            QuotaTarget::PolicyRevision { name, revision_id } => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM policy_revisions
                               WHERE org_id = ?1 AND name = ?2 AND revision_id = ?3)",
                params![org_id, name, revision_id],
                |row| row.get::<_, i64>(0),
            )?,
            QuotaTarget::CookbookArtifactVersion { name, identifier } => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cookbook_artifact_versions
                               WHERE org_id = ?1 AND cookbook_artifact_id =
                                 (SELECT id FROM cookbook_artifacts WHERE org_id = ?1 AND name = ?2)
                               AND identifier = ?3)",
                params![org_id, name, identifier],
                |row| row.get::<_, i64>(0),
            )?,
        };
        Ok(exists != 0)
    }
}

pub(crate) fn count_quota(
    tx: &Transaction<'_>,
    org_id: &str,
    kind: QuotaKind,
) -> Result<u64, StoreError> {
    let count: i64 = tx.query_row(count_sql(kind), params![org_id], |row| row.get(0))?;
    Ok(count as u64)
}
