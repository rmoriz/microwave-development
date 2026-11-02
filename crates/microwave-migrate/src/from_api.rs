//! Live crawler for a legacy Chef Infra Server.
//!
//! Every request is signed with the configured client key using Chef's
//! `X-Ops-Sign: version=1.3` protocol. The crawler only reads; it never
//! creates, updates, or deletes objects on the source server.
//!
//! The legacy API does not expose the source database's internal ids or authz
//! ids. Those are derived deterministically from the organization and object
//! names so repeated crawls produce the same snapshot and importing the same
//! snapshot twice is idempotent.

use crate::crawl::{
    bool_field, cookbook_files, encode_segment, store_blob, string_field, synthetic_i64,
    synthetic_id, url_field, BlobSummary, CollectionEntry, Fetcher,
};
use crate::{
    write_snapshot, MigrateError, Snapshot, SnapshotSource, SnapshotSourceKind, SNAPSHOT_FORMAT,
    SNAPSHOT_VERSION,
};
use chrono::{SecondsFormat, Utc};
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{
    parse_version, ActorKeyRow, AuthzAclActorRow, AuthzAclGroupRow, AuthzEntityRow,
    AuthzGroupActorRow, AuthzGroupGroupRow, ChecksumRow, ClientRow, CookbookRow,
    CookbookVersionChecksumRow, CookbookVersionRow, DataBagItemRow, GroupRow, NamedObjectRow,
    NodeRow, OrgRow, OrgUserAssociationRow, PolicyGroupRevisionAssociationRow, PolicyGroupRow,
    PolicyRevisionRow, PolicyRow, StoreSnapshot, UserRow,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Options for [`from_api_with_options`].
#[derive(Debug, Clone)]
pub struct FromApiOptions {
    /// Base URL of the legacy Chef Infra Server, for example
    /// `https://chef.example.com`.
    pub url: String,
    /// User or client name used in `X-Ops-Userid`.
    pub client_name: String,
    /// PEM-encoded RSA private key used to sign requests.
    pub key_path: PathBuf,
    /// Snapshot file to write.
    pub output: PathBuf,
    /// Restrict the crawl to a single organization.
    pub org: Option<String>,
    /// Value sent in `X-Ops-Server-API-Version` (default `2`).
    pub api_version: u32,
    /// Local object-store root used to copy cookbook file blobs.
    ///
    /// When `None` the crawl only records metadata and checksums. When set,
    /// each unique cookbook file is written to
    /// `<bookshelf_dir>/organization-<org_id>/checksum-<md5>`.
    pub bookshelf_dir: Option<PathBuf>,
}

impl FromApiOptions {
    /// Build options with API version 2, no organization filter, and no
    /// bookshelf blob copy.
    pub fn new(
        url: impl Into<String>,
        client_name: impl Into<String>,
        key_path: impl Into<PathBuf>,
        output: impl Into<PathBuf>,
    ) -> Self {
        Self {
            url: url.into(),
            client_name: client_name.into(),
            key_path: key_path.into(),
            output: output.into(),
            org: None,
            api_version: 2,
            bookshelf_dir: None,
        }
    }
}

/// Counts from the optional cookbook blob copy during a live crawl.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlobDownloadSummary {
    /// Blobs written to the local object store.
    pub downloaded: usize,
    /// Blobs that already existed locally and were not requested again.
    pub skipped: usize,
}

/// Crawl a live legacy Chef Infra Server and write a snapshot.
pub fn from_api_with_options(options: &FromApiOptions) -> Result<(), MigrateError> {
    from_api_with_options_with_summary(options).map(|_| ())
}

/// Crawl a live legacy Chef Infra Server, write a snapshot, and return the
/// counts from the optional cookbook blob copy.
pub fn from_api_with_options_with_summary(
    options: &FromApiOptions,
) -> Result<BlobDownloadSummary, MigrateError> {
    let (snapshot, summary) = crawl_from_api_with_summary(options)?;
    write_snapshot(&snapshot, &options.output)?;
    Ok(summary)
}

/// Crawl a live legacy Chef Infra Server and return the snapshot without
/// writing it.
pub fn crawl_from_api(options: &FromApiOptions) -> Result<Snapshot, MigrateError> {
    crawl_from_api_with_summary(options).map(|(snapshot, _)| snapshot)
}

