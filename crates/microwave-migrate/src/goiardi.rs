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
    bool_field, encode_segment, string_field, synthetic_id, BlobSummary, CollectionEntry, Fetcher,
};
use crate::{
    write_snapshot, MigrateError, Snapshot, SnapshotSource, SnapshotSourceKind, SNAPSHOT_FORMAT,
    SNAPSHOT_VERSION,
};
use chrono::{SecondsFormat, Utc};
use microwave_store::{
    ActorKeyRow, AuthzEntityRow, ClientRow, DataBagItemRow, NamedObjectRow, NodeRow, OrgRow,
    OrgUserAssociationRow, StoreSnapshot, UserRow,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

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
/// The snapshot is returned either way, but a dry run leaves the output path
/// untouched.
pub fn crawl_from_goiardi(
    options: &GoiardiOptions,
) -> Result<(Snapshot, GoiardiSummary), MigrateError> {
    validate_org(&options.org)?;
    let mut crawler = Crawler::new(options)?;
    crawler.crawl_identity()?;
    crawler.crawl_objects()?;
    crawler.summary.blobs_downloaded = crawler.blob_summary.downloaded;
    crawler.summary.blobs_skipped = crawler.blob_summary.skipped;

    let snapshot = Snapshot {
        format: SNAPSHOT_FORMAT.to_string(),
        version: SNAPSHOT_VERSION,
        generated_at: crawler.generated_at.clone(),
        source: SnapshotSource {
            kind: SnapshotSourceKind::LegacyApi,
            url: Some(options.url.trim_end_matches('/').to_string()),
            data_dir: None,
            layout: Some("goiardi".to_string()),
        },
        secrets_included: false,
        data: crawler.data,
    };

    if !options.dry_run {
        write_snapshot(&snapshot, &options.output)?;
    }
    Ok((snapshot, crawler.summary))
}

/// Crawl a goiardi server, write the snapshot, and return the summary.
pub fn from_goiardi_with_options(options: &GoiardiOptions) -> Result<GoiardiSummary, MigrateError> {
    crawl_from_goiardi(options).map(|(_, summary)| summary)
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
    /// `(org id, checksum)` to the first URL that advertised the blob.
    pub(crate) pending_blobs: HashMap<(String, String), String>,
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
            pending_blobs: HashMap::new(),
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

/// Organizations are Chef names: lowercase letters, digits and dashes.
fn validate_org(org: &str) -> Result<(), MigrateError> {
    if org.is_empty()
        || !org
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(MigrateError::Config(format!(
            "invalid --org {org:?}; expected [a-z0-9-]+"
        )));
    }
    Ok(())
}

fn non_empty_string(value: &Value, key: &str) -> Option<String> {
    string_field(value, key).filter(|text| !text.is_empty())
}
