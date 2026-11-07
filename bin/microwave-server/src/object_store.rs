//! Builds the object-store routing snapshot from the configured tenant map.
//!
//! Installed by startup and, later, swapped by the reloader; both need the
//! build without the install, so the two stay separate.

use microwave_config::{
    Config, ObjectStoreBackend, ObjectStoreConfig, OrgCredentials, TenantIssue,
};
use microwave_objectstore::{
    LocalObjectStore, ObjectStore, ObjectStoreError, S3Config, S3ObjectStore, TenantSnapshot,
};
use microwave_store::MetadataStore;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
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

/// Build a complete snapshot for the given configuration: the `local` default,
/// or the tenant cross-checks followed by [`build_snapshot`].
///
/// Startup and the reloader share this, so the `local` branch exists once. The
/// caller installs the result (and the reloader probes it first).
pub(crate) async fn assemble(
    config: &Config,
    store: &dyn MetadataStore,
    secret: &str,
) -> Result<TenantSnapshot, Box<dyn std::error::Error>> {
    if config.object_store.backend == ObjectStoreBackend::Local {
        let local: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
            Path::new(&config.paths.data_dir).join("bookshelf"),
            secret,
            crate::local_object_store_base_url(config),
        ));
        let mut snapshot =
            build_snapshot(&config.object_store, &BTreeMap::new(), &BTreeMap::new())?;
        snapshot.default = Some(local);
        return Ok(snapshot);
    }

    let organizations = store.list_orgs().await?;
    let organization_names: BTreeSet<String> = organizations
        .iter()
        .map(|organization| organization.name.clone())
        .collect();
    // Only entries that name an existing organization need their secrets read.
    let org_ids: BTreeMap<String, String> = organizations
        .iter()
        .filter(|organization| {
            config
                .object_store
                .organizations
                .contains_key(&organization.name)
        })
        .map(|organization| (organization.name.clone(), organization.id.clone()))
        .collect();
    let configured: BTreeSet<String> = org_ids.keys().cloned().collect();
    let credentials = crate::bootstrap::load_org_credentials(store, &configured).await?;

    let mut fatal = Vec::new();
    for issue in
        microwave_config::tenant_issues(&config.object_store, &organization_names, &credentials)
    {
        match issue {
            TenantIssue::UnknownOrg(organization) => tracing::warn!(
                organization = %organization,
                "object_store.organizations names an unknown organization; its entry stays inactive"
            ),
            issue => fatal.push(issue),
        }
    }
    if !fatal.is_empty() {
        let issues: Vec<String> = fatal.iter().map(ToString::to_string).collect();
        return Err(format!("object store tenant map is invalid: {}", issues.join("; ")).into());
    }

    Ok(build_snapshot(
        &config.object_store,
        &org_ids,
        &credentials,
    )?)
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
        let credentials = credentials("acme-access", "acme-secret");

        let snapshot = build_snapshot(&config, &mapped("org-acme"), &credentials)
            .expect("the overridden entry builds");
        let store = snapshot
            .by_org
            .get("org-acme")
            .expect("the configured org is routed");

        assert_eq!(store.bucket(), Some("mw-acme"));
        // The URL pins the whole call-site wiring: the entry's endpoint becomes
        // the host, its region the credential scope, its access key the signer.
        // Swapping the `endpoint`/`region` arguments, which are both
        // `Option<&String>`, would dial region-as-host here.
        let url = store
            .presigned_url("GET", "org-acme", "0123456789abcdef0123456789abcdef")
            .expect("the overridden entry signs");
        assert!(url.contains("127.0.0.1:3901"), "{url}");
        assert!(!url.contains("127.0.0.1:3900"), "{url}");
        assert!(url.contains("other"), "{url}");
        assert!(url.contains("acme-access"), "{url}");

        // `secret_key` and `path_style` never reach a URL.
        let store = super::SharedTarget::new(&config)
            .expect("the shared target builds")
            .entry_store("mw-acme", None, None, Some(false), credentials.get("acme"))
            .expect("the entry builds");
        assert_eq!(store.config().secret_key, "acme-secret");
        assert!(!store.config().path_style);
    }

    #[test]
    fn blank_org_credentials_fall_back_to_the_shared_values() {
        let config = strict_config();

        for credentials in [
            None,
            Some(OrgCredentials {
                access_key: String::new(),
                secret_key: String::new(),
            }),
            Some(OrgCredentials {
                access_key: "   ".into(),
                secret_key: "   ".into(),
            }),
        ] {
            let store = super::SharedTarget::new(&config)
                .expect("the shared target builds")
                .entry_store("mw-acme", None, None, None, credentials.as_ref())
                .expect("the entry builds");

            let merged = store.config();
            assert_eq!(merged.access_key, "shared-access");
            assert_eq!(merged.secret_key, "shared-secret");
            assert_eq!(merged.endpoint, "http://127.0.0.1:3900");
            assert_eq!(merged.region, "garage");
            assert!(merged.path_style);
        }
    }

    /// The shared default and the health store keep the global target and
    /// never take the credentials of an entry.
    #[test]
    fn the_default_and_health_stores_use_the_global_values() {
        let config = strict_config();

        let shared = super::SharedTarget::new(&config).expect("the shared target builds");
        let default = shared.store("shared").expect("the default store");
        let health = shared.store("mw-health").expect("the health store");

        assert_eq!(default.config().bucket, "shared");
        assert_eq!(health.config().bucket, "mw-health");
        assert_eq!(default.config().endpoint, "http://127.0.0.1:3900");
        assert_eq!(health.config().endpoint, "http://127.0.0.1:3900");
        assert_eq!(default.config().region, "garage");
        assert_eq!(health.config().region, "garage");
        assert_eq!(default.config().access_key, "shared-access");
        assert_eq!(health.config().secret_key, "shared-secret");
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

        let mut config = strict_config();
        config.access_key = None;
        let error = build_snapshot(&config, &BTreeMap::new(), &BTreeMap::new())
            .err()
            .expect("an access key is required for the health store");
        assert!(
            error.to_string().contains("object_store.access_key"),
            "{error}"
        );

        let mut config = strict_config();
        config.secret_key = Some("   ".into());
        let error = build_snapshot(&config, &BTreeMap::new(), &BTreeMap::new())
            .err()
            .expect("a secret key is required for the health store");
        assert!(
            error.to_string().contains("object_store.secret_key"),
            "{error}"
        );
    }
}
