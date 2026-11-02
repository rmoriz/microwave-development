//! Live crawler for a goiardi server.
//!
//! goiardi speaks a flat variant of the Chef 11 API: no organizations, no
//! groups, no ACL endpoints and no key endpoints. Every request is signed with
//! the configured client key using Chef's `X-Ops-Sign: version=1.3` protocol.
//! The crawler only reads; it never creates, updates, or deletes objects on the
//! source server.
//!
//! A run maps the whole source into exactly one target organization and marks
//! the snapshot with `source.layout = "goiardi"`. goiardi ignores
//! `X-Ops-Server-API-Version`, so the fetcher always signs as API version 2.
//!
//! The source database does not expose internal ids, nor does the flat API
//! expose authz ids. Both are derived deterministically with the `from-api`
//! scheme, so repeated crawls produce the same snapshot.

use crate::crawl::{
    bool_field, cookbook_files, encode_segment, store_blob, string_field, synthetic_i64,
    synthetic_id, url_field, BlobSummary, CollectionEntry, Fetcher,
};
use crate::{
    write_snapshot, MigrateError, Snapshot, SnapshotSource, SnapshotSourceKind, SNAPSHOT_FORMAT,
    SNAPSHOT_VERSION,
};
use chrono::{SecondsFormat, Utc};
use microwave_authz::{Permission, DEFAULT_CONTAINERS, DEFAULT_GROUPS};
use microwave_objectstore::{object_path, LocalObjectStore, ObjectStore};
use microwave_store::{
    parse_version, ActorKeyRow, AuthzAclActorRow, AuthzAclGroupRow, AuthzEntityRow,
    AuthzGroupActorRow, AuthzGroupGroupRow, ChecksumRow, ClientRow, ContainerRow, CookbookRow,
    CookbookVersionChecksumRow, CookbookVersionRow, DataBagItemRow, GlobalGroupRow, GroupRow,
    NamedObjectRow, NodeRow, OrgRow, OrgUserAssociationRow, StoreSnapshot, UserRow,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Options for [`crawl_from_goiardi`] and [`from_goiardi_with_options`].
#[derive(Debug, Clone)]
pub struct GoiardiOptions {
    /// Base URL of the goiardi server, for example `https://chef.example.com`.
    pub url: String,
    /// User or client name used in `X-Ops-Userid`.
    pub client_name: String,
    /// PEM-encoded RSA private key used to sign requests.
    pub key_path: PathBuf,
    /// Snapshot file to write.
    pub output: PathBuf,
    /// Target organization every source object is mapped into.
    pub org: String,
    /// Human-readable organization name; defaults to `org`.
    pub org_full_name: Option<String>,
    /// Local object-store root used to copy cookbook blobs.
    pub bookshelf_dir: Option<PathBuf>,
    /// SOCKS5 proxy as `HOST:PORT`, for sources only reachable from a bastion.
    pub socks5: Option<String>,
    /// Drop clients that have no node of the same name.
    pub skip_orphan_clients: bool,
    /// Crawl and return the snapshot without writing it or copying blobs.
    pub dry_run: bool,
}

impl GoiardiOptions {
    /// Build options with no full name, proxy, orphan filter, or dry run.
    pub fn new(
        url: impl Into<String>,
        client_name: impl Into<String>,
        key_path: impl Into<PathBuf>,
        output: impl Into<PathBuf>,
        org: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            client_name: client_name.into(),
            key_path: key_path.into(),
            output: output.into(),
            org: org.into(),
            org_full_name: None,
            bookshelf_dir: None,
            socks5: None,
            skip_orphan_clients: false,
            dry_run: false,
        }
    }
}

/// Counts and warnings from one goiardi crawl.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoiardiSummary {
    pub users: usize,
    pub clients: usize,
    pub nodes: usize,
    pub roles: usize,
    pub environments: usize,
    pub data_bags: usize,
    pub data_bag_items: usize,
    pub cookbooks: usize,
    pub cookbook_versions: usize,
    pub checksums: usize,
    pub blobs_downloaded: usize,
    pub blobs_skipped: usize,
    pub warnings: Vec<String>,
}

/// Crawl a goiardi server and write a snapshot unless `dry_run` is set.
///
/// Counts and warnings go to stderr as they are collected, so an aborted run
/// still reports what it saw; the snapshot path alone goes to stdout. The
/// written snapshot is validated first: every imported object needs an ACL and
/// every copied blob must exist. The snapshot is returned either way, but a
/// dry run leaves the output path untouched, copies no blobs and skips the
/// blob half of the validation.
pub fn crawl_from_goiardi(
    options: &GoiardiOptions,
) -> Result<(Snapshot, GoiardiSummary), MigrateError> {
    validate_org(&options.org)?;
    let mut crawler = Crawler::new(options)?;
    crawler.crawl_identity()?;
    crawler.crawl_objects()?;
    let org = crawler.org.clone();
    crawler.crawl_cookbooks(&org)?;
    // The policy owns every ACL the snapshot carries: container, group and org
    // grants plus one object ACL per imported object. It has to be complete
    // before the blob copy, so a failure never leaves a half-described store.
    crawler.apply_policy();
    // Everything up to here can still abort; report it before the blob phase
    // so the counts and skip reasons are not lost.
    print_summary(&crawler.summary);
    // Blob work runs last so a failed metadata crawl never leaves a
    // half-populated bookshelf behind, and a dry run downloads nothing.
    if !options.dry_run {
        crawler.download_blobs()?;
        crawler.summary.blobs_downloaded = crawler.blob_summary.downloaded;
        crawler.summary.blobs_skipped = crawler.blob_summary.skipped;
        // The blob counters only exist once the copy ran; they close the
        // table here instead of being repeated in the block above.
        print_blob_counts(&crawler.summary);
    }

    // A dry run copies nothing, so only a real crawl can check the blobs.
    let blob_check = options
        .bookshelf_dir
        .as_deref()
        .filter(|_| !options.dry_run);

    let snapshot = Snapshot {
        format: SNAPSHOT_FORMAT.to_string(),
        version: SNAPSHOT_VERSION,
        generated_at: crawler.generated_at.clone(),
        source: SnapshotSource {
            kind: SnapshotSourceKind::LegacyApi,
            url: Some(options.url.trim_end_matches('/').to_string()),
            data_dir: crawler
                .bookshelf_dir
                .as_ref()
                .map(|path| path.display().to_string()),
            layout: Some("goiardi".to_string()),
        },
        secrets_included: false,
        data: crawler.data,
    };

    // Blobs only exist when they were copied, so a dry run skips that half.
    validate_snapshot(&snapshot, blob_check)?;

    if options.dry_run {
        eprintln!("dry run: no snapshot written");
    } else {
        write_snapshot(&snapshot, &options.output)?;
    }
    println!("{}", options.output.display());
    Ok((snapshot, crawler.summary))
}

