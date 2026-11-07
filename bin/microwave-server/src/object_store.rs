//! Builds the object-store routing snapshot from the configured tenant map.
//!
//! Installed by startup and, later, swapped by the reloader; both need the
//! build without the install, so the two stay separate.

use microwave_config::{ObjectStoreBackend, ObjectStoreConfig, OrgCredentials};
use microwave_objectstore::{ObjectStoreError, S3Config, S3ObjectStore, TenantSnapshot};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Global endpoint, credentials and defaults every entry merges its own fields
/// over.
struct SharedTarget {
    endpoint: String,
    region: String,
    access_key: String,
    secret_key: String,
    path_style: bool,
}

fn required(value: Option<&String>, field: &str) -> Result<String, ObjectStoreError> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ObjectStoreError::InvalidConfig(format!(
                "{field} is required for the s3/garage backend"
            ))
        })
}

impl SharedTarget {
    fn new(config: &ObjectStoreConfig) -> Result<Self, ObjectStoreError> {
        Ok(Self {
            endpoint: required(config.endpoint.as_ref(), "object_store.endpoint")?,
            region: config.region.clone(),
            access_key: required(config.access_key.as_ref(), "object_store.access_key")?,
            secret_key: required(config.secret_key.as_ref(), "object_store.secret_key")?,
            path_style: config.path_style,
        })
    }

    fn store(&self, bucket: &str) -> Result<S3ObjectStore, ObjectStoreError> {
        let mut config = S3Config::new(
            self.endpoint.as_str(),
            bucket,
            self.region.as_str(),
            self.access_key.as_str(),
            self.secret_key.as_str(),
        );
        config.path_style = self.path_style;
        S3ObjectStore::new(config)
    }

    /// Build the store for one entry: its fields win field by field, its
    /// credentials fall back to the shared ones when they are unset.
    fn entry_store(
        &self,
        bucket: &str,
        endpoint: Option<&String>,
        region: Option<&String>,
        path_style: Option<bool>,
        credentials: Option<&OrgCredentials>,
    ) -> Result<S3ObjectStore, ObjectStoreError> {
        let access_key = credentials
            .map(|credentials| credentials.access_key.trim())
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.access_key.clone());
        let secret_key = credentials
            .map(|credentials| credentials.secret_key.trim())
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.secret_key.clone());
        let mut config = S3Config::new(
            endpoint.map_or(self.endpoint.as_str(), String::as_str),
            bucket,
            region.map_or(self.region.as_str(), String::as_str),
            access_key,
            secret_key,
        );
        config.path_style = path_style.unwrap_or(self.path_style);
        S3ObjectStore::new(config)
    }
}

