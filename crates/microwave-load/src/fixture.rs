//! Deterministic fixture for the converge load test.
//!
//! The fixture is a [`microwave_store::StoreSnapshot`] for one organization,
//! applied through `import_snapshot`. Nothing here randomizes or calls the
//! clock: every id, name, body and blob comes from a counter or a fixed
//! string, so two builds of the same [`Sizes`] serialize to the same bytes.
//!
//! Row invariants mirror what `import_all` expects from a real export: every
//! authz id appears exactly once in `authz_entities` with the type its table
//! demands, ACL rows point at existing entities, and the cookbook checksums
//! exist as `checksums` rows before the version checksum rows reference them.

use microwave_objectstore::md5_hex;
use microwave_store::{
    AuthzAclActorRow, AuthzAclGroupRow, AuthzEntityRow, AuthzGroupActorRow, ChecksumRow, ClientRow,
    ContainerRow, CookbookRow, CookbookVersionChecksumRow, CookbookVersionRow, GroupRow,
    NamedObjectRow, NodeRow, OrgRow, OrgUserAssociationRow, StoreSnapshot, UserRow,
};

/// Organization the fixture seeds.
pub const ORG_NAME: &str = "load";
/// Name the server must be configured to treat as superuser.
pub const SUPERUSER: &str = "load-superuser";

/// Fixed org id, also used as the object-store owner of the blob rows.
pub const ORG_ID: &str = "load-org";
const ORG_AUTHZ: &str = "load-authz-org";

const SUPERUSER_ID: &str = "load-user";
const SUPERUSER_AUTHZ: &str = "load-authz-user";

const GROUP_ID: &str = "load-group";
const GROUP_AUTHZ: &str = "load-authz-group";
const GROUP_NAME: &str = "load-clients";

const COOKBOOK_FILES: [&str; 3] = ["recipes/default.rb", "attributes/default.rb", "metadata.rb"];

/// Recipes the static canary run list carries; it names the first
/// `CANARY_RECIPES` cookbooks of the pool.
pub const CANARY_RECIPES: usize = 5;

/// Versions the pool needs before `CANARY_RECIPES` cookbooks exist.
fn canary_versions() -> usize {
    (CANARY_RECIPES - 1) * 3 + 1
}

/// Roles a node's run list carries.
const NODE_ROLES: usize = 3;

/// The three environments the node bodies rotate through.
const ENVIRONMENTS: [&str; 3] = ["load-default", "load-constrained-a", "load-constrained-b"];

const CONTAINERS: [&str; 12] = [
    "clients",
    "containers",
    "cookbooks",
    "data",
    "environments",
    "groups",
    "nodes",
    "roles",
    "sandboxes",
    "policies",
    "policy_groups",
    "cookbook_artifacts",
];

/// Fixed timestamp every row carries.
pub(crate) fn stamp() -> String {
    "2026-01-01T00:00:00Z".to_string()
}

/// Sizes of one fixture build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizes {
    pub nodes: usize,
    pub roles: usize,
    pub versions: usize,
    pub targets: usize,
    pub canary: usize,
}

impl Default for Sizes {
    fn default() -> Self {
        Self {
            nodes: 20_000,
            roles: 1_000,
            versions: 600,
            targets: 5_000,
            canary: 20,
        }
    }
}

impl Sizes {
    /// Reject sizes the fixture cannot lay out consistently.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("nodes", self.nodes),
            ("roles", self.roles),
            ("versions", self.versions),
            ("targets", self.targets),
        ] {
            if value == 0 {
                return Err(format!("--{name} must be greater than zero"));
            }
        }
        if self.targets > self.nodes {
            return Err(format!(
                "--targets ({}) must not exceed --nodes ({})",
                self.targets, self.nodes
            ));
        }
        // Canary run lists name the first `CANARY_RECIPES` cookbooks, so the
        // pool must reach that many cookbooks before a canary can run.
        if self.canary > 0 {
            let needed = canary_versions();
            if self.versions < needed {
                return Err(format!(
                    "--versions must be at least {needed} when --canary is set: a canary run list \
                     needs a {CANARY_RECIPES}-package run list (got {})",
                    self.versions
                ));
            }
        }
        Ok(())
    }

    /// Cookbooks needed to hold `versions` versions at three versions each.
    fn cookbooks(&self) -> usize {
        self.versions.div_ceil(3)
    }
}