/// Crawl a goiardi server, write the snapshot, and return the summary.
pub fn from_goiardi_with_options(options: &GoiardiOptions) -> Result<GoiardiSummary, MigrateError> {
    crawl_from_goiardi(options).map(|(_, summary)| summary)
}

/// Print the crawl counters and every skip warning to stderr.
///
/// The counts are diagnostics; the snapshot path alone goes to stdout so a
/// caller can capture it. The blob counters close the table later, once the
/// copy ran, so an aborted crawl still shows everything up to that point.
fn print_summary(summary: &GoiardiSummary) {
    eprintln!("goiardi crawl summary:");
    for (label, count) in [
        ("users", summary.users),
        ("clients", summary.clients),
        ("nodes", summary.nodes),
        ("roles", summary.roles),
        ("environments", summary.environments),
        ("data bags", summary.data_bags),
        ("data bag items", summary.data_bag_items),
        ("cookbooks", summary.cookbooks),
        ("cookbook versions", summary.cookbook_versions),
        ("checksums", summary.checksums),
    ] {
        eprintln!("  {label}: {count}");
    }
    for warning in &summary.warnings {
        eprintln!("warning: {warning}");
    }
}

/// Close the summary table with the blob counters of the finished copy.
fn print_blob_counts(summary: &GoiardiSummary) {
    eprintln!("  blobs downloaded: {}", summary.blobs_downloaded);
    eprintln!("  blobs skipped: {}", summary.blobs_skipped);
}

