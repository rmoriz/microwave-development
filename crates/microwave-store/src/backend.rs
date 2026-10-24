//! Backend-neutral metadata store contract.
//!
//! `AppState` holds `Arc<dyn MetadataStore>`, so the same API handlers, OAuth
//! surface, embedded search, authorization policy, and bootstrap run against
//! SQLite or PostgreSQL. The contract is split into one repository trait per
//! domain:
//!
//! * [`IdentityRepository`] — orgs, users, clients, actor keys, principals.
//! * [`AuthzRepository`] — authz entities, groups, containers, ACLs and the
//!   recursive permission check.
//! * [`ObjectRepository`] — nodes, roles, environments, data bags and items.
//! * [`SandboxRepository`] — sandbox uploads and checksum commits.
//! * [`CookbookRepository`] — cookbook versions and their checksums.
//! * [`ArtifactRepository`] — cookbook artifacts and their versions.
//! * [`PolicyRepository`] — policyfiles, revisions and group associations.
//! * [`MembershipRepository`] — org memberships and invitations.
//! * [`OAuthRepository`] — OAuth applications, grants, tokens, sessions and
//!   password-reset tokens.
//! * [`KvRepository`] — the raw key/value table and maintenance flag.
//! * [`SecretsRepository`] — operator-managed secrets stored in the KV table.
//!
//! [`MetadataStore`] is a blanket supertrait, so a backend only implements the
//! repository traits. Code that is generic over one domain accepts
//! `S: SomeRepository + ?Sized` so it also works with `&dyn MetadataStore`.
//!
//! The legacy migration helpers are still intentionally **not** part of this
//! contract; see the `microwave-store-postgres` README for the migration
//! checklist.

use crate::artifacts::{StoredCookbookArtifact, StoredCookbookArtifactVersion};
use crate::authz::{Acl, AclEntry, Container, Group, GroupMembers, Permission};
use crate::cookbooks::StoredCookbookVersion;
use crate::identity::{ActorKey, ActorType, Client, Org, Principal, User, UserPatch, UserProfile};
use crate::membership::Invitation;
use crate::oauth::{OAuthAccessGrant, OAuthAccessToken, OAuthApplication};
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
    async fn find_org_by_id(&self, id: &str) -> Result<Option<Org>, StoreError>;
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

    async fn set_acl_for_permission_split(
        &self,
        target_authz_id: &str,
        permission: Permission,
        users: &[String],
        clients: &[String],
        groups: &[String],
    ) -> Result<(), StoreError>;

    async fn grant_actor_all(
        &self,
        target_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError>;

    async fn ensure_global_group(&self, name: &str) -> Result<Group, StoreError>;
    async fn find_global_group(&self, name: &str) -> Result<Option<Group>, StoreError>;

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
    async fn count_nodes(&self) -> Result<u64, StoreError>;
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
    #[allow(clippy::too_many_arguments)]
    async fn create_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
        manifest: &str,
        checksums: &[String],
        frozen: bool,
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

    #[allow(clippy::too_many_arguments)]
    async fn update_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
        manifest: &str,
        checksums: &[String],
        frozen: bool,
        updater_authz_id: &str,
    ) -> Result<(StoredCookbookVersion, Vec<String>), StoreError>;

    async fn delete_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<(StoredCookbookVersion, Vec<String>), StoreError>;
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

/// Cookbook artifacts and their versions.
#[async_trait]
pub trait ArtifactRepository: Send + Sync {
    async fn list_cookbook_artifacts(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredCookbookArtifact>, StoreError>;

    async fn find_cookbook_artifact(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredCookbookArtifact>, StoreError>;

    async fn find_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
    ) -> Result<Option<StoredCookbookArtifactVersion>, StoreError>;

    async fn create_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
        manifest: &str,
        checksums: &[String],
        creator_authz_id: &str,
    ) -> Result<StoredCookbookArtifactVersion, StoreError>;

    async fn delete_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
    ) -> Result<(StoredCookbookArtifactVersion, Vec<String>), StoreError>;
}