/// Crawl a live legacy Chef Infra Server and return the snapshot plus the
/// optional cookbook blob counts without writing the snapshot.
pub fn crawl_from_api_with_summary(
    options: &FromApiOptions,
) -> Result<(Snapshot, BlobDownloadSummary), MigrateError> {
    let mut crawler = Crawler::new(options)?;
    crawler.crawl(options.org.as_deref())?;
    let data_dir = crawler
        .bookshelf_dir
        .as_ref()
        .map(|path| path.display().to_string());
    let summary = BlobDownloadSummary {
        downloaded: crawler.blob_summary.downloaded,
        skipped: crawler.blob_summary.skipped,
    };
    Ok((
        Snapshot {
            format: SNAPSHOT_FORMAT.to_string(),
            version: SNAPSHOT_VERSION,
            generated_at: crawler.generated_at.clone(),
            source: SnapshotSource {
                kind: SnapshotSourceKind::LegacyApi,
                url: Some(crawler.base_url.clone()),
                data_dir,
                layout: None,
            },
            secrets_included: false,
            data: crawler.data,
        },
        summary,
    ))
}

/// One key returned by an actor's keys endpoint. Older API versions embed the
/// public key in the list response instead of linking to a separate key object.
#[derive(Debug, Clone)]
struct KeyDescriptor {
    name: String,
    url: Option<String>,
    value: Option<Value>,
}

struct Crawler {
    fetcher: Fetcher,
    base_url: String,
    generated_at: String,
    data: StoreSnapshot,
    user_authz: HashMap<String, String>,
    client_authz: HashMap<(String, String), String>,
    group_authz: HashMap<(String, String), String>,
    seen_authz: HashSet<String>,
    seen_acl_actor: HashSet<(String, String, String)>,
    seen_acl_group: HashSet<(String, String, String)>,
    seen_checksums: HashSet<(String, String)>,
    seen_cookbook_versions: HashSet<(String, String)>,
    pending_acls: Vec<AclRequest>,
    bookshelf_dir: Option<PathBuf>,
    bookshelf: Option<LocalObjectStore>,
    pending_blobs: HashMap<(String, String), String>,
    blob_summary: BlobSummary,
    downloaded_blob_bytes: u64,
}

struct AclRequest {
    org_name: Option<String>,
    primary: Option<String>,
    fallback: String,
    target_authz: String,
}

impl Crawler {
    fn new(options: &FromApiOptions) -> Result<Self, MigrateError> {
        let fetcher = Fetcher::new(
            &options.url,
            &options.client_name,
            &options.key_path,
            options.api_version,
            None,
        )?;
        let base_url = fetcher.resolve("/").trim_end_matches('/').to_string();

        let bookshelf_dir = options.bookshelf_dir.clone();
        let bookshelf = bookshelf_dir
            .as_ref()
            .map(|dir| LocalObjectStore::new(dir.clone(), "", "http://localhost"));

        Ok(Self {
            fetcher,
            base_url,
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            data: StoreSnapshot::default(),
            user_authz: HashMap::new(),
            client_authz: HashMap::new(),
            group_authz: HashMap::new(),
            seen_authz: HashSet::new(),
            seen_acl_actor: HashSet::new(),
            seen_acl_group: HashSet::new(),
            seen_checksums: HashSet::new(),
            seen_cookbook_versions: HashSet::new(),
            pending_acls: Vec::new(),
            bookshelf_dir,
            bookshelf,
            pending_blobs: HashMap::new(),
            blob_summary: BlobSummary::default(),
            downloaded_blob_bytes: 0,
        })
    }

    fn crawl(&mut self, only_org: Option<&str>) -> Result<(), MigrateError> {
        self.crawl_users()?;

        match only_org {
            Some(name) => {
                let org = self.fetch_org(name)?;
                self.crawl_org(&org)?;
            }
            None => {
                let orgs = self.list_orgs()?;
                for org in orgs {
                    self.crawl_org(&org)?;
                }
            }
        }
        self.apply_pending_acls()?;
        self.download_blobs()
    }

    // ------------------------------------------------------------------
    // Organizations and users
    // ------------------------------------------------------------------

    fn list_orgs(&mut self) -> Result<Vec<OrgRow>, MigrateError> {
        let entries = self.fetcher.get_collection("/organizations")?;
        let mut orgs = Vec::new();
        for entry in entries {
            let name = entry.name.clone();
            let detail = match &entry.url {
                Some(url) => self.fetcher.get_optional_json(&self.fetcher.resolve(url))?,
                None => Some(entry.value.clone()),
            };
            let value = detail.unwrap_or(Value::Null);
            orgs.push(self.org_row(&name, &value));
        }
        Ok(orgs)
    }

    fn fetch_org(&mut self, name: &str) -> Result<OrgRow, MigrateError> {
        let url = self
            .fetcher
            .resolve(&format!("/organizations/{}", encode_segment(name)));
        match self.fetcher.get_optional_raw(&url)? {
            Some((value, _)) => Ok(self.org_row(name, &value)),
            None => Err(MigrateError::Api {
                url,
                status: 404,
                message: format!("organization {name:?} was not found"),
            }),
        }
    }