/// Reject a snapshot an import would leave half-broken.
///
/// Every `object` authz entity needs at least one actor or group ACL row. When
/// a bookshelf directory is passed — a real run that copied blobs, never a dry
/// run — every checksum row also needs its blob under that directory.
pub fn validate_snapshot(
    snapshot: &Snapshot,
    bookshelf_dir: Option<&Path>,
) -> Result<(), MigrateError> {
    let guarded: HashSet<&str> = snapshot
        .data
        .authz_acl_actor
        .iter()
        .map(|row| row.target_authz_id.as_str())
        .chain(
            snapshot
                .data
                .authz_acl_group
                .iter()
                .map(|row| row.target_authz_id.as_str()),
        )
        .collect();
    let mut unguarded: Vec<String> = snapshot
        .data
        .authz_entities
        .iter()
        .filter(|entity| entity.entity_type == "object")
        .filter(|entity| !guarded.contains(entity.authz_id.as_str()))
        .map(|entity| object_label(&snapshot.data, &entity.authz_id))
        .collect();
    if !unguarded.is_empty() {
        unguarded.sort();
        return Err(MigrateError::Validation(format!(
            "objects without any ACL row: {}",
            unguarded.join(", ")
        )));
    }

    let Some(dir) = bookshelf_dir else {
        return Ok(());
    };
    let mut missing: Vec<String> = snapshot
        .data
        .checksums
        .iter()
        .filter(|row| !object_path(dir, &row.org_id, &row.checksum).is_file())
        .map(|row| match cookbook_of_checksum(snapshot, &row.checksum) {
            Some(cookbook) => format!("{} ({cookbook})", row.checksum),
            None => row.checksum.clone(),
        })
        .collect();
    if !missing.is_empty() {
        missing.sort();
        return Err(MigrateError::Validation(format!(
            "cookbook blobs missing under {}: {}",
            dir.display(),
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Name the row behind an object authz id, for a validation error.
fn object_label(data: &StoreSnapshot, authz_id: &str) -> String {
    if let Some(node) = data.nodes.iter().find(|row| row.authz_id == authz_id) {
        return format!("node {:?}", node.name);
    }
    for (kind, rows) in [
        ("role", &data.roles),
        ("environment", &data.environments),
        ("data bag", &data.data_bags),
    ] {
        if let Some(row) = rows.iter().find(|row| row.authz_id == authz_id) {
            return format!("{kind} {:?}", row.name);
        }
    }
    if let Some(cookbook) = data.cookbooks.iter().find(|row| row.authz_id == authz_id) {
        return format!("cookbook {:?}", cookbook.name);
    }
    format!("authz id {authz_id:?}")
}

/// Name the cookbook version that links a checksum, when one does.
fn cookbook_of_checksum(snapshot: &Snapshot, checksum: &str) -> Option<String> {
    let version_id = snapshot
        .data
        .cookbook_version_checksums
        .iter()
        .find(|row| row.checksum == checksum)
        .map(|row| row.cookbook_version_id.as_str())?;
    let version = snapshot
        .data
        .cookbook_versions
        .iter()
        .find(|row| row.id == version_id)?;
    Some(format!(
        "cookbook {:?} {}.{}.{}",
        version.name, version.major, version.minor, version.patch
    ))
}

/// Crawl state for one goiardi source.
///
/// The layout is shared by every crawl step: identity first, then objects,
/// cookbooks, and policy synthesis. That is why some fields are only filled or
/// read by steps that land in later commits.
#[allow(dead_code)]
pub struct Crawler {
    pub(crate) fetcher: Fetcher,
    /// The single target organization every object is mapped into.
    pub(crate) org: OrgRow,
    pub(crate) generated_at: String,
    pub(crate) data: StoreSnapshot,
    /// User name to synthetic authz id.
    pub(crate) user_authz: HashMap<String, String>,
    /// Client name to synthetic authz id.
    pub(crate) client_authz: HashMap<String, String>,
    /// `(container name, authz id)` of every imported object.
    pub(crate) object_authz: Vec<(String, String)>,
    pub(crate) seen_authz: HashSet<String>,
    pub(crate) seen_checksums: HashSet<String>,
    /// `(cookbook name, version)` of every imported version.
    pub(crate) seen_cookbook_versions: HashSet<(String, String)>,
    /// `(org id, checksum)` to the first URL that advertised the blob.
    pub(crate) pending_blobs: HashMap<(String, String), String>,
    /// Local object store root used to copy cookbook blobs.
    pub(crate) bookshelf_dir: Option<PathBuf>,
    pub(crate) bookshelf: Option<LocalObjectStore>,
    pub(crate) blob_summary: BlobSummary,
    pub(crate) downloaded_blob_bytes: u64,
    /// Every environment name in the source, for node validation.
    pub(crate) environment_names: HashSet<String>,
    /// Drop clients that have no node of the same name.
    pub(crate) skip_orphan_clients: bool,
    pub(crate) summary: GoiardiSummary,
}

impl Crawler {
    fn new(options: &GoiardiOptions) -> Result<Self, MigrateError> {
        let fetcher = Fetcher::new(
            &options.url,
            &options.client_name,
            &options.key_path,
            2,
            options.socks5.as_deref(),
        )?;
        let generated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let org = OrgRow {
            id: synthetic_id(&format!("org:{}", options.org)),
            authz_id: synthetic_id(&format!("authz:org:{}", options.org)),
            name: options.org.clone(),
            full_name: options
                .org_full_name
                .clone()
                .unwrap_or_else(|| options.org.clone()),
            assigned_at: generated_at.clone(),
            last_updated_by: String::new(),
            created_at: generated_at.clone(),
            updated_at: generated_at.clone(),
        };

        let mut crawler = Self {
            fetcher,
            org,
            generated_at,
            data: StoreSnapshot::default(),
            user_authz: HashMap::new(),
            client_authz: HashMap::new(),
            object_authz: Vec::new(),
            seen_authz: HashSet::new(),
            seen_checksums: HashSet::new(),
            seen_cookbook_versions: HashSet::new(),
            pending_blobs: HashMap::new(),
            bookshelf_dir: options.bookshelf_dir.clone(),
            bookshelf: options
                .bookshelf_dir
                .as_ref()
                .map(|dir| LocalObjectStore::new(dir.clone(), "", "http://localhost")),
            blob_summary: BlobSummary::default(),
            downloaded_blob_bytes: 0,
            environment_names: HashSet::new(),
            skip_orphan_clients: options.skip_orphan_clients,
            summary: GoiardiSummary::default(),
        };

        let authz_id = crawler.org.authz_id.clone();
        let org_id = crawler.org.id.clone();
        crawler.ensure_entity(&authz_id, "actor", Some(&org_id));
        crawler.data.orgs.push(crawler.org.clone());
        Ok(crawler)
    }

    fn crawl_identity(&mut self) -> Result<(), MigrateError> {
        let org_id = self.org.id.clone();
        self.crawl_users(&org_id)?;
        self.crawl_clients(&org_id)?;
        Ok(())
    }

    fn crawl_objects(&mut self) -> Result<(), MigrateError> {
        let org = self.org.clone();
        // The policy creates `_default`, so it is a valid name even when the
        // source does not list it. Environments come first so node validation
        // knows every other valid name too.
        self.environment_names.insert("_default".to_string());
        self.crawl_named_objects(&org)?;
        self.crawl_nodes(&org)?;
        if self.skip_orphan_clients {
            let node_names = self
                .data
                .nodes
                .iter()
                .map(|node| node.name.clone())
                .collect();
            self.filter_orphan_clients(&node_names);
        }
        self.crawl_data_bags(&org)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Nodes, roles, environments, data bags
    // ------------------------------------------------------------------

    fn crawl_nodes(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection("/nodes")?;
        for entry in entries {
            let list_name = entry.name.clone();
            let fallback = format!("/nodes/{}", encode_segment(&list_name));
            let Some((value, raw)) = self.entry_raw(&entry, &fallback)? else {
                continue;
            };
            let name = string_field(&value, "name").unwrap_or(list_name);
            let environment =
                string_field(&value, "chef_environment").unwrap_or_else(|| "_default".to_string());
            if !self.environment_names.contains(&environment) {
                self.summary.warnings.push(format!(
                    "node {name:?} names unknown environment {environment:?}; imported anyway"
                ));
            }

            let id = synthetic_id(&format!("node:{}:{name}", org.name));
            let authz_id = synthetic_id(&format!("authz:node:{}:{name}", org.name));
            self.ensure_entity(&authz_id, "object", Some(&org.id));

            self.data.nodes.push(NodeRow {
                id,
                authz_id: authz_id.clone(),
                org_id: org.id.clone(),
                name,
                environment,
                policy_name: non_empty_string(&value, "policy_name"),
                policy_group: non_empty_string(&value, "policy_group"),
                serialized_object: Some(raw),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.object_authz.push(("nodes".to_string(), authz_id));
            self.summary.nodes += 1;
        }
        Ok(())
    }

    fn crawl_named_objects(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        for (kind, collection) in [("role", "roles"), ("environment", "environments")] {
            let entries = self.fetcher.get_collection(&format!("/{collection}"))?;
            if kind == "environment" {
                // Node validation needs every name, `_default` included.
                self.environment_names
                    .extend(entries.iter().map(|entry| entry.name.clone()));
            }
            for entry in entries {
                let list_name = entry.name.clone();
                if kind == "environment" && list_name == "_default" {
                    continue;
                }
                let fallback = format!("/{collection}/{}", encode_segment(&list_name));
                let Some((value, raw)) = self.entry_raw(&entry, &fallback)? else {
                    continue;
                };
                let name = string_field(&value, "name").unwrap_or(list_name);
                let id = synthetic_id(&format!("{kind}:{}:{name}", org.name));
                let authz_id = synthetic_id(&format!("authz:{kind}:{}:{name}", org.name));
                self.ensure_entity(&authz_id, "object", Some(&org.id));

                let row = NamedObjectRow {
                    id,
                    authz_id: authz_id.clone(),
                    org_id: org.id.clone(),
                    name,
                    serialized_object: Some(raw),
                    last_updated_by: String::new(),
                    created_at: self.generated_at.clone(),
                    updated_at: self.generated_at.clone(),
                };
                match kind {
                    "role" => {
                        self.data.roles.push(row);
                        self.summary.roles += 1;
                    }
                    "environment" => {
                        self.data.environments.push(row);
                        self.summary.environments += 1;
                    }
                    _ => unreachable!(),
                }
                self.object_authz.push((collection.to_string(), authz_id));
            }
        }
        Ok(())
    }

    fn crawl_data_bags(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection("/data")?;
        for entry in entries {
            let name = entry.name.clone();
            let id = synthetic_id(&format!("data_bag:{}:{name}", org.name));
            let authz_id = synthetic_id(&format!("authz:data_bag:{}:{name}", org.name));
            self.ensure_entity(&authz_id, "object", Some(&org.id));

            // goiardi has no data bag metadata route; the list name is the bag.
            let raw = serde_json::to_string(&serde_json::json!({
                "name": name,
                "json_class": "Chef::DataBag",
                "chef_type": "data_bag"
            }))?;
            self.data.data_bags.push(NamedObjectRow {
                id,
                authz_id: authz_id.clone(),
                org_id: org.id.clone(),
                name: name.clone(),
                serialized_object: Some(raw),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.object_authz.push(("data".to_string(), authz_id));
            self.summary.data_bags += 1;

            let items_path = entry
                .url
                .clone()
                .unwrap_or_else(|| format!("/data/{}", encode_segment(&name)));
            let items = self.fetcher.get_collection(&items_path)?;
            for item in items {
                let item_list_name = item.name.clone();
                let fallback = format!(
                    "/data/{}/{}",
                    encode_segment(&name),
                    encode_segment(&item_list_name)
                );
                let Some((item_value, item_raw)) = self.entry_raw(&item, &fallback)? else {
                    continue;
                };
                let Some(item_name) = non_empty_string(&item_value, "id") else {
                    self.summary.warnings.push(format!(
                        "data bag item {name:?}/{item_list_name:?} has no id; skipped"
                    ));
                    continue;
                };
                self.data.data_bag_items.push(DataBagItemRow {
                    id: synthetic_id(&format!("data_bag_item:{}:{name}:{item_name}", org.name)),
                    org_id: org.id.clone(),
                    data_bag_name: name.clone(),
                    item_name,
                    serialized_object: Some(item_raw),
                    last_updated_by: String::new(),
                    created_at: self.generated_at.clone(),
                    updated_at: self.generated_at.clone(),
                });
                self.summary.data_bag_items += 1;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Cookbooks and blobs
    // ------------------------------------------------------------------

    fn crawl_cookbooks(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection("/cookbooks")?;
        for entry in entries {
            let list_name = entry.name.clone();
            let value = match &entry.url {
                Some(url) => self.fetcher.get_optional_json(&self.fetcher.resolve(url))?,
                None => Some(entry.value.clone()),
            };
            let Some(value) = value else { continue };
            let value = unwrap_cookbook(value, &list_name);
            let name = string_field(&value, "name").unwrap_or(list_name);

            let cookbook_id = synthetic_i64(&format!("cookbook:{}:{name}", org.name));
            let authz_id = synthetic_id(&format!("authz:cookbook:{}:{name}", org.name));
            self.ensure_entity(&authz_id, "object", Some(&org.id));
            if !self
                .data
                .cookbooks
                .iter()
                .any(|cookbook| cookbook.id == cookbook_id)
            {
                self.data.cookbooks.push(CookbookRow {
                    id: cookbook_id,
                    org_id: org.id.clone(),
                    name: name.clone(),
                    authz_id: authz_id.clone(),
                });
                self.object_authz.push(("cookbooks".to_string(), authz_id));
                self.summary.cookbooks += 1;
            }

            let versions = cookbook_versions(&value);
            if versions.is_empty() {
                self.summary.warnings.push(format!(
                    "cookbook {name:?} lists no versions; imported without any"
                ));
            }
            for (version, version_url) in versions {
                if !self
                    .seen_cookbook_versions
                    .insert((name.clone(), version.clone()))
                {
                    continue;
                }
                let version_path = version_url.unwrap_or_else(|| {
                    format!(
                        "/cookbooks/{}/{}",
                        encode_segment(&name),
                        encode_segment(&version)
                    )
                });
                let version_url = self.fetcher.resolve(&version_path);
                let Some((version_value, raw)) = self.fetcher.get_optional_raw(&version_url)?
                else {
                    self.summary.warnings.push(format!(
                        "cookbook {name:?} version {version:?} detail answered 404; skipped"
                    ));
                    continue;
                };
                if self.push_cookbook_version(
                    org,
                    &name,
                    cookbook_id,
                    &version,
                    &version_value,
                    raw,
                ) {
                    self.summary.cookbook_versions += 1;
                }
            }
        }
        Ok(())
    }

    fn push_cookbook_version(
        &mut self,
        org: &OrgRow,
        name: &str,
        cookbook_id: i64,
        fallback_version: &str,
        value: &Value,
        raw: String,
    ) -> bool {
        let version =
            string_field(value, "version").unwrap_or_else(|| fallback_version.to_string());
        let Ok((major, minor, patch)) = parse_version(&version) else {
            return false;
        };
        let metadata = value.get("metadata").cloned().unwrap_or(Value::Null);
        let metadata_text = if metadata.is_null() {
            None
        } else {
            serde_json::to_string(&metadata).ok()
        };
        let meta_attributes = metadata
            .get("attributes")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::to_string(value).ok());
        let meta_deps = metadata
            .get("dependencies")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::to_string(value).ok());
        let meta_long_desc = match metadata.get("long_description") {
            Some(Value::String(text)) => Some(text.clone()),
            Some(value) if !value.is_null() => serde_json::to_string(value).ok(),
            _ => None,
        };

        let version_id = synthetic_id(&format!("cookbook_version:{}:{name}:{version}", org.name));
        self.data.cookbook_versions.push(CookbookVersionRow {
            id: version_id.clone(),
            cookbook_id,
            org_id: org.id.clone(),
            name: name.to_string(),
            major,
            minor,
            patch,
            frozen: bool_field(value, "frozen?").unwrap_or(false),
            meta_attributes,
            meta_deps,
            meta_long_desc,
            metadata: metadata_text,
            serialized_object: Some(raw),
            last_updated_by: String::new(),
            created_at: self.generated_at.clone(),
            updated_at: self.generated_at.clone(),
        });

        for (checksum, file_url) in cookbook_files(value) {
            self.push_file(org, &version_id, &checksum, file_url.as_deref());
        }
        true
    }

    /// Link one cookbook file checksum to a version.
    ///
    /// A manifest entry without a URL (or with a relative one) is fetched from
    /// the API host's `/file_store/<checksum>` route once blobs are copied;
    /// `store_blob` resolves the reference against the base URL.
    fn push_file(&mut self, org: &OrgRow, version_id: &str, checksum: &str, url: Option<&str>) {
        if self.seen_checksums.insert(checksum.to_string()) {
            self.data.checksums.push(ChecksumRow {
                org_id: org.id.clone(),
                checksum: checksum.to_string(),
                created_at: self.generated_at.clone(),
            });
            self.summary.checksums += 1;
        }
        self.data
            .cookbook_version_checksums
            .push(CookbookVersionChecksumRow {
                cookbook_version_id: version_id.to_string(),
                org_id: org.id.clone(),
                checksum: checksum.to_string(),
            });
        if self.bookshelf.is_some() {
            let reference = url
                .map(str::to_string)
                .unwrap_or_else(|| format!("/file_store/{checksum}"));
            self.pending_blobs
                .entry((org.id.clone(), checksum.to_string()))
                .or_insert(reference);
        }
    }

    /// Copy every collected cookbook blob that is not already in the target
    /// local object store. Runs after the metadata crawl so a failed crawl
    /// never leaves a half-populated bookshelf behind.
    fn download_blobs(&mut self) -> Result<(), MigrateError> {
        let Some(store) = self.bookshelf.clone() else {
            return Ok(());
        };
        let mut pending: Vec<((String, String), String)> = std::mem::take(&mut self.pending_blobs)
            .into_iter()
            .collect();
        pending.sort_by(|left, right| left.0.cmp(&right.0));

        for ((org_id, checksum), reference) in pending {
            if store.exists(&org_id, &checksum) {
                self.blob_summary.skipped += 1;
                continue;
            }

            store_blob(
                &store,
                &org_id,
                &checksum,
                &reference,
                &self.fetcher,
                &mut self.blob_summary,
                &mut self.downloaded_blob_bytes,
            )?;
        }
        Ok(())
    }

    /// Drop clients that have no node of the same name.
    ///
    /// `chef-validator` and `chef-webui` always stay. Every row that refers to
    /// a dropped client is removed with it, so the policy synthesis cannot
    /// point at an actor that no longer exists.
    fn filter_orphan_clients(&mut self, node_names: &HashSet<String>) {
        const ALWAYS_KEEP: [&str; 2] = ["chef-validator", "chef-webui"];
        let mut dropped = Vec::new();
        self.data.clients.retain(|client| {
            let keep =
                node_names.contains(&client.name) || ALWAYS_KEEP.contains(&client.name.as_str());
            if !keep {
                dropped.push(client.clone());
            }
            keep
        });

        for client in &dropped {
            self.data.actor_keys.retain(|key| key.actor_id != client.id);
            self.data
                .authz_entities
                .retain(|entity| entity.authz_id != client.authz_id);
            self.seen_authz.remove(&client.authz_id);
            self.client_authz.remove(&client.name);
        }
        self.summary.clients -= dropped.len();
    }

    // ------------------------------------------------------------------
    // Users and clients
    // ------------------------------------------------------------------

    fn crawl_users(&mut self, org_id: &str) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection("/users")?;
        for entry in entries {
            let Some(value) = self.entry_value(&entry.url, &entry.value)? else {
                continue;
            };

            let username = string_field(&value, "username").unwrap_or(entry.name);
            // Users are global actors in Chef Server and goiardi alike, so the
            // id is not org-scoped; clients below are org-scoped.
            let id = synthetic_id(&format!("user:{username}"));
            let authz_id = synthetic_id(&format!("authz:user:{username}"));
            self.ensure_entity(&authz_id, "actor", None);
            self.user_authz.insert(username.clone(), authz_id.clone());

            self.data.users.push(UserRow {
                id: id.clone(),
                authz_id,
                username: username.clone(),
                email: non_empty_string(&value, "email"),
                password_hash: String::new(),
                password_salt: String::new(),
                hash_type: "bcrypt".to_string(),
                admin: bool_field(&value, "admin").unwrap_or(false),
                display_name: non_empty_string(&value, "name").or_else(|| Some(username.clone())),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
                ..UserRow::default()
            });
            self.summary.users += 1;

            self.push_inline_key(&value, &id, "user", &username);
            if username == "admin" {
                self.data.org_user_associations.push(OrgUserAssociationRow {
                    org_id: org_id.to_string(),
                    user_id: id,
                    last_updated_by: "admin".to_string(),
                    created_at: self.generated_at.clone(),
                    updated_at: self.generated_at.clone(),
                });
            }
        }
        Ok(())
    }

    fn crawl_clients(&mut self, org_id: &str) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection("/clients")?;
        for entry in entries {
            let Some(value) = self.entry_value(&entry.url, &entry.value)? else {
                continue;
            };

            let name = string_field(&value, "name").unwrap_or(entry.name);
            let id = synthetic_id(&format!("client:{}:{name}", self.org.name));
            let authz_id = synthetic_id(&format!("authz:client:{}:{name}", self.org.name));
            self.ensure_entity(&authz_id, "actor", Some(org_id));
            self.client_authz.insert(name.clone(), authz_id.clone());

            self.data.clients.push(ClientRow {
                id: id.clone(),
                org_id: org_id.to_string(),
                authz_id,
                name: name.clone(),
                validator: bool_field(&value, "validator").unwrap_or(false),
                admin: bool_field(&value, "admin").unwrap_or(false),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.summary.clients += 1;

            self.push_inline_key(&value, &id, "client", &name);
        }
        Ok(())
    }

    /// Import the inline `public_key` of a flat goiardi actor.
    fn push_inline_key(&mut self, actor: &Value, actor_id: &str, actor_type: &str, name: &str) {
        match non_empty_string(actor, "public_key") {
            Some(public_key) => self.push_actor_key(actor_id, actor_type, "default", &public_key),
            None => self.summary.warnings.push(format!(
                "{actor_type} {name:?} has no public key; imported without one"
            )),
        }
    }

    fn push_actor_key(
        &mut self,
        actor_id: &str,
        actor_type: &str,
        key_name: &str,
        public_key: &str,
    ) {
        if self
            .data
            .actor_keys
            .iter()
            .any(|key| key.actor_id == actor_id && key.key_name == key_name)
        {
            return;
        }
        self.data.actor_keys.push(ActorKeyRow {
            actor_id: actor_id.to_string(),
            actor_type: actor_type.to_string(),
            key_name: key_name.to_string(),
            public_key: public_key.to_string(),
            key_version: 1,
            expires_at: "infinity".to_string(),
            last_updated_by: String::new(),
            created_at: self.generated_at.clone(),
            updated_at: self.generated_at.clone(),
        });
    }

    // ------------------------------------------------------------------
    // Default policy
    // ------------------------------------------------------------------

    /// Synthesize the default organization policy over the crawled rows.
    ///
    /// goiardi exports neither organizations nor ACLs, so the whole source is
    /// mapped into the one target org and the policy the server would have
    /// written is rebuilt here. `admin` is the creator; it is the source
    /// superuser and the only user the migration needs.
    fn apply_policy(&mut self) {
        let admin_authz = self
            .user_authz
            .get("admin")
            .cloned()
            .unwrap_or_else(|| synthetic_id("authz:user:admin"));
        // The source account may have been renamed, but the policy and the org
        // association need an `admin` user row either way. Synthesize one
        // instead of failing; the operator resets its password after import.
        if !self
            .data
            .users
            .iter()
            .any(|user| user.authz_id == admin_authz)
        {
            self.data.users.push(UserRow {
                id: synthetic_id("user:admin"),
                authz_id: admin_authz.clone(),
                username: "admin".to_string(),
                email: None,
                password_hash: String::new(),
                password_salt: String::new(),
                hash_type: "bcrypt".to_string(),
                admin: true,
                recovery_authentication_enabled: true,
                last_updated_by: "admin".to_string(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
                ..UserRow::default()
            });
            self.summary.users += 1;
            self.summary.warnings.push(
                "source has no user named \"admin\"; synthesized one for the organization association"
                    .to_string(),
            );
        }
        // Sorted so repeated crawls write the same rows in the same order.
        let mut client_authz: Vec<(String, bool)> = self
            .client_authz
            .values()
            .map(|authz_id| {
                let validator = self
                    .data
                    .clients
                    .iter()
                    .any(|client| client.authz_id == *authz_id && client.validator);
                (authz_id.clone(), validator)
            })
            .collect();
        client_authz.sort_unstable();
        let object_authz = self.object_authz.clone();
        let org = self.org.clone();
        apply_default_policy(
            &mut self.data,
            &org,
            &admin_authz,
            &client_authz,
            &object_authz,
            &self.generated_at,
        );
        // The policy appends the `_default` environment the source crawl skips,
        // so the reported count has to come from the snapshot.
        self.summary.environments = self.data.environments.len();
    }

    // ------------------------------------------------------------------
    // JSON helpers
    // ------------------------------------------------------------------

    /// Return the detail body for a collection entry.
    ///
    /// Lists are name to URL maps, so the detail is normally fetched. An
    /// embedded object is used as-is, and a 404 skips the entry.
    fn entry_value(
        &self,
        url: &Option<String>,
        embedded: &Value,
    ) -> Result<Option<Value>, MigrateError> {
        if let Some(url) = url {
            return self.fetcher.get_optional_json(&self.fetcher.resolve(url));
        }
        if embedded.is_null() {
            return Ok(None);
        }
        Ok(Some(embedded.clone()))
    }

    /// Return the detail body of a collection entry together with its raw
    /// text. Embedded objects fall back to their serialized form.
    fn entry_raw(
        &self,
        entry: &CollectionEntry,
        fallback_path: &str,
    ) -> Result<Option<(Value, String)>, MigrateError> {
        if let Some(url) = &entry.url {
            return self.fetcher.get_optional_raw(&self.fetcher.resolve(url));
        }
        if !entry.value.is_null() {
            let raw = serde_json::to_string(&entry.value)?;
            return Ok(Some((entry.value.clone(), raw)));
        }
        self.fetcher
            .get_optional_raw(&self.fetcher.resolve(fallback_path))
    }

    fn ensure_entity(&mut self, authz_id: &str, entity_type: &str, org_id: Option<&str>) {
        if self.seen_authz.insert(authz_id.to_string()) {
            self.data.authz_entities.push(AuthzEntityRow {
                authz_id: authz_id.to_string(),
                entity_type: entity_type.to_string(),
                org_id: org_id.map(str::to_string),
                created_at: self.generated_at.clone(),
            });
        }
    }
}

/// The upstream body of the `_default` environment, verbatim from
/// `microwave-authz`'s policy so an imported environment is indistinguishable
/// from a created one.
const DEFAULT_ENVIRONMENT_BODY: &str = r#"{"name":"_default","description":"The default Chef environment","json_class":"Chef::Environment","chef_type":"environment","cookbook_versions":{},"default_attributes":{},"override_attributes":{}}"#;

/// Write the default organization policy into a snapshot.
///
/// goiardi has no organizations, groups or ACL endpoints, so the importer maps
/// the whole source into one organization and synthesizes the policy the
/// server would have written. The rows, their order and their ids mirror
/// `microwave_authz::org`, with `synthetic_id` instead of random uuids:
/// `tests/goiardi_policy_test.rs` compares the result against a real
/// `create_organization` so server policy drift fails the suite.
pub fn apply_default_policy(
    data: &mut StoreSnapshot,
    org: &OrgRow,
    admin_authz: &str,
    client_authz: &[(String, bool)],
    object_authz: &[(String, String)],
    generated_at: &str,
) {
    // The source user is literally `admin`; fall back to its synthetic id when
    // the source did not export one.
    let admin_user_id = data
        .users
        .iter()
        .find(|user| user.authz_id == admin_authz)
        .map(|user| user.id.clone())
        .unwrap_or_else(|| synthetic_id("user:admin"));
    ensure_entity(data, admin_authz, "actor", None, generated_at);

    let mut containers = Vec::with_capacity(DEFAULT_CONTAINERS.len());
    for name in DEFAULT_CONTAINERS {
        let authz_id = synthetic_id(&format!("authz:container:{}:{name}", org.name));
        ensure_entity(data, &authz_id, "container", Some(&org.id), generated_at);
        data.containers.push(ContainerRow {
            id: synthetic_id(&format!("container:{}:{name}", org.name)),
            org_id: org.id.clone(),
            authz_id: authz_id.clone(),
            name: name.to_string(),
            created_at: generated_at.to_string(),
            updated_at: generated_at.to_string(),
        });
        containers.push((name, authz_id));
    }

    let mut groups = Vec::with_capacity(DEFAULT_GROUPS.len());
    for name in DEFAULT_GROUPS {
        let authz_id = synthetic_id(&format!("authz:group:{}:{name}", org.name));
        ensure_entity(data, &authz_id, "group", Some(&org.id), generated_at);
        data.groups.push(GroupRow {
            id: synthetic_id(&format!("group:{}:{name}", org.name)),
            org_id: org.id.clone(),
            authz_id: authz_id.clone(),
            name: name.to_string(),
            created_at: generated_at.to_string(),
            updated_at: generated_at.to_string(),
        });
        groups.push((name, authz_id));
    }

    // Upstream creates a global `<org>_read_access_group` and makes the org's
    // admins and users groups members of it. It is rendered as
    // `::<org>_read_access_group` in ACL responses. `server-admins` is kept
    // available for the superuser bootstrap path.
    let read_access = push_global_group(
        data,
        &format!("{}_read_access_group", org.name),
        generated_at,
    );
    let server_admins = push_global_group(data, "server-admins", generated_at);

    // `create_user` gives every user ALL on itself and hands `server-admins`
    // create/read/update/delete on it; admins also join that group.
    let users_acls: Vec<(String, bool)> = data
        .users
        .iter()
        .map(|user| (user.authz_id.clone(), user.admin))
        .collect();
    for (authz_id, admin) in users_acls {
        grant_actor_all(data, &authz_id, &authz_id);
        grant_group(
            data,
            &authz_id,
            &[
                Permission::Create,
                Permission::Read,
                Permission::Update,
                Permission::Delete,
            ],
            &server_admins,
        );
        if admin {
            add_group_member(data, &server_admins, &authz_id);
        }
    }

    let admins = group_authz(&groups, "admins");
    let users = group_authz(&groups, "users");
    let clients = group_authz(&groups, "clients");
    let public_key_read_access = group_authz(&groups, "public_key_read_access");

    if !data
        .org_user_associations
        .iter()
        .any(|row| row.org_id == org.id && row.user_id == admin_user_id)
    {
        data.org_user_associations.push(OrgUserAssociationRow {
            org_id: org.id.clone(),
            user_id: admin_user_id,
            last_updated_by: "admin".to_string(),
            created_at: generated_at.to_string(),
            updated_at: generated_at.to_string(),
        });
    }
    // `associate_user_org` also lets the read-access group read the user.
    push_acl_group(data, admin_authz, "read", &read_access);

    add_group_member(data, admins, admin_authz);
    add_group_member(data, users, admin_authz);
    for (client, _) in client_authz {
        add_group_member(data, clients, client);
    }
    add_group_child(data, &read_access, admins);
    add_group_child(data, &read_access, users);
    add_group_child(data, public_key_read_access, users);
    add_group_child(data, public_key_read_access, clients);

    // The creator gets every permission on every object created by the policy.
    for (_, container) in &containers {
        grant_actor_all(data, container, admin_authz);
        grant_group_all(data, container, admins);
    }
    for (_, group) in &groups {
        grant_actor_all(data, group, admin_authz);
    }
    grant_actor_all(data, &read_access, admin_authz);
    for name in ["admins", "clients", "users"] {
        grant_group_all(data, group_authz(&groups, name), admins);
    }
    grant_group(
        data,
        public_key_read_access,
        &[Permission::Read, Permission::Update],
        admins,
    );
    grant_actor_all(data, &org.authz_id, admin_authz);
    grant_group_all(data, &org.authz_id, admins);

    // Normal users can manage cookbooks, data, nodes, roles, environments and
    // policies.
    const USERS_MANAGE: [&str; 8] = [
        "cookbooks",
        "data",
        "nodes",
        "roles",
        "environments",
        "policies",
        "policy_groups",
        "cookbook_artifacts",
    ];
    for name in USERS_MANAGE {
        grant_group(
            data,
            container_authz(&containers, name),
            &[
                Permission::Create,
                Permission::Read,
                Permission::Update,
                Permission::Delete,
            ],
            users,
        );
    }
    grant_group(
        data,
        container_authz(&containers, "clients"),
        &[Permission::Read, Permission::Delete],
        users,
    );
    grant_group(
        data,
        container_authz(&containers, "sandboxes"),
        &[Permission::Create],
        users,
    );
    for name in ["groups", "containers"] {
        grant_group(
            data,
            container_authz(&containers, name),
            &[Permission::Read],
            users,
        );
    }
    grant_group(data, &org.authz_id, &[Permission::Read], users);

    // Clients may update nodes and read the objects needed for a converge.
    grant_group(
        data,
        container_authz(&containers, "nodes"),
        &[Permission::Read, Permission::Create],
        clients,
    );
    for name in [
        "policies",
        "policy_groups",
        "cookbook_artifacts",
        "data",
        "cookbooks",
        "environments",
        "roles",
    ] {
        grant_group(
            data,
            container_authz(&containers, name),
            &[Permission::Read],
            clients,
        );
    }

    // Billing admins can read and update the billing-admins group.
    let billing_admins = group_authz(&groups, "billing-admins");
    grant_actor(
        data,
        billing_admins,
        &[Permission::Read, Permission::Update],
        admin_authz,
    );
    grant_group(
        data,
        billing_admins,
        &[Permission::Read, Permission::Update],
        billing_admins,
    );

    // Create the default environment only after every container ACL is in
    // place: object ACLs are copied from their container, and creating it
    // earlier would leave clients without the read grant they need.
    let environment_authz = synthetic_id(&format!("authz:environment:{}:_default", org.name));
    ensure_entity(
        data,
        &environment_authz,
        "object",
        Some(&org.id),
        generated_at,
    );
    data.environments.push(NamedObjectRow {
        id: synthetic_id(&format!("environment:{}:_default", org.name)),
        authz_id: environment_authz.clone(),
        org_id: org.id.clone(),
        name: "_default".to_string(),
        serialized_object: Some(DEFAULT_ENVIRONMENT_BODY.to_string()),
        last_updated_by: "admin".to_string(),
        created_at: generated_at.to_string(),
        updated_at: generated_at.to_string(),
    });
    copy_container_acl(data, "environments", &environment_authz, admin_authz);
    grant_group_all(data, &environment_authz, admins);
    grant_group(
        data,
        &environment_authz,
        &[
            Permission::Create,
            Permission::Read,
            Permission::Update,
            Permission::Delete,
        ],
        users,
    );
    grant_group(data, &environment_authz, &[Permission::Read], clients);

    // Every imported object inherits its container ACL plus the creator grant.
    for (container, target) in object_authz {
        ensure_entity(data, target, "object", Some(&org.id), generated_at);
        copy_container_acl(data, container, target, admin_authz);
    }

    // A client is an object of the `clients` container and inherits its ACL.
    // Non-validator clients may also manage themselves; validators have no
    // self-service grants, matching `create_client_with_acl`.
    for (client, validator) in client_authz {
        ensure_entity(data, client, "actor", Some(&org.id), generated_at);
        copy_container_acl(data, "clients", client, admin_authz);
        if !*validator {
            grant_actor_all(data, client, client);
        }
    }
}

/// Copy every ACE of `container` onto `target` and add `creator` `ALL`.
fn copy_container_acl(data: &mut StoreSnapshot, container: &str, target: &str, creator: &str) {
    let source = data
        .containers
        .iter()
        .find(|row| row.name == container)
        .map(|row| row.authz_id.clone())
        .unwrap_or_else(|| panic!("default container {container}"));
    let actors: Vec<(String, String)> = data
        .authz_acl_actor
        .iter()
        .filter(|row| row.target_authz_id == source)
        .map(|row| (row.permission.clone(), row.actor_authz_id.clone()))
        .collect();
    for (permission, actor) in actors {
        push_acl_actor(data, target, &permission, &actor);
    }
    let groups: Vec<(String, String)> = data
        .authz_acl_group
        .iter()
        .filter(|row| row.target_authz_id == source)
        .map(|row| (row.permission.clone(), row.group_authz_id.clone()))
        .collect();
    for (permission, group) in groups {
        push_acl_group(data, target, &permission, &group);
    }
    grant_actor_all(data, target, creator);
}

fn ensure_entity(
    data: &mut StoreSnapshot,
    authz_id: &str,
    entity_type: &str,
    org_id: Option<&str>,
    generated_at: &str,
) {
    if data
        .authz_entities
        .iter()
        .any(|entity| entity.authz_id == authz_id)
    {
        return;
    }
    data.authz_entities.push(AuthzEntityRow {
        authz_id: authz_id.to_string(),
        entity_type: entity_type.to_string(),
        org_id: org_id.map(str::to_string),
        created_at: generated_at.to_string(),
    });
}

/// Create a global group and its upstream self-grants, as
/// `ensure_global_group` does.
fn push_global_group(data: &mut StoreSnapshot, name: &str, generated_at: &str) -> String {
    let authz_id = synthetic_id(&format!("authz:global_group:{name}"));
    ensure_entity(data, &authz_id, "group", None, generated_at);
    data.global_groups.push(GlobalGroupRow {
        id: synthetic_id(&format!("global_group:{name}")),
        authz_id: authz_id.clone(),
        name: name.to_string(),
        created_at: generated_at.to_string(),
        updated_at: generated_at.to_string(),
    });
    for permission in Permission::ALL {
        push_acl_group(data, &authz_id, permission.as_str(), &authz_id);
    }
    authz_id
}

fn add_group_member(data: &mut StoreSnapshot, group: &str, actor: &str) {
    if data
        .authz_group_actors
        .iter()
        .any(|row| row.group_authz_id == group && row.actor_authz_id == actor)
    {
        return;
    }
    data.authz_group_actors.push(AuthzGroupActorRow {
        group_authz_id: group.to_string(),
        actor_authz_id: actor.to_string(),
    });
}

fn add_group_child(data: &mut StoreSnapshot, parent: &str, child: &str) {
    if data
        .authz_group_groups
        .iter()
        .any(|row| row.parent_group_authz_id == parent && row.child_group_authz_id == child)
    {
        return;
    }
    data.authz_group_groups.push(AuthzGroupGroupRow {
        parent_group_authz_id: parent.to_string(),
        child_group_authz_id: child.to_string(),
    });
}

fn push_acl_actor(data: &mut StoreSnapshot, target: &str, permission: &str, actor: &str) {
    if data.authz_acl_actor.iter().any(|row| {
        row.target_authz_id == target && row.permission == permission && row.actor_authz_id == actor
    }) {
        return;
    }
    data.authz_acl_actor.push(AuthzAclActorRow {
        target_authz_id: target.to_string(),
        permission: permission.to_string(),
        actor_authz_id: actor.to_string(),
    });
}

fn push_acl_group(data: &mut StoreSnapshot, target: &str, permission: &str, group: &str) {
    if data.authz_acl_group.iter().any(|row| {
        row.target_authz_id == target && row.permission == permission && row.group_authz_id == group
    }) {
        return;
    }
    data.authz_acl_group.push(AuthzAclGroupRow {
        target_authz_id: target.to_string(),
        permission: permission.to_string(),
        group_authz_id: group.to_string(),
    });
}

fn grant_actor(data: &mut StoreSnapshot, target: &str, permissions: &[Permission], actor: &str) {
    for permission in permissions {
        push_acl_actor(data, target, permission.as_str(), actor);
    }
}

fn grant_group(data: &mut StoreSnapshot, target: &str, permissions: &[Permission], group: &str) {
    for permission in permissions {
        push_acl_group(data, target, permission.as_str(), group);
    }
}

fn grant_actor_all(data: &mut StoreSnapshot, target: &str, actor: &str) {
    grant_actor(data, target, &Permission::ALL, actor);
}

fn grant_group_all(data: &mut StoreSnapshot, target: &str, group: &str) {
    grant_group(data, target, &Permission::ALL, group);
}

fn container_authz<'a>(containers: &'a [(&'static str, String)], name: &str) -> &'a str {
    containers
        .iter()
        .find(|(container, _)| *container == name)
        .map(|(_, authz_id)| authz_id.as_str())
        .expect("default container")
}

fn group_authz<'a>(groups: &'a [(&'static str, String)], name: &str) -> &'a str {
    groups
        .iter()
        .find(|(group, _)| *group == name)
        .map(|(_, authz_id)| authz_id.as_str())
        .expect("default group")
}

/// Organizations are Chef names: lowercase letters, digits and dashes.
fn validate_org(org: &str) -> Result<(), MigrateError> {
    if org.is_empty()
        || !org
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(MigrateError::Config(format!(
            "invalid organization --org {org:?}; expected [a-z0-9-]+"
        )));
    }
    Ok(())
}

fn non_empty_string(value: &Value, key: &str) -> Option<String> {
    string_field(value, key).filter(|text| !text.is_empty())
}

/// Unwrap goiardi's cookbook-name envelope.
///
/// Both `/cookbooks` and `/cookbooks/<name>` wrap the resource under its name
/// (`{"app": {"url": ..., "versions": [...]}}`), so the body has to be
/// descended into before it looks like a cookbook. A body that already is the
/// resource passes through unchanged.
fn unwrap_cookbook(value: Value, name: &str) -> Value {
    if value.get("versions").is_some() || value.get("version").is_some() {
        return value;
    }
    if let Some(inner) = value.get(name).cloned() {
        return inner;
    }
    value
}

/// Read the version list of a cookbook resource.
///
/// goiardi's flat API returns `versions: [{version, url}]`, but the same
/// helper also accepts the object and single-version shapes so a variant
/// response does not drop versions.
fn cookbook_versions(value: &Value) -> Vec<(String, Option<String>)> {
    if let Value::Array(items) = value {
        return items
            .iter()
            .filter_map(|item| {
                let version = string_field(item, "version")?;
                Some((version, url_field(item)))
            })
            .collect();
    }
    let Some(versions) = value.get("versions") else {
        return string_field(value, "version")
            .map(|version| vec![(version, url_field(value))])
            .unwrap_or_default();
    };

    let mut output = Vec::new();
    match versions {
        Value::Object(map) => {
            for (version, entries) in map {
                match entries {
                    Value::Array(items) => {
                        for item in items {
                            let version =
                                string_field(item, "version").unwrap_or_else(|| version.clone());
                            output.push((version, url_field(item)));
                        }
                    }
                    Value::Object(_) => output.push((
                        string_field(entries, "version").unwrap_or_else(|| version.clone()),
                        url_field(entries),
                    )),
                    _ => output.push((version.clone(), None)),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                if let Some(version) = string_field(item, "version") {
                    output.push((version, url_field(item)));
                }
            }
        }
        _ => {}
    }
    output
}
