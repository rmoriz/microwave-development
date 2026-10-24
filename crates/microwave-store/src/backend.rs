//! Backend-neutral metadata store contract.
//!
//! `AppState` currently holds a concrete [`SqliteStore`](crate::SqliteStore).
//! To run the same API handlers against PostgreSQL, the state must depend on
//! this trait instead of a concrete backend. The contract is split into one
//! repository trait per domain so that the eventual `AppState<S: MetadataStore>`
//! change stays mechanical:
//!
//! * [`IdentityRepository`] — orgs, users, clients, actor keys, principals.
//! * [`AuthzRepository`] — authz entities, groups, containers, ACLs and the
//!   recursive permission check.
//! * [`ObjectRepository`] — nodes, roles, environments, data bags and items.
//! * [`SandboxRepository`] — sandbox uploads and checksum commits.
//! * [`CookbookRepository`] — cookbook versions and their checksums.
//! * [`PolicyRepository`] — policyfiles, revisions and group associations.
//!
//! [`MetadataStore`] is a blanket supertrait, so a backend only implements the
//! six repository traits and `AppState` can be bounded by `S: MetadataStore`.
//!
//! OAuth (`oauth`) and key/value (`kv`), org membership/invites and the legacy
//! migration helpers are intentionally **not** part of this first slice. They
//! must be added as `OAuthRepository`, `KvRepository` and
//! `MembershipRepository` before `AppState` is fully backend-agnostic; see the
//! `microwave-store-postgres` README for the migration checklist.

use crate::authz::{Acl, AclEntry, Container, Group, GroupMembers, Permission};
use crate::cookbooks::StoredCookbookVersion;
use crate::identity::{ActorKey, ActorType, Client, Org, Principal, User, UserPatch, UserProfile};
use crate::objects::{NewNode, StoredDataBagItem, StoredNode, StoredObject};
use crate::policies::{
    StoredPolicy, StoredPolicyGroup, StoredPolicyGroupAssociation, StoredPolicyRevision,
};
use crate::sandboxes::SandboxCheck;
use crate::sqlite::StoreError;
use async_trait::async_trait;

