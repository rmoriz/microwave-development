//! Cross-checks between the object-store tenant map, the organization table
//! and the per-organization credentials.
//!
//! Structural validation lives in [`ObjectStoreConfig::validate`]; the checks
//! here need data the configuration does not own (the organization table and
//! the secrets table), so they stay separate and only produce issues. Whether
//! an issue is fatal is up to the caller: the server warns, `microwave-ctl`
//! fails.

use crate::{ObjectStoreConfig, OrgCredentials};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// One inconsistency in the tenant map.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TenantIssue {
    /// The tenant map maps an organization that does not exist.
    UnknownOrg(String),
    /// Strict mode: the organization has no tenant map entry.
    MissingEntry(String),
    /// Strict mode: the organization's access key or secret key is unset.
    MissingCredential(String, &'static str),
}

impl fmt::Display for TenantIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOrg(organization) => write!(
                formatter,
                "object_store.organizations.{organization} names an unknown organization"
            ),
            Self::MissingEntry(organization) => write!(
                formatter,
                "organization {organization} has no object_store.organizations entry"
            ),
            Self::MissingCredential(organization, field) => write!(
                formatter,
                "organization {organization} is missing object_store.{organization}.{field}"
            ),
        }
    }
}

/// Collect every tenant-map issue for the given organization table and
/// credential map.
///
/// Unknown organizations are always reported. Missing entries and missing
/// credentials only matter in strict mode (`require_org_bucket`); the shared
/// bucket never applies there. The result is sorted so callers print a stable
/// list.
pub fn tenant_issues(
    config: &ObjectStoreConfig,
    organizations: &BTreeSet<String>,
    credentials: &BTreeMap<String, OrgCredentials>,
) -> Vec<TenantIssue> {
    let mut issues = Vec::new();

    for organization in config.organizations.keys() {
        if !organizations.contains(organization) {
            issues.push(TenantIssue::UnknownOrg(organization.clone()));
        }
    }

    if config.require_org_bucket {
        for organization in organizations {
            if !config.organizations.contains_key(organization) {
                issues.push(TenantIssue::MissingEntry(organization.clone()));
            }
            let organization_credentials = credentials.get(organization);
            for (field, value) in [
                (
                    "access_key",
                    organization_credentials.map(|c| &c.access_key),
                ),
                (
                    "secret_key",
                    organization_credentials.map(|c| &c.secret_key),
                ),
            ] {
                if value.is_none_or(|value| value.trim().is_empty()) {
                    issues.push(TenantIssue::MissingCredential(organization.clone(), field));
                }
            }
        }
    }

    issues.sort();
    issues
}
