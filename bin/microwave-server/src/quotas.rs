//! Resolve the configured per-organization quotas against the organization
//! table.
//!
//! An entry names an organization; the store keys its policy by organization
//! id, so the names are looked up once at startup and on every reload. An
//! unknown name only warns and leaves the entry inactive.

use microwave_config::Config;
use microwave_store::{MetadataStore, QuotaPolicy, StoreError};
use std::collections::BTreeMap;

pub(crate) async fn resolve(
    config: &Config,
    store: &dyn MetadataStore,
) -> Result<QuotaPolicy, StoreError> {
    let mut policy = QuotaPolicy::default();
    if !config.quotas.enabled || config.quotas.organizations.is_empty() {
        return Ok(policy);
    }
    let ids: BTreeMap<String, String> = store
        .list_orgs()
        .await?
        .into_iter()
        .map(|org| (org.name, org.id))
        .collect();
    for (name, quotas) in &config.quotas.organizations {
        match ids.get(name) {
            Some(id) => policy.set_org(id, Some(quotas.clone())),
            None => tracing::warn!(
                organization = %name,
                "quotas.organizations names an unknown organization; its entry stays inactive"
            ),
        }
    }
    Ok(policy)
}