/// Identity and key material.
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait IdentityRepository: Send + Sync {
    async fn create_org(&self, name: &str, full_name: &str) -> Result<Org, StoreError>;
    async fn update_org(&self, name: &str, full_name: &str) -> Result<Org, StoreError>;
    async fn find_org(&self, name: &str) -> Result<Option<Org>, StoreError>;
    async fn list_orgs(&self) -> Result<Vec<Org>, StoreError>;

    async fn create_user(
        &self,
        username: &str,
        email: &str,
        hash_type: &str,
        password_hash: &str,
        password_salt: &str,
        admin: bool,
    ) -> Result<User, StoreError>;

    async fn create_user_with_profile(
        &self,
        username: &str,
        email: Option<&str>,
        hash_type: &str,
        password_hash: &str,
        password_salt: &str,
        admin: bool,
        profile: &UserProfile,
    ) -> Result<User, StoreError>;

    async fn find_user(&self, username: &str) -> Result<Option<User>, StoreError>;
    async fn find_user_by_id(&self, id: &str) -> Result<Option<User>, StoreError>;
    async fn list_users(&self) -> Result<Vec<User>, StoreError>;
    async fn list_users_filtered(
        &self,
        email: Option<&str>,
        external_authentication_uid: Option<&str>,
    ) -> Result<Vec<User>, StoreError>;

    async fn update_user(
        &self,
        username: &str,
        email: Option<&str>,
        password: Option<(&str, &str, &str)>,
        admin: Option<bool>,
    ) -> Result<User, StoreError>;

    async fn update_user_patch(
        &self,
        username: &str,
        patch: &UserPatch,
    ) -> Result<User, StoreError>;

    async fn users_share_org(
        &self,
        left_user_id: &str,
        right_user_id: &str,
    ) -> Result<bool, StoreError>;

    async fn set_recovery_authentication_enabled(
        &self,
        username: &str,
        enabled: bool,
    ) -> Result<(), StoreError>;

    async fn delete_user(&self, username: &str) -> Result<(), StoreError>;

    async fn create_client(
        &self,
        org_id: &str,
        name: &str,
        validator: bool,
        admin: bool,
    ) -> Result<Client, StoreError>;

    async fn create_client_with_acl(
        &self,
        org_id: &str,
        name: &str,
        validator: bool,
        admin: bool,
        creator_authz_id: Option<&str>,
    ) -> Result<Client, StoreError>;

    async fn update_client(
        &self,
        org_id: &str,
        name: &str,
        new_name: &str,
        validator: Option<bool>,
    ) -> Result<Client, StoreError>;

    async fn find_client(&self, org_id: &str, name: &str) -> Result<Option<Client>, StoreError>;
    async fn list_clients(&self, org_id: &str) -> Result<Vec<Client>, StoreError>;
    async fn delete_client(&self, org_id: &str, name: &str) -> Result<(), StoreError>;

    async fn add_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        public_key: &str,
    ) -> Result<(), StoreError>;

    async fn update_or_create_default_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        public_key: &str,
    ) -> Result<ActorKey, StoreError>;

    async fn create_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        public_key: &str,
        expires_at: &str,
    ) -> Result<ActorKey, StoreError>;

    async fn find_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
    ) -> Result<Option<ActorKey>, StoreError>;

    async fn update_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        new_key_name: Option<&str>,
        public_key: Option<&str>,
        expires_at: Option<&str>,
    ) -> Result<ActorKey, StoreError>;

    async fn delete_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
    ) -> Result<(), StoreError>;

    async fn find_actor_keys(
        &self,
        actor_id: &str,
        actor_type: ActorType,
    ) -> Result<Vec<ActorKey>, StoreError>;

    async fn list_actor_keys(
        &self,
        actor_id: &str,
        actor_type: ActorType,
    ) -> Result<Vec<ActorKey>, StoreError>;

    async fn find_principals(
        &self,
        org_id: Option<&str>,
        name: &str,
    ) -> Result<Vec<Principal>, StoreError>;

    async fn find_principal_keys(&self, principal: &Principal)
        -> Result<Vec<ActorKey>, StoreError>;
}

/// Authorization graph and ACL evaluation.
#[async_trait]
pub trait AuthzRepository: Send + Sync {
    async fn create_actor(&self) -> Result<String, StoreError>;
    async fn create_object(&self) -> Result<String, StoreError>;
    async fn create_group(&self, name: &str) -> Result<String, StoreError>;
    async fn create_container(&self) -> Result<String, StoreError>;