/// Build the snapshot: the shared default (absent in strict mode), one store
/// per organization mapped to an existing organization id, and the health
/// store.
///
/// `org_ids` holds only organizations that exist, so an entry for an unknown
/// organization is silently left out; the caller warns about it.
///
/// A `local` backend has no endpoint or credentials to merge: this returns an
/// empty snapshot and the caller installs the local store as its `default`.
pub fn build_snapshot(
    config: &ObjectStoreConfig,
    org_ids: &BTreeMap<String, String>,
    creds: &BTreeMap<String, OrgCredentials>,
) -> Result<TenantSnapshot, ObjectStoreError> {
    if config.backend == ObjectStoreBackend::Local {
        return Ok(TenantSnapshot::default());
    }

    let shared = SharedTarget::new(config)?;
    let mut snapshot = TenantSnapshot::default();

    if !config.require_org_bucket {
        let bucket = required(config.bucket.as_ref(), "object_store.bucket")?;
        snapshot.default = Some(Arc::new(shared.store(&bucket)?));
    }

    for (organization, entry) in &config.organizations {
        let Some(organization_id) = org_ids.get(organization) else {
            continue;
        };
        let store = shared.entry_store(
            entry.bucket.trim(),
            entry.endpoint.as_ref(),
            entry.region.as_ref(),
            entry.path_style,
            creds.get(organization),
        )?;
        snapshot
            .by_org
            .insert(organization_id.clone(), Arc::new(store));
    }

    if let Some(bucket) = config.health_bucket.as_deref() {
        snapshot.health = Some(Arc::new(shared.store(bucket.trim())?));
    }

    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::build_snapshot;
    use microwave_config::{
        ObjectStoreBackend, ObjectStoreConfig, ObjectStoreOrgConfig, OrgCredentials,
    };
    use std::collections::BTreeMap;

    fn strict_config() -> ObjectStoreConfig {
        ObjectStoreConfig {
            backend: ObjectStoreBackend::S3,
            endpoint: Some("http://127.0.0.1:3900".into()),
            region: "garage".into(),
            access_key: Some("shared-access".into()),
            secret_key: Some("shared-secret".into()),
            require_org_bucket: true,
            health_bucket: Some("mw-health".into()),
            ..ObjectStoreConfig::default()
        }
    }

    fn credentials(access_key: &str, secret_key: &str) -> BTreeMap<String, OrgCredentials> {
        BTreeMap::from([(
            "acme".to_string(),
            OrgCredentials {
                access_key: access_key.to_string(),
                secret_key: secret_key.to_string(),
            },
        )])
    }

    fn mapped(id: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("acme".to_string(), id.to_string())])
    }

    #[test]
    fn a_strict_snapshot_has_no_default_but_maps_the_org_and_the_health_bucket() {
        let mut config = strict_config();
        config.organizations.insert(
            "acme".to_string(),
            ObjectStoreOrgConfig {
                bucket: "mw-acme".into(),
                ..Default::default()
            },
        );

        let snapshot = build_snapshot(&config, &mapped("org-acme"), &credentials("a", "s"))
            .expect("a fully configured strict snapshot builds");

        assert!(snapshot.default.is_none());
        assert_eq!(
            snapshot
                .by_org
                .get("org-acme")
                .and_then(|store| store.bucket()),
            Some("mw-acme")
        );
        assert_eq!(
            snapshot.health.as_ref().and_then(|store| store.bucket()),
            Some("mw-health")
        );
    }

    #[test]
    fn a_non_strict_snapshot_routes_unmapped_orgs_to_the_shared_store() {
        let mut config = strict_config();
        config.require_org_bucket = false;
        config.health_bucket = None;
        config.bucket = Some("shared".into());

        let snapshot = build_snapshot(&config, &BTreeMap::new(), &BTreeMap::new())
            .expect("a shared bucket is enough outside strict mode");

        assert_eq!(
            snapshot.default.as_ref().and_then(|store| store.bucket()),
            Some("shared")
        );
        assert!(snapshot.by_org.is_empty());
        assert!(snapshot.health.is_none());
    }

    #[test]
    fn an_entry_for_an_unknown_org_is_left_out() {
        let mut config = strict_config();
        config.organizations.insert(
            "ghost".to_string(),
            ObjectStoreOrgConfig {
                bucket: "mw-ghost".into(),
                ..Default::default()
            },
        );

        let snapshot = build_snapshot(&config, &BTreeMap::new(), &BTreeMap::new())
            .expect("an unknown org must not reject the snapshot");

        assert!(snapshot.by_org.is_empty());
    }

    #[test]
    fn entry_overrides_win_over_the_shared_values() {
        let mut config = strict_config();
        config.organizations.insert(
            "acme".to_string(),
            ObjectStoreOrgConfig {
                bucket: "mw-acme".into(),
                endpoint: Some("http://127.0.0.1:3901".into()),
                region: Some("other".into()),
                path_style: Some(false),
            },
        );

        let snapshot = build_snapshot(&config, &mapped("org-acme"), &credentials("a", "s"))
            .expect("overrides build");

        assert_eq!(
            snapshot
                .by_org
                .get("org-acme")
                .and_then(|store| store.bucket()),
            Some("mw-acme")
        );
    }

    #[test]
    fn a_local_config_builds_an_empty_snapshot() {
        let config = ObjectStoreConfig {
            backend: ObjectStoreBackend::Local,
            ..ObjectStoreConfig::default()
        };

        let snapshot = build_snapshot(&config, &BTreeMap::new(), &BTreeMap::new())
            .expect("local needs no endpoint or credentials");

        assert!(snapshot.default.is_none());
        assert!(snapshot.by_org.is_empty());
    }

    #[test]
    fn missing_shared_values_are_invalid_config_errors() {
        let mut config = strict_config();
        config.endpoint = None;

        let error = build_snapshot(&config, &BTreeMap::new(), &BTreeMap::new())
            .err()
            .expect("an endpoint is required");
        assert!(
            error.to_string().contains("object_store.endpoint"),
            "{error}"
        );

        let mut config = strict_config();
        config.require_org_bucket = false;
        let error = build_snapshot(&config, &BTreeMap::new(), &BTreeMap::new())
            .err()
            .expect("a shared bucket is required outside strict mode");
        assert!(error.to_string().contains("object_store.bucket"), "{error}");
    }
}
