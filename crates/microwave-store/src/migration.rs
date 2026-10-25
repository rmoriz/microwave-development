//! Full-fidelity export/import of the local metadata store.
//!
//! The snapshot is deliberately table-shaped: every row keeps its original
//! primary keys, authz ids and timestamps so an imported server behaves like
//! the source. Password hashes, salts and hash types are preserved; plaintext
//! passwords and private keys are never stored by Microwave and therefore
//! never exported.

use crate::compression::{decode_serialized_optional, encode_serialized_optional, Compression};
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, Connection, Row, Transaction};
use serde::{Deserialize, Serialize};

/// A complete logical dump of a local Microwave store.
///
/// Every collection is optional when deserializing so older or partial
/// snapshots remain readable. Unknown fields from newer writers are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreSnapshot {
    pub orgs: Vec<OrgRow>,
    pub users: Vec<UserRow>,
    pub org_user_associations: Vec<OrgUserAssociationRow>,
    pub org_user_invites: Vec<OrgUserInviteRow>,
    pub clients: Vec<ClientRow>,
    pub actor_keys: Vec<ActorKeyRow>,
    pub authz_entities: Vec<AuthzEntityRow>,
    pub authz_acl_actor: Vec<AuthzAclActorRow>,
    pub authz_acl_group: Vec<AuthzAclGroupRow>,
    pub authz_group_actors: Vec<AuthzGroupActorRow>,
    pub authz_group_groups: Vec<AuthzGroupGroupRow>,
    pub containers: Vec<ContainerRow>,
    pub groups: Vec<GroupRow>,
    pub global_groups: Vec<GlobalGroupRow>,
    pub nodes: Vec<NodeRow>,
    pub roles: Vec<NamedObjectRow>,
    pub environments: Vec<NamedObjectRow>,
    pub data_bags: Vec<NamedObjectRow>,
    pub data_bag_items: Vec<DataBagItemRow>,
    pub checksums: Vec<ChecksumRow>,
    pub sandboxed_checksums: Vec<SandboxedChecksumRow>,
    pub cookbooks: Vec<CookbookRow>,
    pub cookbook_versions: Vec<CookbookVersionRow>,
    pub cookbook_version_checksums: Vec<CookbookVersionChecksumRow>,
    pub cookbook_artifacts: Vec<CookbookArtifactRow>,
    pub cookbook_artifact_versions: Vec<CookbookArtifactVersionRow>,
    pub cookbook_artifact_version_checksums: Vec<CookbookArtifactVersionChecksumRow>,
    pub policies: Vec<PolicyRow>,
    pub policy_groups: Vec<PolicyGroupRow>,
    pub policy_revisions: Vec<PolicyRevisionRow>,
    pub policy_group_revision_associations: Vec<PolicyGroupRevisionAssociationRow>,
    pub oauth_applications: Vec<OAuthApplicationRow>,
    /// Latest OAuth access grants. Secret material; omitted from safe exports.
    pub oauth_access_grants: Vec<OAuthAccessGrantRow>,
    /// Latest OAuth access tokens. Secret material; omitted from safe exports.
    pub oauth_access_tokens: Vec<OAuthAccessTokenRow>,
    /// Raw `kvpair` rows, including `secret:*`, session, reset, and
    /// `maintenance:enabled`. Secret material; omitted from safe exports.
    pub kvpair: Vec<KvPairRow>,
    /// Explicit sandbox rows. Carries empty in-flight sandboxes that have no
    /// uploaded checksums yet.
    pub sandboxes: Vec<SandboxRow>,
    /// Pending sandbox upload rows used by SQLite's commit path. PostgreSQL
    /// derives this collection from `sandboxed_checksums` instead.
    pub sandbox_checksums: Vec<SandboxChecksumRow>,
    /// Telemetry event rows, when the source table exists.
    pub telemetry: Vec<TelemetryRow>,
}