    async fn create_container_for_org(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Container, StoreError>;

    async fn create_container_for_org_with_acl(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<Container, StoreError>;

    async fn create_group_for_org(&self, org_id: &str, name: &str) -> Result<Group, StoreError>;

    async fn create_group_for_org_with_acl(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<Group, StoreError>;

    async fn find_group(&self, org_id: &str, name: &str) -> Result<Option<Group>, StoreError>;
    async fn find_container(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<Container>, StoreError>;

    async fn list_groups(&self, org_id: &str) -> Result<Vec<Group>, StoreError>;
    async fn list_containers(&self, org_id: &str) -> Result<Vec<Container>, StoreError>;
    async fn rename_group(
        &self,
        org_id: &str,
        name: &str,
        new_name: &str,
    ) -> Result<Group, StoreError>;
    async fn delete_group(&self, org_id: &str, name: &str) -> Result<(), StoreError>;
    async fn delete_container(&self, org_id: &str, name: &str) -> Result<(), StoreError>;

    async fn list_group_members(
        &self,
        org_id: &str,
        group_authz_id: &str,
    ) -> Result<GroupMembers, StoreError>;

    async fn replace_group_members(
        &self,
        org_id: &str,
        group_authz_id: &str,
        users: Option<&[String]>,
        clients: Option<&[String]>,
        groups: Option<&[String]>,
    ) -> Result<(), StoreError>;

    async fn add_group_member(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError>;

    async fn remove_group_member(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError>;

    async fn add_group_child(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError>;

    async fn ensure_acyclic(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError>;

    async fn set_acl(
        &self,
        target_authz_id: &str,
        permission: Permission,
        actor_authz_id: Option<&str>,
        group_authz_id: Option<&str>,
    ) -> Result<(), StoreError>;

    async fn get_acl(&self, target_authz_id: &str) -> Result<Acl, StoreError>;
    async fn get_acl_entries(&self, target_authz_id: &str) -> Result<Vec<AclEntry>, StoreError>;

    async fn set_acl_for_permission(
        &self,
        target_authz_id: &str,
        permission: Permission,
        actors: &[String],
        groups: &[String],
    ) -> Result<(), StoreError>;

    async fn check_permission(
        &self,
        actor_authz_id: &str,
        target_authz_id: &str,
        permission: Permission,
    ) -> Result<bool, StoreError>;

    async fn find_acl_target(
        &self,
        org_id: &str,
        object_type: &str,
        name: &str,
    ) -> Result<Option<String>, StoreError>;

    async fn associate_user_org(&self, org_id: &str, user_id: &str) -> Result<(), StoreError>;
    async fn delete_org(&self, org_id: &str) -> Result<(), StoreError>;
}

/// Nodes, roles, environments, data bags and data bag items.
#[async_trait]
pub trait ObjectRepository: Send + Sync {
    async fn create_node(&self, org_id: &str, node: NewNode<'_>) -> Result<StoredNode, StoreError>;
    async fn find_node(&self, org_id: &str, name: &str) -> Result<Option<StoredNode>, StoreError>;
    async fn list_nodes(&self, org_id: &str) -> Result<Vec<StoredNode>, StoreError>;
    async fn list_nodes_in_environment(
        &self,
        org_id: &str,
        environment: &str,
    ) -> Result<Vec<StoredNode>, StoreError>;
    async fn update_node(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        environment: &str,
        policy_name: Option<&str>,
        policy_group: Option<&str>,
    ) -> Result<StoredNode, StoreError>;
    async fn delete_node(&self, org_id: &str, name: &str) -> Result<(), StoreError>;

    async fn create_environment(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError>;
    async fn find_environment(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError>;
    async fn list_environments(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError>;
    async fn update_environment(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError>;
    async fn delete_environment(&self, org_id: &str, name: &str) -> Result<(), StoreError>;

    async fn create_role(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError>;
    async fn find_role(&self, org_id: &str, name: &str)
        -> Result<Option<StoredObject>, StoreError>;
    async fn list_roles(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError>;
    async fn update_role(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError>;
    async fn delete_role(&self, org_id: &str, name: &str) -> Result<(), StoreError>;

    async fn create_data_bag(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError>;
    async fn find_data_bag(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError>;
    async fn list_data_bags(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError>;
    async fn delete_data_bag(&self, org_id: &str, name: &str) -> Result<(), StoreError>;

    async fn create_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
        body: &str,
    ) -> Result<StoredDataBagItem, StoreError>;
    async fn find_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
    ) -> Result<Option<StoredDataBagItem>, StoreError>;
    async fn list_data_bag_items(
        &self,
        org_id: &str,
        data_bag_name: &str,
    ) -> Result<Vec<StoredDataBagItem>, StoreError>;
    async fn update_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
        body: &str,
    ) -> Result<StoredDataBagItem, StoreError>;
    async fn delete_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
    ) -> Result<(), StoreError>;
}

/// Sandbox and checksum tracking.
#[async_trait]
pub trait SandboxRepository: Send + Sync {
    async fn create_sandbox(
        &self,
        org_id: &str,
        checksums: &[String],
    ) -> Result<(String, Vec<SandboxCheck>), StoreError>;

    async fn sandbox_checksums(
        &self,
        org_id: &str,
        sandbox_id: &str,
    ) -> Result<Vec<String>, StoreError>;

    async fn sandbox_exists(&self, org_id: &str, sandbox_id: &str) -> Result<bool, StoreError>;
    async fn sandbox_created_at(
        &self,
        org_id: &str,
        sandbox_id: &str,
    ) -> Result<Option<String>, StoreError>;
    async fn commit_sandbox(&self, org_id: &str, sandbox_id: &str) -> Result<(), StoreError>;
    async fn checksum_exists(&self, org_id: &str, checksum: &str) -> Result<bool, StoreError>;
    async fn find_checksum(
        &self,
        org_id: &str,
        checksum: &str,
    ) -> Result<Option<String>, StoreError>;
}

/// Cookbook versions and their checksums.
#[async_trait]
pub trait CookbookRepository: Send + Sync {
    async fn create_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
        manifest: &str,
        checksums: &[String],
        creator_authz_id: &str,
    ) -> Result<StoredCookbookVersion, StoreError>;

    async fn find_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<StoredCookbookVersion>, StoreError>;

    async fn list_cookbook_versions(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredCookbookVersion>, StoreError>;

    async fn delete_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<(), StoreError>;
}

/// Policyfiles, revisions, groups and group/revision associations.
#[async_trait]
pub trait PolicyRepository: Send + Sync {
    async fn create_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredPolicyRevision, StoreError>;

    async fn find_policy(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredPolicy>, StoreError>;
    async fn list_policies(&self, org_id: &str) -> Result<Vec<StoredPolicy>, StoreError>;
    async fn find_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
    ) -> Result<Option<StoredPolicyRevision>, StoreError>;
    async fn list_policy_revisions(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError>;
    async fn list_all_policy_revisions(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError>;
    async fn delete_policy(&self, org_id: &str, name: &str) -> Result<(), StoreError>;
    async fn delete_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
    ) -> Result<(), StoreError>;

    async fn create_policy_group(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredPolicyGroup, StoreError>;
    async fn find_policy_group(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredPolicyGroup>, StoreError>;
    async fn list_policy_groups(&self, org_id: &str) -> Result<Vec<StoredPolicyGroup>, StoreError>;
    async fn delete_policy_group(&self, org_id: &str, name: &str) -> Result<(), StoreError>;

    #[allow(clippy::too_many_arguments)]
    async fn associate_policy_revision(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
        revision_id: &str,
        body: &str,
        group_body: &str,
        creator_authz_id: &str,
    ) -> Result<(StoredPolicyGroupAssociation, bool), StoreError>;

    async fn find_policy_group_association(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
    ) -> Result<Option<StoredPolicyGroupAssociation>, StoreError>;
    async fn list_policy_group_associations(
        &self,
        org_id: &str,
        group_name: &str,
    ) -> Result<Vec<StoredPolicyGroupAssociation>, StoreError>;
    async fn list_policy_groups_for_revision(
        &self,
        org_id: &str,
        policy_name: &str,
        revision_id: &str,
    ) -> Result<Vec<String>, StoreError>;
    async fn delete_policy_group_association(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
    ) -> Result<(), StoreError>;
}

/// The complete metadata store contract for the first PostgreSQL slice.
///
/// `AppState` should eventually hold `Arc<S>` where `S: MetadataStore`. The
/// remaining repository traits (`OAuthRepository`, `KvRepository`,
/// `MembershipRepository`, `MigrationRepository`) are listed in the
/// `microwave-store-postgres` README.
pub trait MetadataStore:
    IdentityRepository
    + AuthzRepository
    + ObjectRepository
    + SandboxRepository
    + CookbookRepository
    + PolicyRepository
    + Send
    + Sync
{
}

impl<T> MetadataStore for T where
    T: IdentityRepository
        + AuthzRepository
        + ObjectRepository
        + SandboxRepository
        + CookbookRepository
        + PolicyRepository
        + Send
        + Sync
{
}