    fn org_row(&mut self, name: &str, value: &Value) -> OrgRow {
        let guid = string_field(value, "guid").filter(|guid| !guid.is_empty());
        let id = guid.unwrap_or_else(|| synthetic_id(&format!("org:{name}")));
        let authz_id = synthetic_id(&format!("authz:org:{name}"));
        self.ensure_entity(&authz_id, "actor", Some(&id));

        OrgRow {
            id,
            authz_id,
            name: name.to_string(),
            full_name: string_field(value, "full_name").unwrap_or_else(|| name.to_string()),
            assigned_at: self.generated_at.clone(),
            last_updated_by: String::new(),
            created_at: self.generated_at.clone(),
            updated_at: self.generated_at.clone(),
        }
    }

    fn crawl_users(&mut self) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection("/users")?;
        for entry in entries {
            let name = entry.name.clone();
            let value = match &entry.url {
                Some(url) => self.fetcher.get_optional_json(&self.fetcher.resolve(url))?,
                None => Some(entry.value.clone()),
            };
            let Some(value) = value else { continue };

            let username = string_field(&value, "username").unwrap_or(name);
            let id = synthetic_id(&format!("user:{username}"));
            let authz_id = synthetic_id(&format!("authz:user:{username}"));
            self.ensure_entity(&authz_id, "actor", None);
            self.user_authz.insert(username.clone(), authz_id.clone());

            self.data.users.push(UserRow {
                id: id.clone(),
                authz_id: authz_id.clone(),
                username: username.clone(),
                email: non_empty_string(&value, "email"),
                password_hash: String::new(),
                password_salt: String::new(),
                hash_type: "bcrypt".to_string(),
                admin: bool_field(&value, "admin").unwrap_or(false),
                recovery_authentication_enabled: bool_field(
                    &value,
                    "recovery_authentication_enabled",
                )
                .unwrap_or(false),
                external_authentication_uid: non_empty_string(
                    &value,
                    "external_authentication_uid",
                ),
                display_name: non_empty_string(&value, "display_name"),
                first_name: non_empty_string(&value, "first_name"),
                middle_name: non_empty_string(&value, "middle_name"),
                last_name: non_empty_string(&value, "last_name"),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });

            let added = self.crawl_actor_keys(
                &format!("/users/{}/keys", encode_segment(&username)),
                &id,
                "user",
            )?;
            if !added {
                self.add_inline_key(&value, &id, "user");
            }
            self.queue_acl(
                None,
                None,
                &format!("/users/{}/_acl", encode_segment(&username)),
                &authz_id,
            );
        }
        Ok(())
    }

    fn crawl_org(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        self.data.orgs.push(org.clone());
        let org_name = org.name.clone();
        let org_authz = org.authz_id.clone();

        self.crawl_org_users(org)?;
        self.crawl_clients(org)?;
        self.crawl_groups(org)?;
        self.crawl_nodes(org)?;
        self.crawl_named_objects(org)?;
        self.crawl_data_bags(org)?;
        self.crawl_cookbooks(org)?;
        self.crawl_policies(org)?;

        // The org ACL endpoint moved between server releases. Try the modern
        // route first, then the older shorthand.
        self.queue_acl(
            Some(&org_name),
            Some(&format!(
                "/organizations/{}/organizations/_acl",
                encode_segment(&org_name)
            )),
            &format!("/organizations/{}/_acl", encode_segment(&org_name)),
            &org_authz,
        );
        Ok(())
    }

    fn crawl_org_users(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection(&format!(
            "/organizations/{}/users",
            encode_segment(&org.name)
        ))?;
        for entry in entries {
            let username = entry.name;
            if !self.user_authz.contains_key(&username) {
                continue;
            }
            self.data.org_user_associations.push(OrgUserAssociationRow {
                org_id: org.id.clone(),
                user_id: synthetic_id(&format!("user:{username}")),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Clients and keys
    // ------------------------------------------------------------------

    fn crawl_clients(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection(&format!(
            "/organizations/{}/clients",
            encode_segment(&org.name)
        ))?;
        for entry in entries {
            let list_name = entry.name.clone();
            let value = match &entry.url {
                Some(url) => self.fetcher.get_optional_json(&self.fetcher.resolve(url))?,
                None => Some(entry.value.clone()),
            };
            let Some(value) = value else { continue };

            let name = string_field(&value, "name")
                .or_else(|| string_field(&value, "clientname"))
                .unwrap_or(list_name);
            let id = synthetic_id(&format!("client:{}:{name}", org.name));
            let authz_id = synthetic_id(&format!("authz:client:{}:{name}", org.name));
            self.ensure_entity(&authz_id, "actor", Some(&org.id));
            self.client_authz
                .insert((org.name.clone(), name.clone()), authz_id.clone());

            self.data.clients.push(ClientRow {
                id: id.clone(),
                org_id: org.id.clone(),
                authz_id: authz_id.clone(),
                name: name.clone(),
                validator: bool_field(&value, "validator").unwrap_or(false),
                admin: bool_field(&value, "admin").unwrap_or(false),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });

            let added = self.crawl_actor_keys(
                &format!(
                    "/organizations/{}/clients/{}/keys",
                    encode_segment(&org.name),
                    encode_segment(&name)
                ),
                &id,
                "client",
            )?;
            if !added {
                self.add_inline_key(&value, &id, "client");
            }
            self.queue_acl(
                Some(&org.name),
                None,
                &format!(
                    "/organizations/{}/clients/{}/_acl",
                    encode_segment(&org.name),
                    encode_segment(&name)
                ),
                &authz_id,
            );
        }
        Ok(())
    }

    /// Crawl the keys API for one actor. Returns true when at least one key was
    /// recorded.
    fn crawl_actor_keys(
        &mut self,
        path: &str,
        actor_id: &str,
        actor_type: &str,
    ) -> Result<bool, MigrateError> {
        let url = self.fetcher.resolve(path);
        let Some((value, _)) = self.fetcher.get_optional_raw(&url)? else {
            return Ok(false);
        };

        let descriptors = key_descriptors(&value);
        let mut added = false;
        for descriptor in descriptors {
            let KeyDescriptor {
                name: key_name,
                url: key_url,
                value: inline,
            } = descriptor;
            let key_value = match (key_url, inline) {
                (Some(key_url), inline) => {
                    match self
                        .fetcher
                        .get_optional_json(&self.fetcher.resolve(&key_url))?
                    {
                        Some(value) => value,
                        None => match inline {
                            Some(value) => value,
                            None => continue,
                        },
                    }
                }
                (None, Some(value)) => value,
                (None, None) => continue,
            };
            let Some(public_key) = string_field(&key_value, "public_key") else {
                continue;
            };
            let expires_at = string_field(&key_value, "expiration_date")
                .unwrap_or_else(|| "infinity".to_string());
            self.push_actor_key(actor_id, actor_type, &key_name, &public_key, &expires_at);
            added = true;
        }
        Ok(added)
    }

    fn add_inline_key(&mut self, actor: &Value, actor_id: &str, actor_type: &str) {
        if let Some(public_key) = string_field(actor, "public_key") {
            self.push_actor_key(actor_id, actor_type, "default", &public_key, "infinity");
        }
    }

    fn push_actor_key(
        &mut self,
        actor_id: &str,
        actor_type: &str,
        key_name: &str,
        public_key: &str,
        expires_at: &str,
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
            expires_at: expires_at.to_string(),
            last_updated_by: String::new(),
            created_at: self.generated_at.clone(),
            updated_at: self.generated_at.clone(),
        });
    }

    // ------------------------------------------------------------------
    // Groups
    // ------------------------------------------------------------------

    fn crawl_groups(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection(&format!(
            "/organizations/{}/groups",
            encode_segment(&org.name)
        ))?;
        let mut memberships: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
        let mut acl_targets: Vec<(String, String)> = Vec::new();

        for entry in entries {
            let list_name = entry.name.clone();
            let value = match &entry.url {
                Some(url) => self.fetcher.get_optional_json(&self.fetcher.resolve(url))?,
                None => Some(entry.value.clone()),
            };
            let Some(value) = value else { continue };

            let name = string_field(&value, "name")
                .or_else(|| string_field(&value, "groupname"))
                .unwrap_or(list_name);
            let id = synthetic_id(&format!("group:{}:{name}", org.name));
            let authz_id = synthetic_id(&format!("authz:group:{}:{name}", org.name));
            self.ensure_entity(&authz_id, "group", Some(&org.id));
            self.group_authz
                .insert((org.name.clone(), name.clone()), authz_id.clone());

            self.data.groups.push(GroupRow {
                id,
                org_id: org.id.clone(),
                authz_id: authz_id.clone(),
                name: name.clone(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });

            let actors = string_array(&value, "actors");
            let groups = string_array(&value, "groups");
            memberships.push((authz_id.clone(), actors, groups));
            acl_targets.push((
                format!(
                    "/organizations/{}/groups/{}/_acl",
                    encode_segment(&org.name),
                    encode_segment(&name)
                ),
                authz_id,
            ));
        }

        // Resolve memberships after every group in the organization has a
        // synthetic authz id.
        for (group_authz, actors, groups) in memberships {
            for actor in actors {
                if let Some(actor_authz) = self.resolve_actor(&org.name, &actor) {
                    self.data.authz_group_actors.push(AuthzGroupActorRow {
                        group_authz_id: group_authz.clone(),
                        actor_authz_id: actor_authz,
                    });
                }
            }
            for child in groups {
                if let Some(child_authz) = self
                    .group_authz
                    .get(&(org.name.clone(), child.clone()))
                    .cloned()
                {
                    self.data.authz_group_groups.push(AuthzGroupGroupRow {
                        parent_group_authz_id: group_authz.clone(),
                        child_group_authz_id: child_authz,
                    });
                }
            }
        }

        for (path, target) in acl_targets {
            self.queue_acl(Some(&org.name), None, &path, &target);
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Nodes, roles, environments, data bags
    // ------------------------------------------------------------------

    fn crawl_nodes(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection(&format!(
            "/organizations/{}/nodes",
            encode_segment(&org.name)
        ))?;
        for entry in entries {
            let list_name = entry.name.clone();
            let Some((value, raw)) = self.entry_raw(
                &entry,
                &format!(
                    "/organizations/{}/nodes/{}",
                    encode_segment(&org.name),
                    encode_segment(&list_name)
                ),
            )?
            else {
                continue;
            };
            let name = string_field(&value, "name").unwrap_or(list_name);
            let id = synthetic_id(&format!("node:{}:{name}", org.name));
            let authz_id = synthetic_id(&format!("authz:node:{}:{name}", org.name));
            self.ensure_entity(&authz_id, "object", Some(&org.id));

            self.data.nodes.push(NodeRow {
                id,
                authz_id: authz_id.clone(),
                org_id: org.id.clone(),
                name: name.clone(),
                environment: string_field(&value, "chef_environment")
                    .unwrap_or_else(|| "_default".to_string()),
                policy_name: non_empty_string(&value, "policy_name"),
                policy_group: non_empty_string(&value, "policy_group"),
                serialized_object: Some(raw),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });

            self.queue_acl(
                Some(&org.name),
                None,
                &format!(
                    "/organizations/{}/nodes/{}/_acl",
                    encode_segment(&org.name),
                    encode_segment(&name)
                ),
                &authz_id,
            );
        }
        Ok(())
    }

    fn crawl_named_objects(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        for (kind, collection, acl_kind) in [
            ("role", "roles", "roles"),
            ("environment", "environments", "environments"),
        ] {
            let entries = self.fetcher.get_collection(&format!(
                "/organizations/{}/{}",
                encode_segment(&org.name),
                collection
            ))?;
            for entry in entries {
                let list_name = entry.name.clone();
                let Some((value, raw)) = self.entry_raw(
                    &entry,
                    &format!(
                        "/organizations/{}/{}/{}",
                        encode_segment(&org.name),
                        collection,
                        encode_segment(&list_name)
                    ),
                )?
                else {
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
                    name: name.clone(),
                    serialized_object: Some(raw),
                    last_updated_by: String::new(),
                    created_at: self.generated_at.clone(),
                    updated_at: self.generated_at.clone(),
                };
                match kind {
                    "role" => self.data.roles.push(row),
                    "environment" => self.data.environments.push(row),
                    _ => unreachable!(),
                }

                self.queue_acl(
                    Some(&org.name),
                    None,
                    &format!(
                        "/organizations/{}/{}/{}/_acl",
                        encode_segment(&org.name),
                        acl_kind,
                        encode_segment(&name)
                    ),
                    &authz_id,
                );
            }
        }
        Ok(())
    }

    fn crawl_data_bags(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection(&format!(
            "/organizations/{}/data",
            encode_segment(&org.name)
        ))?;
        for entry in entries {
            let name = entry.name.clone();
            // The legacy API has no GET for data bag metadata; `/data/{bag}`
            // is the item collection. Reconstruct the canonical bag body.
            let bag_value = if entry.value.is_object() && entry.value.get("name").is_some() {
                entry.value.clone()
            } else {
                serde_json::json!({
                    "name": name,
                    "json_class": "Chef::DataBag",
                    "chef_type": "data_bag"
                })
            };
            let raw = serde_json::to_string(&bag_value)?;
            let id = synthetic_id(&format!("data_bag:{}:{name}", org.name));
            let authz_id = synthetic_id(&format!("authz:data_bag:{}:{name}", org.name));
            self.ensure_entity(&authz_id, "object", Some(&org.id));

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

            self.queue_acl(
                Some(&org.name),
                None,
                &format!(
                    "/organizations/{}/data/{}/_acl",
                    encode_segment(&org.name),
                    encode_segment(&name)
                ),
                &authz_id,
            );

            let items_path = entry.url.clone().unwrap_or_else(|| {
                format!(
                    "/organizations/{}/data/{}",
                    encode_segment(&org.name),
                    encode_segment(&name)
                )
            });
            let items = self.fetcher.get_collection(&items_path)?;
            for item in items {
                let item_list_name = item.name.clone();
                let Some((item_value, item_raw)) = self.entry_raw(
                    &item,
                    &format!(
                        "/organizations/{}/data/{}/{}",
                        encode_segment(&org.name),
                        encode_segment(&name),
                        encode_segment(&item_list_name)
                    ),
                )?
                else {
                    continue;
                };
                let item_name = string_field(&item_value, "id").unwrap_or(item_list_name);
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
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Cookbooks
    // ------------------------------------------------------------------

    fn crawl_cookbooks(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection(&format!(
            "/organizations/{}/cookbooks",
            encode_segment(&org.name)
        ))?;
        for entry in entries {
            let list_name = entry.name.clone();
            let value = match &entry.url {
                Some(url) => self.fetcher.get_optional_json(&self.fetcher.resolve(url))?,
                None => Some(entry.value.clone()),
            };
            let Some(value) = value else { continue };
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
            }

            for (version, version_url) in cookbook_versions(&value) {
                if !self
                    .seen_cookbook_versions
                    .insert((name.clone(), version.clone()))
                {
                    continue;
                }
                let version_path = version_url.unwrap_or_else(|| {
                    format!(
                        "/organizations/{}/cookbooks/{}/{}",
                        encode_segment(&org.name),
                        encode_segment(&name),
                        encode_segment(&version)
                    )
                });
                let version_url = self.fetcher.resolve(&version_path);
                let Some((version_value, raw)) = self.fetcher.get_optional_raw(&version_url)?
                else {
                    continue;
                };
                self.push_cookbook_version(org, &name, cookbook_id, &version, &version_value, raw);
            }

            self.queue_acl(
                Some(&org.name),
                None,
                &format!(
                    "/organizations/{}/cookbooks/{}/_acl",
                    encode_segment(&org.name),
                    encode_segment(&name)
                ),
                &authz_id,
            );
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
    ) {
        let version =
            string_field(value, "version").unwrap_or_else(|| fallback_version.to_string());
        let Ok((major, minor, patch)) = parse_version(&version) else {
            return;
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
            if self
                .seen_checksums
                .insert((org.id.clone(), checksum.clone()))
            {
                self.data.checksums.push(ChecksumRow {
                    org_id: org.id.clone(),
                    checksum: checksum.clone(),
                    created_at: self.generated_at.clone(),
                });
            }
            self.data
                .cookbook_version_checksums
                .push(CookbookVersionChecksumRow {
                    cookbook_version_id: version_id.clone(),
                    org_id: org.id.clone(),
                    checksum: checksum.clone(),
                });
            if self.bookshelf.is_some() {
                if let Some(file_url) = file_url {
                    self.pending_blobs
                        .entry((org.id.clone(), checksum))
                        .or_insert(file_url);
                }
            }
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

    // ------------------------------------------------------------------
    // Policies
    // ------------------------------------------------------------------

    fn crawl_policies(&mut self, org: &OrgRow) -> Result<(), MigrateError> {
        let mut revision_ids: HashMap<(String, String), String> = HashMap::new();
        let entries = self.fetcher.get_collection(&format!(
            "/organizations/{}/policies",
            encode_segment(&org.name)
        ))?;
        for entry in entries {
            let list_name = entry.name.clone();
            let value = match &entry.url {
                Some(url) => self.fetcher.get_optional_json(&self.fetcher.resolve(url))?,
                None => Some(entry.value.clone()),
            };
            let Some(value) = value else { continue };
            let name = string_field(&value, "name").unwrap_or(list_name);
            let id = synthetic_id(&format!("policy:{}:{name}", org.name));
            let authz_id = synthetic_id(&format!("authz:policy:{}:{name}", org.name));
            self.ensure_entity(&authz_id, "object", Some(&org.id));

            self.data.policies.push(PolicyRow {
                id: id.clone(),
                org_id: org.id.clone(),
                name: name.clone(),
                authz_id: authz_id.clone(),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });

            for revision_id in policy_revisions(&value) {
                let path = format!(
                    "/organizations/{}/policies/{}/revisions/{}",
                    encode_segment(&org.name),
                    encode_segment(&name),
                    encode_segment(&revision_id)
                );
                let Some((_revision_value, raw)) = self
                    .fetcher
                    .get_optional_raw(&self.fetcher.resolve(&path))?
                else {
                    continue;
                };
                let row_id = synthetic_id(&format!(
                    "policy_revision:{}:{name}:{revision_id}",
                    org.name
                ));
                revision_ids.insert((name.clone(), revision_id.clone()), row_id.clone());
                self.data.policy_revisions.push(PolicyRevisionRow {
                    id: row_id,
                    org_id: org.id.clone(),
                    policy_id: id.clone(),
                    revision_id: revision_id.clone(),
                    name: name.clone(),
                    serialized_object: Some(raw),
                    last_updated_by: String::new(),
                    created_at: self.generated_at.clone(),
                    updated_at: self.generated_at.clone(),
                });
            }

            self.queue_acl(
                Some(&org.name),
                None,
                &format!(
                    "/organizations/{}/policies/{}/_acl",
                    encode_segment(&org.name),
                    encode_segment(&name)
                ),
                &authz_id,
            );
        }

        self.crawl_policy_groups(org, &revision_ids)
    }

    fn crawl_policy_groups(
        &mut self,
        org: &OrgRow,
        revision_ids: &HashMap<(String, String), String>,
    ) -> Result<(), MigrateError> {
        let entries = self.fetcher.get_collection(&format!(
            "/organizations/{}/policy_groups",
            encode_segment(&org.name)
        ))?;
        for entry in entries {
            let list_name = entry.name.clone();
            let value = match &entry.url {
                Some(url) => self.fetcher.get_optional_json(&self.fetcher.resolve(url))?,
                None => Some(entry.value.clone()),
            };
            let Some(value) = value else { continue };
            let name = string_field(&value, "name").unwrap_or(list_name);
            let id = synthetic_id(&format!("policy_group:{}:{name}", org.name));
            let authz_id = synthetic_id(&format!("authz:policy_group:{}:{name}", org.name));
            self.ensure_entity(&authz_id, "object", Some(&org.id));

            self.data.policy_groups.push(PolicyGroupRow {
                id,
                org_id: org.id.clone(),
                name: name.clone(),
                authz_id: authz_id.clone(),
                serialized_object: serde_json::to_string(&value).ok(),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });

            for (policy_name, revision_id) in policy_group_policies(&value) {
                let Some(policy_revision_id) = revision_ids
                    .get(&(policy_name.clone(), revision_id.clone()))
                    .cloned()
                else {
                    continue;
                };
                self.data.policy_group_revision_associations.push(
                    PolicyGroupRevisionAssociationRow {
                        id: synthetic_id(&format!(
                            "policy_group_association:{}:{name}:{policy_name}",
                            org.name
                        )),
                        org_id: org.id.clone(),
                        policy_revision_id,
                        policy_group_authz_id: authz_id.clone(),
                        policy_revision_name: policy_name.clone(),
                        policy_revision_revision_id: revision_id,
                        policy_group_name: name.clone(),
                        last_updated_by: String::new(),
                        created_at: self.generated_at.clone(),
                        updated_at: self.generated_at.clone(),
                    },
                );
            }

            self.queue_acl(
                Some(&org.name),
                None,
                &format!(
                    "/organizations/{}/policy_groups/{}/_acl",
                    encode_segment(&org.name),
                    encode_segment(&name)
                ),
                &authz_id,
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // ACLs
    // ------------------------------------------------------------------

    fn queue_acl(
        &mut self,
        org_name: Option<&str>,
        primary: Option<&str>,
        fallback: &str,
        target_authz: &str,
    ) {
        self.pending_acls.push(AclRequest {
            org_name: org_name.map(str::to_string),
            primary: primary.map(str::to_string),
            fallback: fallback.to_string(),
            target_authz: target_authz.to_string(),
        });
    }

    fn apply_pending_acls(&mut self) -> Result<(), MigrateError> {
        let pending = std::mem::take(&mut self.pending_acls);
        for request in pending {
            if let Some(primary) = &request.primary {
                let url = self.fetcher.resolve(primary);
                if let Some(value) = self.fetcher.get_optional_json(&url)? {
                    self.apply_acl(&value, &request.target_authz, request.org_name.as_deref())?;
                    continue;
                }
            }
            let url = self.fetcher.resolve(&request.fallback);
            if let Some(value) = self.fetcher.get_optional_json(&url)? {
                self.apply_acl(&value, &request.target_authz, request.org_name.as_deref())?;
            }
        }
        Ok(())
    }

    fn apply_acl(
        &mut self,
        value: &Value,
        target_authz: &str,
        org_name: Option<&str>,
    ) -> Result<(), MigrateError> {
        let Some(permissions) = value.as_object() else {
            return Ok(());
        };
        let org_name = org_name.unwrap_or_default();
        for (permission, entries) in permissions {
            if !matches!(
                permission.as_str(),
                "create" | "read" | "update" | "delete" | "grant"
            ) {
                continue;
            }
            let Some(entries) = entries.as_object() else {
                continue;
            };
            if let Some(actors) = entries.get("actors").and_then(Value::as_array) {
                for actor in actors.iter().filter_map(Value::as_str) {
                    let Some(actor_authz) = self.resolve_actor(org_name, actor) else {
                        continue;
                    };
                    if !self.seen_acl_actor.insert((
                        target_authz.to_string(),
                        permission.clone(),
                        actor_authz.clone(),
                    )) {
                        continue;
                    }
                    self.data.authz_acl_actor.push(AuthzAclActorRow {
                        target_authz_id: target_authz.to_string(),
                        permission: permission.clone(),
                        actor_authz_id: actor_authz,
                    });
                }
            }
            if let Some(groups) = entries.get("groups").and_then(Value::as_array) {
                for group in groups.iter().filter_map(Value::as_str) {
                    let Some(group_authz) = self
                        .group_authz
                        .get(&(org_name.to_string(), group.to_string()))
                        .cloned()
                    else {
                        continue;
                    };
                    if !self.seen_acl_group.insert((
                        target_authz.to_string(),
                        permission.clone(),
                        group_authz.clone(),
                    )) {
                        continue;
                    }
                    self.data.authz_acl_group.push(AuthzAclGroupRow {
                        target_authz_id: target_authz.to_string(),
                        permission: permission.clone(),
                        group_authz_id: group_authz,
                    });
                }
            }
        }
        Ok(())
    }

    fn resolve_actor(&self, org_name: &str, name: &str) -> Option<String> {
        if !org_name.is_empty() {
            if let Some(authz) = self
                .client_authz
                .get(&(org_name.to_string(), name.to_string()))
            {
                return Some(authz.clone());
            }
        }
        self.user_authz.get(name).cloned()
    }

    // ------------------------------------------------------------------
    // HTTP and JSON helpers
    // ------------------------------------------------------------------

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
}

fn key_descriptors(value: &Value) -> Vec<KeyDescriptor> {
    fn from_key_object(item: &Value) -> Option<KeyDescriptor> {
        let name = string_field(item, "name")?;
        Some(KeyDescriptor {
            name,
            url: string_field(item, "uri").or_else(|| string_field(item, "url")),
            value: inline_key_value(item),
        })
    }

    match value {
        Value::Array(items) => items.iter().filter_map(from_key_object).collect(),
        Value::Object(map) => {
            if map.contains_key("public_key") {
                return vec![KeyDescriptor {
                    name: string_field(value, "name").unwrap_or_else(|| "default".to_string()),
                    url: None,
                    value: Some(value.clone()),
                }];
            }
            if let Some(keys) = map.get("keys").and_then(Value::as_array) {
                return keys.iter().filter_map(from_key_object).collect();
            }
            map.iter()
                .filter_map(|(name, value)| match value {
                    Value::String(url) => Some(KeyDescriptor {
                        name: name.clone(),
                        url: Some(url.clone()),
                        value: None,
                    }),
                    Value::Object(_) => Some(KeyDescriptor {
                        name: name.clone(),
                        url: url_field(value),
                        value: inline_key_value(value),
                    }),
                    _ => None,
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Keep a key body that already carries its public key, so an embedded key is
/// not dropped when the response has no separate key URL.
fn inline_key_value(value: &Value) -> Option<Value> {
    value
        .get("public_key")
        .filter(|key| key.as_str().is_some_and(|key| !key.is_empty()))
        .map(|_| value.clone())
}

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
                            output.push((
                                version,
                                string_field(item, "url").or_else(|| string_field(item, "uri")),
                            ));
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

fn policy_revisions(value: &Value) -> Vec<String> {
    let Some(revisions) = value.get("revisions") else {
        return Vec::new();
    };
    match revisions {
        Value::Object(map) => map.keys().cloned().collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                string_field(item, "revision_id").or_else(|| string_field(item, "id"))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn policy_group_policies(value: &Value) -> Vec<(String, String)> {
    let Some(policies) = value.get("policies").and_then(Value::as_object) else {
        return Vec::new();
    };
    policies
        .iter()
        .filter_map(|(name, policy)| {
            let revision =
                string_field(policy, "revision_id").or_else(|| string_field(policy, "revision"))?;
            Some((name.clone(), revision))
        })
        .collect()
}

fn non_empty_string(value: &Value, key: &str) -> Option<String> {
    string_field(value, key).filter(|text| !text.is_empty())
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Read a private key from a path and return it as a string.
///
/// Exposed for tests and for callers that want to validate a key before
/// starting a crawl. The key is never written into a snapshot.
pub fn read_private_key(path: &Path) -> Result<String, MigrateError> {
    std::fs::read_to_string(path).map_err(|source| MigrateError::Read {
        path: path.to_path_buf(),
        source,
    })
}