impl StoreSnapshot {
    /// Remove every secret-bearing value from this snapshot.
    ///
    /// Callers writing a default (safe) export use this before serializing.
    /// The returned document still carries metadata, password hashes, public
    /// keys, and ACLs, so it remains sensitive; it simply no longer contains
    /// long-lived bearer tokens or operator secret values.
    pub fn redact_secrets(&mut self) {
        self.kvpair.clear();
        self.oauth_access_grants.clear();
        self.oauth_access_tokens.clear();
        for application in &mut self.oauth_applications {
            application.secret.clear();
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OrgRow {
    pub id: String,
    pub authz_id: String,
    pub name: String,
    pub full_name: String,
    pub assigned_at: String,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserRow {
    pub id: String,
    pub authz_id: String,
    pub username: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub password_salt: String,
    pub hash_type: String,
    pub admin: bool,
    pub recovery_authentication_enabled: bool,
    pub external_authentication_uid: Option<String>,
    pub display_name: Option<String>,
    pub first_name: Option<String>,
    pub middle_name: Option<String>,
    pub last_name: Option<String>,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OrgUserAssociationRow {
    pub org_id: String,
    pub user_id: String,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OrgUserInviteRow {
    pub id: String,
    pub org_id: String,
    pub user_id: String,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientRow {
    pub id: String,
    pub org_id: String,
    pub authz_id: String,
    pub name: String,
    pub validator: bool,
    pub admin: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActorKeyRow {
    pub actor_id: String,
    pub actor_type: String,
    pub key_name: String,
    pub public_key: String,
    pub key_version: i64,
    pub expires_at: String,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthzEntityRow {
    pub authz_id: String,
    pub entity_type: String,
    pub org_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthzAclActorRow {
    pub target_authz_id: String,
    pub permission: String,
    pub actor_authz_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthzAclGroupRow {
    pub target_authz_id: String,
    pub permission: String,
    pub group_authz_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthzGroupActorRow {
    pub group_authz_id: String,
    pub actor_authz_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthzGroupGroupRow {
    pub parent_group_authz_id: String,
    pub child_group_authz_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerRow {
    pub id: String,
    pub org_id: String,
    pub authz_id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupRow {
    pub id: String,
    pub org_id: String,
    pub authz_id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalGroupRow {
    pub id: String,
    pub authz_id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeRow {
    pub id: String,
    pub authz_id: String,
    pub org_id: String,
    pub name: String,
    pub environment: String,
    pub policy_name: Option<String>,
    pub policy_group: Option<String>,
    pub serialized_object: Option<String>,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NamedObjectRow {
    pub id: String,
    pub authz_id: String,
    pub org_id: String,
    pub name: String,
    pub serialized_object: Option<String>,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataBagItemRow {
    pub id: String,
    pub org_id: String,
    pub data_bag_name: String,
    pub item_name: String,
    pub serialized_object: Option<String>,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChecksumRow {
    pub org_id: String,
    pub checksum: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxedChecksumRow {
    pub org_id: String,
    pub sandbox_id: String,
    pub checksum: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CookbookRow {
    pub id: i64,
    pub org_id: String,
    pub name: String,
    pub authz_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CookbookVersionRow {
    pub id: String,
    pub cookbook_id: i64,
    pub org_id: String,
    pub name: String,
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
    pub frozen: bool,
    pub meta_attributes: Option<String>,
    pub meta_deps: Option<String>,
    pub meta_long_desc: Option<String>,
    pub metadata: Option<String>,
    pub serialized_object: Option<String>,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CookbookVersionChecksumRow {
    pub cookbook_version_id: String,
    pub org_id: String,
    pub checksum: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CookbookArtifactRow {
    pub id: i64,
    pub org_id: String,
    pub name: String,
    pub authz_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CookbookArtifactVersionRow {
    pub id: i64,
    pub cookbook_artifact_id: i64,
    pub org_id: String,
    pub identifier: String,
    pub metadata: Option<String>,
    pub serialized_object: Option<String>,
    pub created_at: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CookbookArtifactVersionChecksumRow {
    pub cookbook_artifact_version_id: i64,
    pub org_id: String,
    pub checksum: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyRow {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub authz_id: String,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyGroupRow {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub authz_id: String,
    pub serialized_object: Option<String>,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyRevisionRow {
    pub id: String,
    pub org_id: String,
    pub policy_id: String,
    pub revision_id: String,
    pub name: String,
    pub serialized_object: Option<String>,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyGroupRevisionAssociationRow {
    pub id: String,
    pub org_id: String,
    pub policy_revision_id: String,
    pub policy_group_authz_id: String,
    pub policy_revision_name: String,
    pub policy_revision_revision_id: String,
    pub policy_group_name: String,
    pub last_updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuthApplicationRow {
    pub id: i64,
    pub name: String,
    pub uid: String,
    pub secret: String,
    pub redirect_uri: String,
    pub scopes: String,
    pub confidential: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuthAccessGrantRow {
    pub id: i64,
    pub resource_owner_id: String,
    pub application_id: i64,
    pub token: String,
    pub expires_in: i64,
    pub redirect_uri: String,
    pub scopes: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuthAccessTokenRow {
    pub id: i64,
    pub resource_owner_id: Option<String>,
    pub application_id: Option<i64>,
    pub token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub scopes: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KvPairRow {
    pub key: String,
    pub value: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxRow {
    pub org_id: String,
    pub sandbox_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxChecksumRow {
    pub org_id: String,
    pub sandbox_id: String,
    pub checksum: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryRow {
    pub property: String,
    pub event_timestamp: String,
}

impl SqliteStore {
    /// Read every migration-relevant table into a [`StoreSnapshot`].
    ///
    /// This full-fidelity form includes secret-bearing `kvpair`, OAuth grant
    /// and token rows, and OAuth application secrets. The `microwave-migrate`
    /// command applies [`StoreSnapshot::redact_secrets`] unless the operator
    /// passes `--include-secrets`.
    pub fn export_snapshot(&self) -> Result<StoreSnapshot, StoreError> {
        self.export_snapshot_with_options(true)
    }

    /// Read every migration-relevant table, optionally excluding secrets.
    pub fn export_snapshot_with_options(
        &self,
        include_secrets: bool,
    ) -> Result<StoreSnapshot, StoreError> {
        let connection = self.connection();
        let mut snapshot = StoreSnapshot {
            orgs: collect(
                &connection,
                "SELECT id, authz_id, name, full_name, assigned_at, last_updated_by,
                        created_at, updated_at
                 FROM orgs ORDER BY id",
                |row| {
                    Ok(OrgRow {
                        id: row.get(0)?,
                        authz_id: row.get(1)?,
                        name: row.get(2)?,
                        full_name: row.get(3)?,
                        assigned_at: row.get(4)?,
                        last_updated_by: row.get(5)?,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )?,
            users: collect(
                &connection,
                "SELECT id, authz_id, username, email, password_hash, password_salt,
                        hash_type, admin, recovery_authentication_enabled,
                        external_authentication_uid, display_name, first_name, middle_name,
                        last_name, last_updated_by, created_at, updated_at
                 FROM users ORDER BY id",
                |row| {
                    Ok(UserRow {
                        id: row.get(0)?,
                        authz_id: row.get(1)?,
                        username: row.get(2)?,
                        email: row.get(3)?,
                        password_hash: row.get(4)?,
                        password_salt: row.get(5)?,
                        hash_type: row.get(6)?,
                        admin: row.get::<_, i64>(7)? != 0,
                        recovery_authentication_enabled: row.get::<_, i64>(8)? != 0,
                        external_authentication_uid: row.get(9)?,
                        display_name: row.get(10)?,
                        first_name: row.get(11)?,
                        middle_name: row.get(12)?,
                        last_name: row.get(13)?,
                        last_updated_by: row.get(14)?,
                        created_at: row.get(15)?,
                        updated_at: row.get(16)?,
                    })
                },
            )?,
            org_user_associations: collect(
                &connection,
                "SELECT org_id, user_id, last_updated_by, created_at, updated_at
                 FROM org_user_associations ORDER BY org_id, user_id",
                |row| {
                    Ok(OrgUserAssociationRow {
                        org_id: row.get(0)?,
                        user_id: row.get(1)?,
                        last_updated_by: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )?,
            org_user_invites: collect(
                &connection,
                "SELECT id, org_id, user_id, last_updated_by, created_at, updated_at
                 FROM org_user_invites ORDER BY id",
                |row| {
                    Ok(OrgUserInviteRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        user_id: row.get(2)?,
                        last_updated_by: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )?,
            clients: collect(
                &connection,
                "SELECT id, org_id, authz_id, name, validator, admin, created_at, updated_at
                 FROM clients ORDER BY id",
                |row| {
                    Ok(ClientRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        authz_id: row.get(2)?,
                        name: row.get(3)?,
                        validator: row.get::<_, i64>(4)? != 0,
                        admin: row.get::<_, i64>(5)? != 0,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )?,
            actor_keys: collect(
                &connection,
                "SELECT actor_id, actor_type, key_name, public_key, key_version,
                        expires_at, last_updated_by, created_at, updated_at
                 FROM actor_keys ORDER BY actor_id, actor_type, key_name",
                |row| {
                    Ok(ActorKeyRow {
                        actor_id: row.get(0)?,
                        actor_type: row.get(1)?,
                        key_name: row.get(2)?,
                        public_key: row.get(3)?,
                        key_version: row.get(4)?,
                        expires_at: row.get(5)?,
                        last_updated_by: row.get(6)?,
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
                    })
                },
            )?,
            authz_entities: collect(
                &connection,
                "SELECT authz_id, entity_type, org_id, created_at
                 FROM authz_entities ORDER BY authz_id",
                |row| {
                    Ok(AuthzEntityRow {
                        authz_id: row.get(0)?,
                        entity_type: row.get(1)?,
                        org_id: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )?,
            authz_acl_actor: collect(
                &connection,
                "SELECT target_authz_id, permission, actor_authz_id
                 FROM authz_acl_actor ORDER BY target_authz_id, permission, actor_authz_id",
                |row| {
                    Ok(AuthzAclActorRow {
                        target_authz_id: row.get(0)?,
                        permission: row.get(1)?,
                        actor_authz_id: row.get(2)?,
                    })
                },
            )?,
            authz_acl_group: collect(
                &connection,
                "SELECT target_authz_id, permission, group_authz_id
                 FROM authz_acl_group ORDER BY target_authz_id, permission, group_authz_id",
                |row| {
                    Ok(AuthzAclGroupRow {
                        target_authz_id: row.get(0)?,
                        permission: row.get(1)?,
                        group_authz_id: row.get(2)?,
                    })
                },
            )?,
            authz_group_actors: collect(
                &connection,
                "SELECT group_authz_id, actor_authz_id
                 FROM authz_group_actors ORDER BY group_authz_id, actor_authz_id",
                |row| {
                    Ok(AuthzGroupActorRow {
                        group_authz_id: row.get(0)?,
                        actor_authz_id: row.get(1)?,
                    })
                },
            )?,
            authz_group_groups: collect(
                &connection,
                "SELECT parent_group_authz_id, child_group_authz_id
                 FROM authz_group_groups ORDER BY parent_group_authz_id, child_group_authz_id",
                |row| {
                    Ok(AuthzGroupGroupRow {
                        parent_group_authz_id: row.get(0)?,
                        child_group_authz_id: row.get(1)?,
                    })
                },
            )?,
            containers: collect(
                &connection,
                "SELECT id, org_id, authz_id, name, created_at, updated_at
                 FROM containers ORDER BY id",
                |row| {
                    Ok(ContainerRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        authz_id: row.get(2)?,
                        name: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )?,
            groups: collect(
                &connection,
                "SELECT id, org_id, authz_id, name, created_at, updated_at
                 FROM groups ORDER BY id",
                |row| {
                    Ok(GroupRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        authz_id: row.get(2)?,
                        name: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )?,
            global_groups: collect(
                &connection,
                "SELECT id, authz_id, name, created_at, updated_at
                 FROM global_groups ORDER BY id",
                |row| {
                    Ok(GlobalGroupRow {
                        id: row.get(0)?,
                        authz_id: row.get(1)?,
                        name: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )?,
            nodes: collect(
                &connection,
                "SELECT id, authz_id, org_id, name, environment, policy_name, policy_group,
                        serialized_object, last_updated_by, created_at, updated_at
                 FROM nodes ORDER BY id",
                |row| {
                    Ok(NodeRow {
                        id: row.get(0)?,
                        authz_id: row.get(1)?,
                        org_id: row.get(2)?,
                        name: row.get(3)?,
                        environment: row.get(4)?,
                        policy_name: row.get(5)?,
                        policy_group: row.get(6)?,
                        serialized_object: row.get(7)?,
                        last_updated_by: row.get(8)?,
                        created_at: row.get(9)?,
                        updated_at: row.get(10)?,
                    })
                },
            )?,
            roles: collect(
                &connection,
                "SELECT id, authz_id, org_id, name, serialized_object,
                        last_updated_by, created_at, updated_at
                 FROM roles ORDER BY id",
                named_object_from_row,
            )?,
            environments: collect(
                &connection,
                "SELECT id, authz_id, org_id, name, serialized_object,
                        last_updated_by, created_at, updated_at
                 FROM environments ORDER BY id",
                named_object_from_row,
            )?,
            data_bags: collect(
                &connection,
                "SELECT id, authz_id, org_id, name, serialized_object,
                        last_updated_by, created_at, updated_at
                 FROM data_bags ORDER BY id",
                named_object_from_row,
            )?,
            data_bag_items: collect(
                &connection,
                "SELECT id, org_id, data_bag_name, item_name, serialized_object,
                        last_updated_by, created_at, updated_at
                 FROM data_bag_items ORDER BY id",
                |row| {
                    Ok(DataBagItemRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        data_bag_name: row.get(2)?,
                        item_name: row.get(3)?,
                        serialized_object: row.get(4)?,
                        last_updated_by: row.get(5)?,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )?,
            checksums: collect(
                &connection,
                "SELECT org_id, checksum, created_at FROM checksums ORDER BY org_id, checksum",
                |row| {
                    Ok(ChecksumRow {
                        org_id: row.get(0)?,
                        checksum: row.get(1)?,
                        created_at: row.get(2)?,
                    })
                },
            )?,
            sandboxed_checksums: collect(
                &connection,
                "SELECT org_id, sandbox_id, checksum, created_at
                 FROM sandboxed_checksums ORDER BY org_id, sandbox_id, checksum",
                |row| {
                    Ok(SandboxedChecksumRow {
                        org_id: row.get(0)?,
                        sandbox_id: row.get(1)?,
                        checksum: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )?,
            cookbooks: collect(
                &connection,
                "SELECT id, org_id, name, authz_id FROM cookbooks ORDER BY id",
                |row| {
                    Ok(CookbookRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        name: row.get(2)?,
                        authz_id: row.get(3)?,
                    })
                },
            )?,
            cookbook_versions: collect(
                &connection,
                "SELECT id, cookbook_id, org_id, name, major, minor, patch, frozen,
                        meta_attributes, meta_deps, meta_long_desc, metadata,
                        serialized_object, last_updated_by, created_at, updated_at
                 FROM cookbook_versions ORDER BY id",
                |row| {
                    Ok(CookbookVersionRow {
                        id: row.get(0)?,
                        cookbook_id: row.get(1)?,
                        org_id: row.get(2)?,
                        name: row.get(3)?,
                        major: row.get(4)?,
                        minor: row.get(5)?,
                        patch: row.get(6)?,
                        frozen: row.get::<_, i64>(7)? != 0,
                        meta_attributes: row.get(8)?,
                        meta_deps: row.get(9)?,
                        meta_long_desc: row.get(10)?,
                        metadata: row.get(11)?,
                        serialized_object: row.get(12)?,
                        last_updated_by: row.get(13)?,
                        created_at: row.get(14)?,
                        updated_at: row.get(15)?,
                    })
                },
            )?,
            cookbook_version_checksums: collect(
                &connection,
                "SELECT cookbook_version_id, org_id, checksum
                 FROM cookbook_version_checksums
                 ORDER BY cookbook_version_id, checksum",
                |row| {
                    Ok(CookbookVersionChecksumRow {
                        cookbook_version_id: row.get(0)?,
                        org_id: row.get(1)?,
                        checksum: row.get(2)?,
                    })
                },
            )?,
            cookbook_artifacts: collect(
                &connection,
                "SELECT id, org_id, name, authz_id FROM cookbook_artifacts ORDER BY id",
                |row| {
                    Ok(CookbookArtifactRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        name: row.get(2)?,
                        authz_id: row.get(3)?,
                    })
                },
            )?,
            cookbook_artifact_versions: collect(
                &connection,
                "SELECT id, cookbook_artifact_id, org_id, identifier, metadata,
                        serialized_object, created_at, created_by
                 FROM cookbook_artifact_versions ORDER BY id",
                |row| {
                    Ok(CookbookArtifactVersionRow {
                        id: row.get(0)?,
                        cookbook_artifact_id: row.get(1)?,
                        org_id: row.get(2)?,
                        identifier: row.get(3)?,
                        metadata: row.get(4)?,
                        serialized_object: row.get(5)?,
                        created_at: row.get(6)?,
                        created_by: row.get(7)?,
                    })
                },
            )?,
            cookbook_artifact_version_checksums: collect(
                &connection,
                "SELECT cookbook_artifact_version_id, org_id, checksum
                 FROM cookbook_artifact_version_checksums
                 ORDER BY cookbook_artifact_version_id, checksum",
                |row| {
                    Ok(CookbookArtifactVersionChecksumRow {
                        cookbook_artifact_version_id: row.get(0)?,
                        org_id: row.get(1)?,
                        checksum: row.get(2)?,
                    })
                },
            )?,
            policies: collect(
                &connection,
                "SELECT id, org_id, name, authz_id, last_updated_by, created_at, updated_at
                 FROM policies ORDER BY id",
                |row| {
                    Ok(PolicyRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        name: row.get(2)?,
                        authz_id: row.get(3)?,
                        last_updated_by: row.get(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                    })
                },
            )?,
            policy_groups: collect(
                &connection,
                "SELECT id, org_id, name, authz_id, serialized_object,
                        last_updated_by, created_at, updated_at
                 FROM policy_groups ORDER BY id",
                |row| {
                    Ok(PolicyGroupRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        name: row.get(2)?,
                        authz_id: row.get(3)?,
                        serialized_object: row.get(4)?,
                        last_updated_by: row.get(5)?,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )?,
            policy_revisions: collect(
                &connection,
                "SELECT id, org_id, policy_id, revision_id, name, serialized_object,
                        last_updated_by, created_at, updated_at
                 FROM policy_revisions ORDER BY id",
                |row| {
                    Ok(PolicyRevisionRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        policy_id: row.get(2)?,
                        revision_id: row.get(3)?,
                        name: row.get(4)?,
                        serialized_object: row.get(5)?,
                        last_updated_by: row.get(6)?,
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
                    })
                },
            )?,
            policy_group_revision_associations: collect(
                &connection,
                "SELECT id, org_id, policy_revision_id, policy_group_authz_id,
                        policy_revision_name, policy_revision_revision_id,
                        policy_group_name, last_updated_by, created_at, updated_at
                 FROM policy_group_revision_associations ORDER BY id",
                |row| {
                    Ok(PolicyGroupRevisionAssociationRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        policy_revision_id: row.get(2)?,
                        policy_group_authz_id: row.get(3)?,
                        policy_revision_name: row.get(4)?,
                        policy_revision_revision_id: row.get(5)?,
                        policy_group_name: row.get(6)?,
                        last_updated_by: row.get(7)?,
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )?,
            oauth_applications: collect(
                &connection,
                "SELECT id, name, uid, secret, redirect_uri, scopes, confidential,
                        created_at, updated_at
                 FROM oauth_applications ORDER BY id",
                |row| {
                    Ok(OAuthApplicationRow {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        uid: row.get(2)?,
                        secret: row.get(3)?,
                        redirect_uri: row.get(4)?,
                        scopes: row.get(5)?,
                        confidential: row.get::<_, i64>(6)? != 0,
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
                    })
                },
            )?,
            oauth_access_grants: if table_exists(&connection, "oauth_access_grants")? {
                collect(
                    &connection,
                    "SELECT id, resource_owner_id, application_id, token, expires_in,
                            redirect_uri, scopes, created_at, revoked_at
                     FROM oauth_access_grants ORDER BY id",
                    |row| {
                        Ok(OAuthAccessGrantRow {
                            id: row.get(0)?,
                            resource_owner_id: row.get(1)?,
                            application_id: row.get(2)?,
                            token: row.get(3)?,
                            expires_in: row.get(4)?,
                            redirect_uri: row.get(5)?,
                            scopes: row.get(6)?,
                            created_at: row.get(7)?,
                            revoked_at: row.get(8)?,
                        })
                    },
                )?
            } else {
                Vec::new()
            },
            oauth_access_tokens: if table_exists(&connection, "oauth_access_tokens")? {
                collect(
                    &connection,
                    "SELECT id, resource_owner_id, application_id, token, refresh_token,
                            expires_in, scopes, created_at, revoked_at
                     FROM oauth_access_tokens ORDER BY id",
                    |row| {
                        Ok(OAuthAccessTokenRow {
                            id: row.get(0)?,
                            resource_owner_id: row.get(1)?,
                            application_id: row.get(2)?,
                            token: row.get(3)?,
                            refresh_token: row.get(4)?,
                            expires_in: row.get(5)?,
                            scopes: row.get(6)?,
                            created_at: row.get(7)?,
                            revoked_at: row.get(8)?,
                        })
                    },
                )?
            } else {
                Vec::new()
            },
            kvpair: if table_exists(&connection, "kvpair")? {
                collect(
                    &connection,
                    "SELECT key, value, updated_at FROM kvpair ORDER BY key",
                    |row| {
                        Ok(KvPairRow {
                            key: row.get(0)?,
                            value: row.get(1)?,
                            updated_at: row.get(2)?,
                        })
                    },
                )?
            } else {
                Vec::new()
            },
            sandboxes: if table_exists(&connection, "sandboxes")? {
                collect(
                    &connection,
                    "SELECT org_id, sandbox_id, created_at
                     FROM sandboxes ORDER BY org_id, sandbox_id",
                    |row| {
                        Ok(SandboxRow {
                            org_id: row.get(0)?,
                            sandbox_id: row.get(1)?,
                            created_at: row.get(2)?,
                        })
                    },
                )?
            } else {
                Vec::new()
            },
            sandbox_checksums: if table_exists(&connection, "sandbox_checksums")? {
                collect(
                    &connection,
                    "SELECT org_id, sandbox_id, checksum, created_at
                     FROM sandbox_checksums ORDER BY org_id, sandbox_id, checksum",
                    |row| {
                        Ok(SandboxChecksumRow {
                            org_id: row.get(0)?,
                            sandbox_id: row.get(1)?,
                            checksum: row.get(2)?,
                            created_at: row.get(3)?,
                        })
                    },
                )?
            } else {
                Vec::new()
            },
            telemetry: if table_exists(&connection, "telemetry")? {
                collect(
                    &connection,
                    "SELECT property, event_timestamp
                     FROM telemetry ORDER BY event_timestamp, property",
                    |row| {
                        Ok(TelemetryRow {
                            property: row.get(0)?,
                            event_timestamp: row.get(1)?,
                        })
                    },
                )?
            } else {
                Vec::new()
            },
        };
        if !include_secrets {
            snapshot.redact_secrets();
        }
        decode_snapshot_serialized(&mut snapshot)?;
        Ok(snapshot)
    }

    /// Import a snapshot into this store in one transaction.
    ///
    /// Rows are upserted by primary key, so importing the same snapshot twice
    /// is safe and preserves the source rows' ids, authz ids and timestamps.
    pub fn import_snapshot(&self, snapshot: &StoreSnapshot) -> Result<(), StoreError> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        import_all(&transaction, snapshot, self.compression())?;
        transaction.commit()?;
        Ok(())
    }
}

fn collect<T, F>(connection: &Connection, sql: &str, map: F) -> Result<Vec<T>, StoreError>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
{
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], map)?;
    let mut values = Vec::new();
    for row in rows {
        values.push(row?);
    }
    Ok(values)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, StoreError> {
    let exists: i64 = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

/// Snapshots always carry plain JSON, never the storage-level `mwz1:` form.
fn decode_snapshot_serialized(snapshot: &mut StoreSnapshot) -> Result<(), StoreError> {
    for row in &mut snapshot.nodes {
        decode_optional(&mut row.serialized_object)?;
    }
    for row in &mut snapshot.roles {
        decode_optional(&mut row.serialized_object)?;
    }
    for row in &mut snapshot.environments {
        decode_optional(&mut row.serialized_object)?;
    }
    for row in &mut snapshot.data_bags {
        decode_optional(&mut row.serialized_object)?;
    }
    for row in &mut snapshot.data_bag_items {
        decode_optional(&mut row.serialized_object)?;
    }
    for row in &mut snapshot.cookbook_versions {
        decode_optional(&mut row.serialized_object)?;
    }
    for row in &mut snapshot.cookbook_artifact_versions {
        decode_optional(&mut row.serialized_object)?;
    }
    for row in &mut snapshot.policy_groups {
        decode_optional(&mut row.serialized_object)?;
    }
    for row in &mut snapshot.policy_revisions {
        decode_optional(&mut row.serialized_object)?;
    }
    Ok(())
}

fn decode_optional(value: &mut Option<String>) -> Result<(), StoreError> {
    *value = decode_serialized_optional(value.as_deref())?;
    Ok(())
}

fn named_object_from_row(row: &Row<'_>) -> rusqlite::Result<NamedObjectRow> {
    Ok(NamedObjectRow {
        id: row.get(0)?,
        authz_id: row.get(1)?,
        org_id: row.get(2)?,
        name: row.get(3)?,
        serialized_object: row.get(4)?,
        last_updated_by: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn import_all(
    transaction: &Transaction<'_>,
    snapshot: &StoreSnapshot,
    compression: Compression,
) -> Result<(), StoreError> {
    for row in &snapshot.orgs {
        transaction.execute(
            "INSERT INTO orgs
             (id, authz_id, name, full_name, assigned_at, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 authz_id = excluded.authz_id,
                 name = excluded.name,
                 full_name = excluded.full_name,
                 assigned_at = excluded.assigned_at,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.authz_id,
                row.name,
                row.full_name,
                row.assigned_at,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.users {
        transaction.execute(
            "INSERT INTO users
             (id, authz_id, username, email, password_hash, password_salt, hash_type,
              admin, recovery_authentication_enabled, external_authentication_uid,
              display_name, first_name, middle_name, last_name,
              last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET
                 authz_id = excluded.authz_id,
                 username = excluded.username,
                 email = excluded.email,
                 password_hash = excluded.password_hash,
                 password_salt = excluded.password_salt,
                 hash_type = excluded.hash_type,
                 admin = excluded.admin,
                 recovery_authentication_enabled = excluded.recovery_authentication_enabled,
                 external_authentication_uid = excluded.external_authentication_uid,
                 display_name = excluded.display_name,
                 first_name = excluded.first_name,
                 middle_name = excluded.middle_name,
                 last_name = excluded.last_name,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.authz_id,
                row.username,
                row.email,
                row.password_hash,
                row.password_salt,
                row.hash_type,
                row.admin as i64,
                row.recovery_authentication_enabled as i64,
                row.external_authentication_uid,
                row.display_name,
                row.first_name,
                row.middle_name,
                row.last_name,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.org_user_associations {
        transaction.execute(
            "INSERT INTO org_user_associations
             (org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(org_id, user_id) DO UPDATE SET
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.org_id,
                row.user_id,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.org_user_invites {
        transaction.execute(
            "INSERT INTO org_user_invites
             (id, org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 user_id = excluded.user_id,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.org_id,
                row.user_id,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.clients {
        transaction.execute(
            "INSERT INTO clients
             (id, org_id, authz_id, name, validator, admin, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 authz_id = excluded.authz_id,
                 name = excluded.name,
                 validator = excluded.validator,
                 admin = excluded.admin,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.org_id,
                row.authz_id,
                row.name,
                row.validator as i64,
                row.admin as i64,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.actor_keys {
        transaction.execute(
            "INSERT INTO actor_keys
             (actor_id, actor_type, key_name, public_key, key_version, expires_at,
              last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(actor_id, key_name) DO UPDATE SET
                 actor_type = excluded.actor_type,
                 public_key = excluded.public_key,
                 key_version = excluded.key_version,
                 expires_at = excluded.expires_at,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.actor_id,
                row.actor_type,
                row.key_name,
                row.public_key,
                row.key_version,
                row.expires_at,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.authz_entities {
        transaction.execute(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(authz_id) DO UPDATE SET
                 entity_type = excluded.entity_type,
                 org_id = excluded.org_id,
                 created_at = excluded.created_at",
            params![row.authz_id, row.entity_type, row.org_id, row.created_at],
        )?;
    }

    for row in &snapshot.authz_acl_actor {
        transaction.execute(
            "INSERT OR IGNORE INTO authz_acl_actor
             (target_authz_id, permission, actor_authz_id)
             VALUES (?1, ?2, ?3)",
            params![row.target_authz_id, row.permission, row.actor_authz_id],
        )?;
    }

    for row in &snapshot.authz_acl_group {
        transaction.execute(
            "INSERT OR IGNORE INTO authz_acl_group
             (target_authz_id, permission, group_authz_id)
             VALUES (?1, ?2, ?3)",
            params![row.target_authz_id, row.permission, row.group_authz_id],
        )?;
    }

    for row in &snapshot.authz_group_actors {
        transaction.execute(
            "INSERT OR IGNORE INTO authz_group_actors (group_authz_id, actor_authz_id)
             VALUES (?1, ?2)",
            params![row.group_authz_id, row.actor_authz_id],
        )?;
    }

    for row in &snapshot.authz_group_groups {
        transaction.execute(
            "INSERT OR IGNORE INTO authz_group_groups
             (parent_group_authz_id, child_group_authz_id)
             VALUES (?1, ?2)",
            params![row.parent_group_authz_id, row.child_group_authz_id],
        )?;
    }

    for row in &snapshot.containers {
        transaction.execute(
            "INSERT INTO containers
             (id, org_id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 authz_id = excluded.authz_id,
                 name = excluded.name,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.org_id,
                row.authz_id,
                row.name,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.groups {
        transaction.execute(
            "INSERT INTO groups
             (id, org_id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 authz_id = excluded.authz_id,
                 name = excluded.name,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.org_id,
                row.authz_id,
                row.name,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.global_groups {
        transaction.execute(
            "INSERT INTO global_groups
             (id, authz_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 authz_id = excluded.authz_id,
                 name = excluded.name,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.authz_id,
                row.name,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.nodes {
        transaction.execute(
            "INSERT INTO nodes
             (id, authz_id, org_id, name, environment, policy_name, policy_group,
              serialized_object, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                 authz_id = excluded.authz_id,
                 org_id = excluded.org_id,
                 name = excluded.name,
                 environment = excluded.environment,
                 policy_name = excluded.policy_name,
                 policy_group = excluded.policy_group,
                 serialized_object = excluded.serialized_object,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.authz_id,
                row.org_id,
                row.name,
                row.environment,
                row.policy_name,
                row.policy_group,
                encode_serialized_optional(row.serialized_object.as_deref(), compression)?,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.roles {
        upsert_named_object(transaction, "roles", row, compression)?;
    }
    for row in &snapshot.environments {
        upsert_named_object(transaction, "environments", row, compression)?;
    }
    for row in &snapshot.data_bags {
        upsert_named_object(transaction, "data_bags", row, compression)?;
    }

    for row in &snapshot.data_bag_items {
        transaction.execute(
            "INSERT INTO data_bag_items
             (id, org_id, data_bag_name, item_name, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 data_bag_name = excluded.data_bag_name,
                 item_name = excluded.item_name,
                 serialized_object = excluded.serialized_object,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.org_id,
                row.data_bag_name,
                row.item_name,
                encode_serialized_optional(row.serialized_object.as_deref(), compression)?,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.checksums {
        transaction.execute(
            "INSERT INTO checksums (org_id, checksum, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(org_id, checksum) DO UPDATE SET created_at = excluded.created_at",
            params![row.org_id, row.checksum, row.created_at],
        )?;
    }

    for row in &snapshot.sandboxed_checksums {
        transaction.execute(
            "INSERT INTO sandboxed_checksums (org_id, sandbox_id, checksum, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(sandbox_id, org_id, checksum)
             DO UPDATE SET created_at = excluded.created_at",
            params![row.org_id, row.sandbox_id, row.checksum, row.created_at],
        )?;
    }

    // Backfill explicit sandboxes and SQLite's commit-path checksum rows for
    // pre-sandbox snapshots and older crawler output.
    transaction.execute(
        "INSERT OR IGNORE INTO sandboxes (org_id, sandbox_id, created_at)
         SELECT org_id, sandbox_id, MIN(created_at)
         FROM sandboxed_checksums
         GROUP BY org_id, sandbox_id",
        [],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO sandbox_checksums (org_id, sandbox_id, checksum, created_at)
         SELECT org_id, sandbox_id, checksum, created_at
         FROM sandboxed_checksums",
        [],
    )?;
    for row in &snapshot.sandboxes {
        transaction.execute(
            "INSERT INTO sandboxes (org_id, sandbox_id, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(org_id, sandbox_id)
             DO UPDATE SET created_at = excluded.created_at",
            params![row.org_id, row.sandbox_id, row.created_at],
        )?;
    }
    for row in &snapshot.sandbox_checksums {
        transaction.execute(
            "INSERT INTO sandbox_checksums (org_id, sandbox_id, checksum, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(sandbox_id, org_id, checksum)
             DO UPDATE SET created_at = excluded.created_at",
            params![row.org_id, row.sandbox_id, row.checksum, row.created_at],
        )?;
    }

    for row in &snapshot.cookbooks {
        transaction.execute(
            "INSERT INTO cookbooks (id, org_id, name, authz_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 name = excluded.name,
                 authz_id = excluded.authz_id",
            params![row.id, row.org_id, row.name, row.authz_id],
        )?;
    }

    for row in &snapshot.cookbook_versions {
        transaction.execute(
            "INSERT INTO cookbook_versions
             (id, cookbook_id, org_id, name, major, minor, patch, frozen,
              meta_attributes, meta_deps, meta_long_desc, metadata, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(id) DO UPDATE SET
                 cookbook_id = excluded.cookbook_id,
                 org_id = excluded.org_id,
                 name = excluded.name,
                 major = excluded.major,
                 minor = excluded.minor,
                 patch = excluded.patch,
                 frozen = excluded.frozen,
                 meta_attributes = excluded.meta_attributes,
                 meta_deps = excluded.meta_deps,
                 meta_long_desc = excluded.meta_long_desc,
                 metadata = excluded.metadata,
                 serialized_object = excluded.serialized_object,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.cookbook_id,
                row.org_id,
                row.name,
                row.major,
                row.minor,
                row.patch,
                row.frozen as i64,
                row.meta_attributes,
                row.meta_deps,
                row.meta_long_desc,
                row.metadata,
                encode_serialized_optional(row.serialized_object.as_deref(), compression)?,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
        // This table has no primary key, so replace the version's checksum set
        // rather than appending on every import.
        transaction.execute(
            "DELETE FROM cookbook_version_checksums WHERE cookbook_version_id = ?1",
            [&row.id],
        )?;
    }
    for row in &snapshot.cookbook_version_checksums {
        transaction.execute(
            "INSERT INTO cookbook_version_checksums
             (cookbook_version_id, org_id, checksum)
             VALUES (?1, ?2, ?3)",
            params![row.cookbook_version_id, row.org_id, row.checksum],
        )?;
    }

    for row in &snapshot.cookbook_artifacts {
        transaction.execute(
            "INSERT INTO cookbook_artifacts (id, org_id, name, authz_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 name = excluded.name,
                 authz_id = excluded.authz_id",
            params![row.id, row.org_id, row.name, row.authz_id],
        )?;
    }

    for row in &snapshot.cookbook_artifact_versions {
        transaction.execute(
            "INSERT INTO cookbook_artifact_versions
             (id, cookbook_artifact_id, org_id, identifier, metadata,
              serialized_object, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 cookbook_artifact_id = excluded.cookbook_artifact_id,
                 org_id = excluded.org_id,
                 identifier = excluded.identifier,
                 metadata = excluded.metadata,
                 serialized_object = excluded.serialized_object,
                 created_at = excluded.created_at,
                 created_by = excluded.created_by",
            params![
                row.id,
                row.cookbook_artifact_id,
                row.org_id,
                row.identifier,
                row.metadata,
                encode_serialized_optional(row.serialized_object.as_deref(), compression)?,
                row.created_at,
                row.created_by
            ],
        )?;
        transaction.execute(
            "DELETE FROM cookbook_artifact_version_checksums
             WHERE cookbook_artifact_version_id = ?1",
            [row.id],
        )?;
    }
    for row in &snapshot.cookbook_artifact_version_checksums {
        transaction.execute(
            "INSERT INTO cookbook_artifact_version_checksums
             (cookbook_artifact_version_id, org_id, checksum)
             VALUES (?1, ?2, ?3)",
            params![row.cookbook_artifact_version_id, row.org_id, row.checksum],
        )?;
    }

    for row in &snapshot.policies {
        transaction.execute(
            "INSERT INTO policies
             (id, org_id, name, authz_id, last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 name = excluded.name,
                 authz_id = excluded.authz_id,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.org_id,
                row.name,
                row.authz_id,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.policy_groups {
        transaction.execute(
            "INSERT INTO policy_groups
             (id, org_id, name, authz_id, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 name = excluded.name,
                 authz_id = excluded.authz_id,
                 serialized_object = excluded.serialized_object,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.org_id,
                row.name,
                row.authz_id,
                encode_serialized_optional(row.serialized_object.as_deref(), compression)?,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.policy_revisions {
        transaction.execute(
            "INSERT INTO policy_revisions
             (id, org_id, policy_id, revision_id, name, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 policy_id = excluded.policy_id,
                 revision_id = excluded.revision_id,
                 name = excluded.name,
                 serialized_object = excluded.serialized_object,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.org_id,
                row.policy_id,
                row.revision_id,
                row.name,
                encode_serialized_optional(row.serialized_object.as_deref(), compression)?,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.policy_group_revision_associations {
        transaction.execute(
            "INSERT INTO policy_group_revision_associations
             (id, org_id, policy_revision_id, policy_group_authz_id,
              policy_revision_name, policy_revision_revision_id, policy_group_name,
              last_updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                 org_id = excluded.org_id,
                 policy_revision_id = excluded.policy_revision_id,
                 policy_group_authz_id = excluded.policy_group_authz_id,
                 policy_revision_name = excluded.policy_revision_name,
                 policy_revision_revision_id = excluded.policy_revision_revision_id,
                 policy_group_name = excluded.policy_group_name,
                 last_updated_by = excluded.last_updated_by,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.org_id,
                row.policy_revision_id,
                row.policy_group_authz_id,
                row.policy_revision_name,
                row.policy_revision_revision_id,
                row.policy_group_name,
                row.last_updated_by,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.oauth_applications {
        transaction.execute(
            "INSERT INTO oauth_applications
             (id, name, uid, secret, redirect_uri, scopes, confidential, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 uid = excluded.uid,
                 secret = CASE
                     WHEN excluded.secret = '' THEN oauth_applications.secret
                     ELSE excluded.secret
                 END,
                 redirect_uri = excluded.redirect_uri,
                 scopes = excluded.scopes,
                 confidential = excluded.confidential,
                 created_at = excluded.created_at,
                 updated_at = excluded.updated_at",
            params![
                row.id,
                row.name,
                row.uid,
                row.secret,
                row.redirect_uri,
                row.scopes,
                row.confidential as i64,
                row.created_at,
                row.updated_at
            ],
        )?;
    }

    for row in &snapshot.oauth_access_grants {
        transaction.execute(
            "INSERT INTO oauth_access_grants
             (id, resource_owner_id, application_id, token, expires_in,
              redirect_uri, scopes, created_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                 resource_owner_id = excluded.resource_owner_id,
                 application_id = excluded.application_id,
                 token = excluded.token,
                 expires_in = excluded.expires_in,
                 redirect_uri = excluded.redirect_uri,
                 scopes = excluded.scopes,
                 created_at = excluded.created_at,
                 revoked_at = excluded.revoked_at",
            params![
                row.id,
                row.resource_owner_id,
                row.application_id,
                row.token,
                row.expires_in,
                row.redirect_uri,
                row.scopes,
                row.created_at,
                row.revoked_at
            ],
        )?;
    }

    for row in &snapshot.oauth_access_tokens {
        transaction.execute(
            "INSERT INTO oauth_access_tokens
             (id, resource_owner_id, application_id, token, refresh_token,
              expires_in, scopes, created_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                 resource_owner_id = excluded.resource_owner_id,
                 application_id = excluded.application_id,
                 token = excluded.token,
                 refresh_token = excluded.refresh_token,
                 expires_in = excluded.expires_in,
                 scopes = excluded.scopes,
                 created_at = excluded.created_at,
                 revoked_at = excluded.revoked_at",
            params![
                row.id,
                row.resource_owner_id,
                row.application_id,
                row.token,
                row.refresh_token,
                row.expires_in,
                row.scopes,
                row.created_at,
                row.revoked_at
            ],
        )?;
    }

    for row in &snapshot.kvpair {
        transaction.execute(
            "INSERT INTO kvpair (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value,
                 updated_at = excluded.updated_at",
            params![row.key, row.value, row.updated_at],
        )?;
    }

    // Telemetry is an append-only event log with no primary key. Replace it
    // when the snapshot carries rows so importing the same snapshot twice
    // stays idempotent. An absent/empty collection leaves target rows alone,
    // matching the merge semantics of every other collection.
    if !snapshot.telemetry.is_empty() {
        transaction.execute("DELETE FROM telemetry", [])?;
        for row in &snapshot.telemetry {
            transaction.execute(
                "INSERT INTO telemetry (property, event_timestamp) VALUES (?1, ?2)",
                params![row.property, row.event_timestamp],
            )?;
        }
    }

    Ok(())
}

fn upsert_named_object(
    transaction: &Transaction<'_>,
    table: &str,
    row: &NamedObjectRow,
    compression: Compression,
) -> Result<(), StoreError> {
    let sql = format!(
        "INSERT INTO {table}
         (id, authz_id, org_id, name, serialized_object, last_updated_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
             authz_id = excluded.authz_id,
             org_id = excluded.org_id,
             name = excluded.name,
             serialized_object = excluded.serialized_object,
             last_updated_by = excluded.last_updated_by,
             created_at = excluded.created_at,
             updated_at = excluded.updated_at"
    );
    transaction.execute(
        &sql,
        params![
            row.id,
            row.authz_id,
            row.org_id,
            row.name,
            encode_serialized_optional(row.serialized_object.as_deref(), compression)?,
            row.last_updated_by,
            row.created_at,
            row.updated_at
        ],
    )?;
    Ok(())
}