/// What a manifest entry stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Superuser,
    Client,
    Canary,
}

impl KeyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Superuser => "superuser",
            Self::Client => "client",
            Self::Canary => "canary",
        }
    }
}

/// One actor the seed generates a keypair for, in manifest order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEntry {
    pub kind: KeyKind,
    pub name: String,
    pub actor_id: String,
    pub authz_id: String,
}

/// A cookbook file whose MD5 is its checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookBlob {
    pub checksum: String,
    pub data: Vec<u8>,
}

/// A built fixture. `snapshot` holds the rows; `books` the blobs the seed
/// writes into the object store.
#[derive(Debug, Clone, Default)]
pub struct Fixture {
    pub sizes: Sizes,
    pub snapshot: StoreSnapshot,
    pub entries: Vec<KeyEntry>,
    pub books: Vec<BookBlob>,
}

impl Fixture {
    /// Build the snapshot for `sizes`.
    pub fn build(sizes: Sizes) -> Result<Self, String> {
        sizes.validate()?;
        let now = stamp();
        let mut fixture = Self {
            sizes,
            ..Self::default()
        };
        let snapshot = &mut fixture.snapshot;

        snapshot.orgs.push(OrgRow {
            id: ORG_ID.into(),
            authz_id: ORG_AUTHZ.into(),
            name: ORG_NAME.into(),
            full_name: "Microwave load test".into(),
            assigned_at: now.clone(),
            last_updated_by: SUPERUSER.into(),
            created_at: now.clone(),
            updated_at: now.clone(),
        });
        snapshot.authz_entities.push(entity(ORG_AUTHZ, "actor"));

        snapshot.users.push(UserRow {
            id: SUPERUSER_ID.into(),
            authz_id: SUPERUSER_AUTHZ.into(),
            username: SUPERUSER.into(),
            email: Some("load-superuser@load.invalid".into()),
            password_hash: String::new(),
            password_salt: String::new(),
            hash_type: "bcrypt".into(),
            admin: true,
            recovery_authentication_enabled: false,
            external_authentication_uid: None,
            display_name: Some(SUPERUSER.into()),
            first_name: None,
            middle_name: None,
            last_name: None,
            last_updated_by: SUPERUSER.into(),
            created_at: now.clone(),
            updated_at: now.clone(),
        });
        snapshot
            .authz_entities
            .push(entity(SUPERUSER_AUTHZ, "actor"));
        snapshot.org_user_associations.push(OrgUserAssociationRow {
            org_id: ORG_ID.into(),
            user_id: SUPERUSER_ID.into(),
            last_updated_by: SUPERUSER.into(),
            created_at: now.clone(),
            updated_at: now.clone(),
        });
        fixture.entries.push(KeyEntry {
            kind: KeyKind::Superuser,
            name: SUPERUSER.into(),
            actor_id: SUPERUSER_ID.into(),
            authz_id: SUPERUSER_AUTHZ.into(),
        });

        for (index, name) in CONTAINERS.iter().enumerate() {
            let authz_id = format!("load-authz-container-{name}");
            snapshot.containers.push(ContainerRow {
                id: format!("load-container-{index:04}"),
                org_id: ORG_ID.into(),
                authz_id: authz_id.clone(),
                name: (*name).into(),
                created_at: now.clone(),
                updated_at: now.clone(),
            });
            snapshot.authz_entities.push(entity(&authz_id, "container"));
        }
        snapshot.groups.push(GroupRow {
            id: GROUP_ID.into(),
            org_id: ORG_ID.into(),
            authz_id: GROUP_AUTHZ.into(),
            name: GROUP_NAME.into(),
            created_at: now.clone(),
            updated_at: now.clone(),
        });
        snapshot.authz_entities.push(entity(GROUP_AUTHZ, "group"));

        let mut environments = Vec::with_capacity(ENVIRONMENTS.len());
        for (index, name) in ENVIRONMENTS.iter().enumerate() {
            let authz_id = format!("load-authz-env-{:04}", index + 1);
            let mut constraints = serde_json::Map::new();
            match index {
                1 => constraints.insert("load-cb-0001".into(), "1.0.0".into()),
                2 => constraints.insert("load-cb-0002".into(), "~> 1.0".into()),
                _ => None,
            };
            let body = serde_json::json!({
                "name": name,
                "description": "load test environment",
                "json_class": "Chef::Environment",
                "chef_type": "environment",
                "cookbook_versions": constraints,
                "default_attributes": {},
                "override_attributes": {},
            });
            snapshot.environments.push(NamedObjectRow {
                id: format!("load-env-{:04}", index + 1),
                authz_id: authz_id.clone(),
                org_id: ORG_ID.into(),
                name: (*name).into(),
                serialized_object: Some(body.to_string()),
                last_updated_by: SUPERUSER.into(),
                created_at: now.clone(),
                updated_at: now.clone(),
            });
            snapshot.authz_entities.push(entity(&authz_id, "object"));
            environments.push(authz_id);
        }
        let cookbooks = sizes.cookbooks();

        for index in 0..sizes.targets {
            let (client, row, entry) = client_row(index, KeyKind::Client, &now);
            snapshot.authz_group_actors.push(AuthzGroupActorRow {
                group_authz_id: GROUP_AUTHZ.into(),
                actor_authz_id: client.authz_id.clone(),
            });
            snapshot.authz_entities.push(row);
            snapshot.clients.push(client);
            fixture.entries.push(entry);
        }
        for index in 0..sizes.canary {
            let (client, row, entry) = client_row(index, KeyKind::Canary, &now);
            snapshot.authz_group_actors.push(AuthzGroupActorRow {
                group_authz_id: GROUP_AUTHZ.into(),
                actor_authz_id: client.authz_id.clone(),
            });
            snapshot.authz_entities.push(row);
            snapshot.clients.push(client);
            fixture.entries.push(entry);
        }

        for index in 0..sizes.nodes {
            let environment = ENVIRONMENTS[index % ENVIRONMENTS.len()];
            let name = format!("load-node-{index:05}");
            let run_list: Vec<String> = (0..NODE_ROLES.min(sizes.roles))
                .map(|slot| format!("role[load-role-{:04}]", (index + slot) % sizes.roles))
                .collect();
            let body = serde_json::json!({
                "name": name,
                "chef_environment": environment,
                "json_class": "Chef::Node",
                "chef_type": "node",
                "normal": {},
                "default": {},
                "override": {},
                "automatic": {},
                "run_list": run_list,
            });
            snapshot.nodes.push(NodeRow {
                id: format!("load-node-{index:05}"),
                authz_id: format!("load-authz-node-{index:05}"),
                org_id: ORG_ID.into(),
                name,
                environment: environment.into(),
                policy_name: None,
                policy_group: None,
                serialized_object: Some(body.to_string()),
                last_updated_by: SUPERUSER.into(),
                created_at: now.clone(),
                updated_at: now.clone(),
            });
            snapshot
                .authz_entities
                .push(entity(&format!("load-authz-node-{index:05}"), "object"));
        }

        for index in 0..sizes.canary {
            let name = format!("canary-{index}");
            let authz_id = format!("load-authz-canary-node-{index:02}");
            let run_list: Vec<String> = (1..=CANARY_RECIPES)
                .map(|recipe| format!("recipe[load-cb-{recipe:04}::default]"))
                .collect();
            let body = serde_json::json!({
                "name": name,
                "chef_environment": "load-default",
                "json_class": "Chef::Node",
                "chef_type": "node",
                "normal": {},
                "default": {},
                "override": {},
                "automatic": {},
                "run_list": run_list,
            });
            snapshot.nodes.push(NodeRow {
                id: format!("load-canary-node-{index:02}"),
                authz_id: authz_id.clone(),
                org_id: ORG_ID.into(),
                name,
                environment: "load-default".into(),
                policy_name: None,
                policy_group: None,
                serialized_object: Some(body.to_string()),
                last_updated_by: SUPERUSER.into(),
                created_at: now.clone(),
                updated_at: now.clone(),
            });
            snapshot.authz_entities.push(entity(&authz_id, "object"));
        }

        for index in 0..sizes.roles {
            let name = format!("load-role-{index:04}");
            let mut run_list: Vec<String> = (0..5)
                .map(|slot| {
                    let cookbook = (index * 7 + slot) % cookbooks;
                    format!("recipe[load-cb-{:04}::default]", cookbook + 1)
                })
                .collect();
            if index % 3 == 0 && index + 1 < sizes.roles {
                run_list.push(format!("role[load-role-{:04}]", index + 1));
            }
            let body = serde_json::json!({
                "name": name,
                "description": "load test role",
                "json_class": "Chef::Role",
                "chef_type": "role",
                "run_list": run_list,
                "default_attributes": {},
                "override_attributes": {},
            });
            let authz_id = format!("load-authz-role-{index:04}");
            snapshot.roles.push(NamedObjectRow {
                id: format!("load-role-{index:04}"),
                authz_id: authz_id.clone(),
                org_id: ORG_ID.into(),
                name,
                serialized_object: Some(body.to_string()),
                last_updated_by: SUPERUSER.into(),
                created_at: now.clone(),
                updated_at: now.clone(),
            });
            snapshot.authz_entities.push(entity(&authz_id, "object"));
        }

        let mut checksums = std::collections::BTreeSet::new();
        for cookbook in 0..cookbooks {
            let name = format!("load-cb-{:04}", cookbook + 1);
            let authz_id = format!("load-authz-cb-{:04}", cookbook + 1);
            snapshot.cookbooks.push(CookbookRow {
                id: cookbook as i64 + 1,
                org_id: ORG_ID.into(),
                name: name.clone(),
                authz_id: authz_id.clone(),
            });
            snapshot.authz_entities.push(entity(&authz_id, "object"));

            for (slot, version) in ["1.0.0", "1.1.0", "2.0.0"].iter().enumerate() {
                if cookbook * 3 + slot >= sizes.versions {
                    break;
                }
                let mut dependencies = serde_json::Map::new();
                let target = cookbook + 1 + slot;
                if target < cookbooks {
                    dependencies.insert(
                        format!("load-cb-{:04}", target + 1),
                        serde_json::Value::String("~> 1.0".into()),
                    );
                }

                let mut files = Vec::new();
                let mut file_checksums = Vec::new();
                for file in COOKBOOK_FILES {
                    let data = format!("# {name} {version} {file}\n");
                    let checksum = md5_hex(data.as_bytes());
                    checksums.insert(checksum.clone());
                    files.push(serde_json::json!({
                        "name": file,
                        "path": file,
                        "specificity": "default",
                        "checksum": checksum,
                    }));
                    file_checksums.push(checksum.clone());
                    fixture.books.push(BookBlob {
                        checksum,
                        data: data.into_bytes(),
                    });
                }

                let metadata = serde_json::json!({
                    "name": name,
                    "version": version,
                    "description": "load test cookbook",
                    "dependencies": dependencies,
                });
                let manifest = serde_json::json!({
                    "name": format!("{name}-{version}"),
                    "cookbook_name": name,
                    "version": version,
                    "json_class": "Chef::CookbookVersion",
                    "chef_type": "cookbook_version",
                    "frozen?": false,
                    "metadata": metadata,
                    "all_files": files,
                });
                let serialized = manifest.to_string();
                let (major, minor, patch) =
                    microwave_store::parse_version(version).map_err(|error| error.to_string())?;
                let id = format!("load-cv-{:04}-{version}", cookbook + 1);
                snapshot.cookbook_versions.push(CookbookVersionRow {
                    id: id.clone(),
                    cookbook_id: cookbook as i64 + 1,
                    org_id: ORG_ID.into(),
                    name: name.clone(),
                    major,
                    minor,
                    patch,
                    frozen: false,
                    meta_attributes: None,
                    meta_deps: microwave_store::cookbook_dependencies(&serialized),
                    meta_long_desc: None,
                    metadata: Some(metadata.to_string()),
                    serialized_object: Some(serialized),
                    last_updated_by: SUPERUSER.into(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                });
                for checksum in file_checksums {
                    snapshot
                        .cookbook_version_checksums
                        .push(CookbookVersionChecksumRow {
                            cookbook_version_id: id.clone(),
                            org_id: ORG_ID.into(),
                            checksum,
                        });
                }
            }
        }
        for checksum in checksums {
            snapshot.checksums.push(ChecksumRow {
                org_id: ORG_ID.into(),
                checksum,
                created_at: now.clone(),
            });
        }

        grant_group_reads(snapshot, &environments, cookbooks, sizes.roles);
        push_node_acls(snapshot, sizes.targets, sizes.canary);
        Ok(fixture)
    }

    /// Org-scoped rows: the targets with their owning node, in index order.
    pub fn owned_nodes(&self) -> impl Iterator<Item = (&NodeRow, &ClientRow)> {
        self.snapshot
            .nodes
            .iter()
            .zip(self.snapshot.clients.iter())
            .take(self.sizes.targets)
    }
}

/// One engine entity row.
fn entity(authz_id: &str, entity_type: &str) -> AuthzEntityRow {
    AuthzEntityRow {
        authz_id: authz_id.into(),
        entity_type: entity_type.into(),
        org_id: Some(ORG_ID.into()),
        created_at: stamp(),
    }
}

/// A target or canary client with its authz entity and manifest entry.
fn client_row(index: usize, kind: KeyKind, now: &str) -> (ClientRow, AuthzEntityRow, KeyEntry) {
    let (id, name, authz_id) = match kind {
        KeyKind::Superuser => unreachable!("the superuser is a user"),
        KeyKind::Canary => (
            format!("load-canary-client-{index:02}"),
            format!("canary-{index}"),
            format!("load-authz-canary-client-{index:02}"),
        ),
        KeyKind::Client => (
            format!("load-client-{index:05}"),
            format!("load-client-{index:05}"),
            format!("load-authz-client-{index:05}"),
        ),
    };
    (
        ClientRow {
            id: id.clone(),
            org_id: ORG_ID.into(),
            authz_id: authz_id.clone(),
            name: name.clone(),
            validator: false,
            admin: false,
            created_at: now.into(),
            updated_at: now.into(),
        },
        entity(&authz_id, "actor"),
        KeyEntry {
            kind,
            name,
            actor_id: id,
            authz_id,
        },
    )
}

/// Give every client of the group the reads a converge needs: the containers
/// it lists through, its environments, its roles and its cookbooks.
fn grant_group_reads(
    snapshot: &mut StoreSnapshot,
    environments: &[String],
    cookbooks: usize,
    roles: usize,
) {
    let mut targets = Vec::with_capacity(5 + environments.len() + roles + cookbooks);
    for name in ["cookbooks", "environments", "roles", "sandboxes"] {
        targets.push((format!("load-authz-container-{name}"), "read"));
    }
    // `--upload` creates a sandbox before it presigns a blob.
    targets.push(("load-authz-container-sandboxes".into(), "create"));
    targets.extend(environments.iter().map(|id| (id.clone(), "read")));
    targets.extend((0..roles).map(|index| (format!("load-authz-role-{index:04}"), "read")));
    targets.extend((0..cookbooks).map(|index| (format!("load-authz-cb-{:04}", index + 1), "read")));
    for (target, permission) in targets {
        snapshot.authz_acl_group.push(AuthzAclGroupRow {
            target_authz_id: target,
            permission: permission.into(),
            group_authz_id: GROUP_AUTHZ.into(),
        });
    }
}

/// Read and update on its own node for every client that owns one. Target
/// clients come first in `clients`, then canaries, exactly like their nodes.
fn push_node_acls(snapshot: &mut StoreSnapshot, targets: usize, canary: usize) {
    let mut push = |node: String, client: usize| {
        let actor_authz_id = snapshot.clients[client].authz_id.clone();
        for permission in ["read", "update"] {
            snapshot.authz_acl_actor.push(AuthzAclActorRow {
                target_authz_id: node.clone(),
                permission: permission.into(),
                actor_authz_id: actor_authz_id.clone(),
            });
        }
    };
    for index in 0..targets {
        push(format!("load-authz-node-{index:05}"), index);
    }
    for index in 0..canary {
        push(
            format!("load-authz-canary-node-{index:02}"),
            targets + index,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> Sizes {
        Sizes {
            nodes: 50,
            roles: 8,
            versions: 18,
            targets: 6,
            canary: 2,
        }
    }

    fn json(sizes: Sizes) -> String {
        let fixture = Fixture::build(sizes).expect("fixture builds");
        serde_json::to_string(&fixture.snapshot).expect("snapshot serializes")
    }

    #[test]
    fn two_builds_are_byte_identical() {
        assert_eq!(json(small()), json(small()));

        let first = Fixture::build(small()).expect("fixture builds");
        let second = Fixture::build(small()).expect("fixture builds");
        assert_eq!(first.books, second.books);
        assert!(first.books.len() >= small().versions);
    }

    #[test]
    fn every_target_client_may_read_and_update_its_node() {
        let fixture = Fixture::build(small()).expect("fixture builds");
        let acls = &fixture.snapshot.authz_acl_actor;
        let mut checked = 0;
        for (node, client) in fixture.owned_nodes() {
            for permission in ["read", "update"] {
                assert!(
                    acls.iter().any(|row| row.target_authz_id == node.authz_id
                        && row.actor_authz_id == client.authz_id
                        && row.permission == permission),
                    "{} lacks {permission} on {}",
                    client.name,
                    node.name
                );
                checked += 1;
            }
        }
        assert_eq!(checked, small().targets * 2);
    }

    #[test]
    fn sizes_drive_the_row_counts() {
        let sizes = Sizes {
            nodes: 10,
            roles: 4,
            versions: 6,
            targets: 4,
            canary: 0,
        };
        let fixture = Fixture::build(sizes).expect("fixture builds");
        assert_eq!(fixture.snapshot.nodes.len(), 10);
        assert_eq!(fixture.snapshot.roles.len(), 4);
        assert_eq!(fixture.snapshot.cookbook_versions.len(), 6);
        let clients = fixture
            .entries
            .iter()
            .filter(|entry| entry.kind == KeyKind::Client)
            .count();
        assert_eq!(clients, 4);
        assert_eq!(fixture.entries.len(), 5, "four clients plus the superuser");
    }

    #[test]
    fn every_authz_id_is_one_typed_entity() {
        let fixture = Fixture::build(small()).expect("fixture builds");
        let mut seen = std::collections::BTreeMap::new();
        for entity in &fixture.snapshot.authz_entities {
            assert!(
                seen.insert(entity.authz_id.clone(), entity.entity_type.clone())
                    .is_none(),
                "duplicate authz id {}",
                entity.authz_id
            );
        }
        for row in &fixture.snapshot.authz_acl_actor {
            let target = seen.get(&row.target_authz_id).expect("ACL target exists");
            assert!(
                matches!(target.as_str(), "object" | "container"),
                "ACL target {} is a {target}",
                row.target_authz_id
            );
            let actor = seen.get(&row.actor_authz_id).expect("ACL actor exists");
            assert!(
                matches!(actor.as_str(), "actor" | "group"),
                "ACL actor {} is a {actor}",
                row.actor_authz_id
            );
        }
        for row in &fixture.snapshot.authz_acl_group {
            let target = seen
                .get(&row.target_authz_id)
                .expect("group ACL target exists");
            assert!(
                matches!(target.as_str(), "object" | "container"),
                "group ACL target {} is a {target}",
                row.target_authz_id
            );
            assert_eq!(
                seen.get(&row.group_authz_id).map(String::as_str),
                Some("group")
            );
        }
        for row in &fixture.snapshot.authz_group_actors {
            assert_eq!(
                seen.get(&row.group_authz_id).map(String::as_str),
                Some("group")
            );
            assert_eq!(
                seen.get(&row.actor_authz_id).map(String::as_str),
                Some("actor")
            );
        }
    }

    #[test]
    fn every_node_carries_three_distinct_roles() {
        let fixture = Fixture::build(small()).expect("fixture builds");
        let nodes = fixture
            .snapshot
            .nodes
            .iter()
            .filter(|node| node.name.starts_with("load-node-"));
        for node in nodes {
            let body: serde_json::Value =
                serde_json::from_str(node.serialized_object.as_deref().expect("node body"))
                    .expect("node body parses");
            let run_list: Vec<&str> = body["run_list"]
                .as_array()
                .expect("run list is an array")
                .iter()
                .map(|entry| entry.as_str().expect("run list entries are strings"))
                .collect();
            assert_eq!(run_list.len(), NODE_ROLES);
            let distinct: std::collections::BTreeSet<_> = run_list.iter().collect();
            assert_eq!(distinct.len(), NODE_ROLES, "roles repeat in {}", node.name);
        }
    }

    #[test]
    fn the_client_group_may_create_sandboxes() {
        let fixture = Fixture::build(small()).expect("fixture builds");
        let sandboxes = fixture
            .snapshot
            .containers
            .iter()
            .find(|container| container.name == "sandboxes")
            .expect("sandbox container exists");
        for permission in ["read", "create"] {
            assert!(
                fixture.snapshot.authz_acl_group.iter().any(|row| {
                    row.target_authz_id == sandboxes.authz_id
                        && row.group_authz_id == GROUP_AUTHZ
                        && row.permission == permission
                }),
                "group lacks {permission} on sandboxes"
            );
        }
    }

    #[test]
    fn the_canary_guard_names_the_run_list_width() {
        let mut sizes = small();
        sizes.versions = 6;
        let error = Fixture::build(sizes).expect_err("6 versions cannot feed five cookbooks");
        assert!(error.contains("at least 13"), "{error}");
        assert!(error.contains("5-package run list"), "{error}");
    }

    #[test]
    fn no_parameter_yields_errors() {
        let mut sizes = small();
        sizes.targets = sizes.nodes + 1;
        assert!(Fixture::build(sizes).is_err());
        sizes = small();
        sizes.roles = 0;
        assert!(Fixture::build(sizes).is_err());
        sizes = small();
        sizes.versions = 6;
        assert!(
            Fixture::build(sizes).is_err(),
            "canaries need five cookbooks"
        );
        sizes = small();
        sizes.versions = 6;
        sizes.canary = 0;
        assert!(Fixture::build(sizes).is_ok());
    }
}