/// Organization memberships and invitations.
#[async_trait]
pub trait MembershipRepository: Send + Sync {
    async fn list_org_users(&self, org_id: &str) -> Result<Vec<User>, StoreError>;
    async fn is_user_in_org(&self, org_id: &str, user_id: &str) -> Result<bool, StoreError>;
    async fn add_user_to_org(&self, org_id: &str, user_id: &str) -> Result<(), StoreError>;
    async fn remove_user_from_org(
        &self,
        org_id: &str,
        user_id: &str,
        username: &str,
    ) -> Result<(), StoreError>;
    async fn list_user_orgs(&self, user_id: &str) -> Result<Vec<Org>, StoreError>;
    async fn user_in_group(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<bool, StoreError>;
    async fn is_org_admin_for_user(
        &self,
        actor_authz_id: &str,
        user_id: &str,
    ) -> Result<bool, StoreError>;

    async fn create_invitation(
        &self,
        org_id: &str,
        user_id: &str,
        last_updated_by: &str,
    ) -> Result<Invitation, StoreError>;
    async fn find_invitation(&self, id: &str) -> Result<Option<Invitation>, StoreError>;
    async fn list_org_invitations(&self, org_id: &str) -> Result<Vec<Invitation>, StoreError>;
    async fn list_user_invitations(&self, user_id: &str) -> Result<Vec<Invitation>, StoreError>;
    async fn count_user_invitations(&self, user_id: &str) -> Result<usize, StoreError>;
    async fn delete_invitation(&self, id: &str) -> Result<(), StoreError>;
    async fn accept_invitation(&self, id: &str) -> Result<Invitation, StoreError>;
    async fn reject_invitation(&self, id: &str) -> Result<Invitation, StoreError>;
}

/// OAuth applications, grants, tokens, browser sessions and password resets.
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait OAuthRepository: Send + Sync {
    async fn create_oauth_application(
        &self,
        name: &str,
        redirect_uri: &str,
        scopes: &str,
        confidential: bool,
    ) -> Result<OAuthApplication, StoreError>;
    async fn find_oauth_application(&self, id: i64)
        -> Result<Option<OAuthApplication>, StoreError>;
    async fn find_oauth_application_by_uid(
        &self,
        uid: &str,
    ) -> Result<Option<OAuthApplication>, StoreError>;
    async fn list_oauth_applications(&self) -> Result<Vec<OAuthApplication>, StoreError>;
    async fn update_oauth_application(
        &self,
        id: i64,
        name: Option<&str>,
        redirect_uri: Option<&str>,
    ) -> Result<OAuthApplication, StoreError>;
    async fn delete_oauth_application(&self, id: i64) -> Result<(), StoreError>;

    async fn create_oauth_access_grant(
        &self,
        resource_owner_id: &str,
        application_id: i64,
        redirect_uri: &str,
        scopes: &str,
        expires_in: i64,
    ) -> Result<OAuthAccessGrant, StoreError>;
    async fn find_oauth_access_grant(
        &self,
        token: &str,
    ) -> Result<Option<OAuthAccessGrant>, StoreError>;
    async fn revoke_oauth_access_grant(&self, token: &str) -> Result<(), StoreError>;

    async fn create_oauth_access_token(
        &self,
        resource_owner_id: &str,
        application_id: i64,
        scopes: &str,
        expires_in: Option<i64>,
        include_refresh_token: bool,
    ) -> Result<OAuthAccessToken, StoreError>;
    async fn find_oauth_access_token(
        &self,
        token: &str,
    ) -> Result<Option<OAuthAccessToken>, StoreError>;
    async fn find_oauth_access_token_by_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<Option<OAuthAccessToken>, StoreError>;
    async fn revoke_oauth_access_token(&self, token: &str) -> Result<(), StoreError>;
    async fn revoke_oauth_access_token_by_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<(), StoreError>;

    async fn create_id_session(&self, username: &str) -> Result<String, StoreError>;
    async fn find_id_session(&self, token: &str) -> Result<Option<String>, StoreError>;
    async fn delete_id_session(&self, token: &str) -> Result<(), StoreError>;

    async fn create_password_reset_token(&self, username: &str) -> Result<String, StoreError>;
    async fn find_password_reset_token(&self, token: &str) -> Result<Option<String>, StoreError>;
    async fn consume_password_reset_token(&self, token: &str)
        -> Result<Option<String>, StoreError>;
}

/// Raw key/value storage plus the operator maintenance flag.
#[async_trait]
pub trait KvRepository: Send + Sync {
    async fn get_kv(&self, key: &str) -> Result<Option<String>, StoreError>;
    async fn set_kv(&self, key: &str, value: &str) -> Result<(), StoreError>;
    async fn remove_kv(&self, key: &str) -> Result<bool, StoreError>;
    async fn maintenance_enabled(&self) -> Result<bool, StoreError>;
    async fn set_maintenance_enabled(&self, enabled: bool) -> Result<(), StoreError>;
}

/// Operator-managed secrets stored under namespaced KV keys.
#[async_trait]
pub trait SecretsRepository: Send + Sync {
    async fn get_secret(&self, group: &str, name: &str) -> Result<Option<String>, StoreError>;
    async fn set_secret(&self, group: &str, name: &str, value: &str) -> Result<(), StoreError>;

    /// Insert a secret only when it does not exist and return the value that is
    /// stored afterwards. This is the HA-safe bootstrap primitive.
    async fn set_secret_if_absent(
        &self,
        group: &str,
        name: &str,
        value: &str,
    ) -> Result<String, StoreError>;

    async fn remove_secret(&self, group: &str, name: &str) -> Result<(), StoreError>;
}

/// The complete metadata store contract.
///
/// The server holds `Arc<dyn MetadataStore>`. The legacy `MigrationRepository`
/// helpers are the only remaining domain outside this contract.
pub trait MetadataStore:
    IdentityRepository
    + AuthzRepository
    + ObjectRepository
    + SandboxRepository
    + CookbookRepository
    + ArtifactRepository
    + PolicyRepository
    + MembershipRepository
    + OAuthRepository
    + KvRepository
    + SecretsRepository
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
        + ArtifactRepository
        + PolicyRepository
        + MembershipRepository
        + OAuthRepository
        + KvRepository
        + SecretsRepository
        + Send
        + Sync
{
}
