//! Async forwarding implementations of the backend-neutral contract for the
//! SQLite store.
//!
//! Every method delegates to the existing synchronous inherent method on
//! [`SqliteStore`]. The synchronous methods stay available for callers that
//! have not been switched to the trait yet, so this module is purely additive.

use crate::artifacts::{StoredCookbookArtifact, StoredCookbookArtifactVersion};
use crate::authz::{Acl, AclEntry, Container, Group, GroupMembers, Permission};
use crate::backend::{
    ArtifactRepository, AuthzRepository, CookbookRepository, IdentityRepository, KvRepository,
    MembershipRepository, OAuthRepository, ObjectRepository, PolicyRepository, SandboxRepository,
    SecretsRepository,
};
use crate::cookbooks::StoredCookbookVersion;
use crate::identity::{ActorKey, ActorType, Client, Org, Principal, User, UserPatch, UserProfile};
use crate::membership::Invitation;
use crate::oauth::{OAuthAccessGrant, OAuthAccessToken, OAuthApplication};
use crate::objects::{NewNode, StoredDataBagItem, StoredNode, StoredObject};
use crate::policies::{
    StoredPolicy, StoredPolicyGroup, StoredPolicyGroupAssociation, StoredPolicyRevision,
};
use crate::sandboxes::SandboxCheck;
use crate::sqlite::{SqliteStore, StoreError};
use async_trait::async_trait;

#[async_trait]
impl IdentityRepository for SqliteStore {
    async fn create_org(&self, name: &str, full_name: &str) -> Result<Org, StoreError> {
        self.create_org(name, full_name)
    }

    async fn update_org(&self, name: &str, full_name: &str) -> Result<Org, StoreError> {
        self.update_org(name, full_name)
    }

    async fn find_org(&self, name: &str) -> Result<Option<Org>, StoreError> {
        self.find_org(name)
    }

    async fn find_org_by_id(&self, id: &str) -> Result<Option<Org>, StoreError> {
        self.find_org_by_id(id)
    }

    async fn list_orgs(&self) -> Result<Vec<Org>, StoreError> {
        self.list_orgs()
    }

    async fn create_user(
        &self,
        username: &str,
        email: &str,
        hash_type: &str,
        password_hash: &str,
        password_salt: &str,
        admin: bool,
    ) -> Result<User, StoreError> {
        self.create_user(
            username,
            email,
            hash_type,
            password_hash,
            password_salt,
            admin,
        )
    }

    async fn create_user_with_profile(
        &self,
        username: &str,
        email: Option<&str>,
        hash_type: &str,
        password_hash: &str,
        password_salt: &str,
        admin: bool,
        profile: &UserProfile,
    ) -> Result<User, StoreError> {
        self.create_user_with_profile(
            username,
            email,
            hash_type,
            password_hash,
            password_salt,
            admin,
            profile,
        )
    }

    async fn find_user(&self, username: &str) -> Result<Option<User>, StoreError> {
        self.find_user(username)
    }

    async fn find_user_by_id(&self, id: &str) -> Result<Option<User>, StoreError> {
        self.find_user_by_id(id)
    }

    async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        self.list_users()
    }

    async fn list_users_filtered(
        &self,
        email: Option<&str>,
        external_authentication_uid: Option<&str>,
    ) -> Result<Vec<User>, StoreError> {
        self.list_users_filtered(email, external_authentication_uid)
    }

    async fn update_user(
        &self,
        username: &str,
        email: Option<&str>,
        password: Option<(&str, &str, &str)>,
        admin: Option<bool>,
    ) -> Result<User, StoreError> {
        self.update_user(username, email, password, admin)
    }

    async fn update_user_patch(
        &self,
        username: &str,
        patch: &UserPatch,
    ) -> Result<User, StoreError> {
        self.update_user_patch(username, patch)
    }

    async fn users_share_org(
        &self,
        left_user_id: &str,
        right_user_id: &str,
    ) -> Result<bool, StoreError> {
        self.users_share_org(left_user_id, right_user_id)
    }

    async fn set_recovery_authentication_enabled(
        &self,
        username: &str,
        enabled: bool,
    ) -> Result<(), StoreError> {
        self.set_recovery_authentication_enabled(username, enabled)
    }

    async fn delete_user(&self, username: &str) -> Result<(), StoreError> {
        self.delete_user(username)
    }

    async fn create_client(
        &self,
        org_id: &str,
        name: &str,
        validator: bool,
        admin: bool,
    ) -> Result<Client, StoreError> {
        self.create_client(org_id, name, validator, admin)
    }

    async fn create_client_with_acl(
        &self,
        org_id: &str,
        name: &str,
        validator: bool,
        admin: bool,
        creator_authz_id: Option<&str>,
    ) -> Result<Client, StoreError> {
        self.create_client_with_acl(org_id, name, validator, admin, creator_authz_id)
    }

    async fn update_client(
        &self,
        org_id: &str,
        name: &str,
        new_name: &str,
        validator: Option<bool>,
    ) -> Result<Client, StoreError> {
        self.update_client(org_id, name, new_name, validator)
    }

    async fn find_client(&self, org_id: &str, name: &str) -> Result<Option<Client>, StoreError> {
        self.find_client(org_id, name)
    }

    async fn list_clients(&self, org_id: &str) -> Result<Vec<Client>, StoreError> {
        self.list_clients(org_id)
    }

    async fn delete_client(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_client(org_id, name)
    }

    async fn add_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        public_key: &str,
    ) -> Result<(), StoreError> {
        self.add_actor_key(actor_id, actor_type, key_name, public_key)
    }

    async fn update_or_create_default_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        public_key: &str,
    ) -> Result<ActorKey, StoreError> {
        self.update_or_create_default_key(actor_id, actor_type, public_key)
    }

    async fn create_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        public_key: &str,
        expires_at: &str,
    ) -> Result<ActorKey, StoreError> {
        self.create_actor_key(actor_id, actor_type, key_name, public_key, expires_at)
    }

    async fn find_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
    ) -> Result<Option<ActorKey>, StoreError> {
        self.find_actor_key(actor_id, actor_type, key_name)
    }

    async fn update_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
        new_key_name: Option<&str>,
        public_key: Option<&str>,
        expires_at: Option<&str>,
    ) -> Result<ActorKey, StoreError> {
        self.update_actor_key(
            actor_id,
            actor_type,
            key_name,
            new_key_name,
            public_key,
            expires_at,
        )
    }

    async fn delete_actor_key(
        &self,
        actor_id: &str,
        actor_type: ActorType,
        key_name: &str,
    ) -> Result<(), StoreError> {
        self.delete_actor_key(actor_id, actor_type, key_name)
    }

    async fn find_actor_keys(
        &self,
        actor_id: &str,
        actor_type: ActorType,
    ) -> Result<Vec<ActorKey>, StoreError> {
        self.find_actor_keys(actor_id, actor_type)
    }

    async fn list_actor_keys(
        &self,
        actor_id: &str,
        actor_type: ActorType,
    ) -> Result<Vec<ActorKey>, StoreError> {
        self.list_actor_keys(actor_id, actor_type)
    }

    async fn find_principals(
        &self,
        org_id: Option<&str>,
        name: &str,
    ) -> Result<Vec<Principal>, StoreError> {
        self.find_principals(org_id, name)
    }

    async fn find_principal_keys(
        &self,
        principal: &Principal,
    ) -> Result<Vec<ActorKey>, StoreError> {
        self.find_principal_keys(principal)
    }
}

#[async_trait]
impl AuthzRepository for SqliteStore {
    async fn create_actor(&self) -> Result<String, StoreError> {
        self.create_actor()
    }

    async fn create_object(&self) -> Result<String, StoreError> {
        self.create_object()
    }

    async fn create_group(&self, name: &str) -> Result<String, StoreError> {
        self.create_group(name)
    }

    async fn create_container(&self) -> Result<String, StoreError> {
        self.create_container()
    }

    async fn create_container_for_org(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Container, StoreError> {
        self.create_container_for_org(org_id, name)
    }

    async fn create_container_for_org_with_acl(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<Container, StoreError> {
        self.create_container_for_org_with_acl(org_id, name, creator_authz_id)
    }

    async fn create_group_for_org(&self, org_id: &str, name: &str) -> Result<Group, StoreError> {
        self.create_group_for_org(org_id, name)
    }

    async fn create_group_for_org_with_acl(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<Group, StoreError> {
        self.create_group_for_org_with_acl(org_id, name, creator_authz_id)
    }

    async fn find_group(&self, org_id: &str, name: &str) -> Result<Option<Group>, StoreError> {
        self.find_group(org_id, name)
    }

    async fn find_container(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<Container>, StoreError> {
        self.find_container(org_id, name)
    }

    async fn list_groups(&self, org_id: &str) -> Result<Vec<Group>, StoreError> {
        self.list_groups(org_id)
    }

    async fn list_containers(&self, org_id: &str) -> Result<Vec<Container>, StoreError> {
        self.list_containers(org_id)
    }

    async fn rename_group(
        &self,
        org_id: &str,
        name: &str,
        new_name: &str,
    ) -> Result<Group, StoreError> {
        self.rename_group(org_id, name, new_name)
    }

    async fn delete_group(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_group(org_id, name)
    }

    async fn delete_container(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_container(org_id, name)
    }

    async fn list_group_members(
        &self,
        org_id: &str,
        group_authz_id: &str,
    ) -> Result<GroupMembers, StoreError> {
        self.list_group_members(org_id, group_authz_id)
    }

    async fn replace_group_members(
        &self,
        org_id: &str,
        group_authz_id: &str,
        users: Option<&[String]>,
        clients: Option<&[String]>,
        groups: Option<&[String]>,
    ) -> Result<(), StoreError> {
        self.replace_group_members(org_id, group_authz_id, users, clients, groups)
    }

    async fn add_group_member(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError> {
        self.add_group_member(group_authz_id, actor_authz_id)
    }

    async fn remove_group_member(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError> {
        self.remove_group_member(group_authz_id, actor_authz_id)
    }

    async fn add_group_child(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError> {
        self.add_group_child(parent_group_authz_id, child_group_authz_id)
    }

    async fn ensure_acyclic(
        &self,
        parent_group_authz_id: &str,
        child_group_authz_id: &str,
    ) -> Result<(), StoreError> {
        self.ensure_acyclic(parent_group_authz_id, child_group_authz_id)
    }

    async fn set_acl(
        &self,
        target_authz_id: &str,
        permission: Permission,
        actor_authz_id: Option<&str>,
        group_authz_id: Option<&str>,
    ) -> Result<(), StoreError> {
        self.set_acl(target_authz_id, permission, actor_authz_id, group_authz_id)
    }

    async fn get_acl(&self, target_authz_id: &str) -> Result<Acl, StoreError> {
        self.get_acl(target_authz_id)
    }

    async fn get_acl_entries(&self, target_authz_id: &str) -> Result<Vec<AclEntry>, StoreError> {
        self.get_acl_entries(target_authz_id)
    }

    async fn set_acl_for_permission(
        &self,
        target_authz_id: &str,
        permission: Permission,
        actors: &[String],
        groups: &[String],
    ) -> Result<(), StoreError> {
        self.set_acl_for_permission(target_authz_id, permission, actors, groups)
    }

    async fn set_acl_for_permission_split(
        &self,
        target_authz_id: &str,
        permission: Permission,
        users: &[String],
        clients: &[String],
        groups: &[String],
    ) -> Result<(), StoreError> {
        self.set_acl_for_permission_split(target_authz_id, permission, users, clients, groups)
    }

    async fn grant_actor_all(
        &self,
        target_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<(), StoreError> {
        self.grant_actor_all(target_authz_id, actor_authz_id)
    }

    async fn ensure_global_group(&self, name: &str) -> Result<Group, StoreError> {
        self.ensure_global_group(name)
    }

    async fn find_global_group(&self, name: &str) -> Result<Option<Group>, StoreError> {
        self.find_global_group(name)
    }

    async fn check_permission(
        &self,
        actor_authz_id: &str,
        target_authz_id: &str,
        permission: Permission,
    ) -> Result<bool, StoreError> {
        self.check_permission(actor_authz_id, target_authz_id, permission)
    }

    async fn find_acl_target(
        &self,
        org_id: &str,
        object_type: &str,
        name: &str,
    ) -> Result<Option<String>, StoreError> {
        self.find_acl_target(org_id, object_type, name)
    }

    async fn associate_user_org(&self, org_id: &str, user_id: &str) -> Result<(), StoreError> {
        self.associate_user_org(org_id, user_id)
    }

    async fn delete_org(&self, org_id: &str) -> Result<(), StoreError> {
        self.delete_org(org_id)
    }
}

#[async_trait]
impl ObjectRepository for SqliteStore {
    async fn create_node(&self, org_id: &str, node: NewNode<'_>) -> Result<StoredNode, StoreError> {
        self.create_node(org_id, node)
    }

    async fn find_node(&self, org_id: &str, name: &str) -> Result<Option<StoredNode>, StoreError> {
        self.find_node(org_id, name)
    }

    async fn list_nodes(&self, org_id: &str) -> Result<Vec<StoredNode>, StoreError> {
        self.list_nodes(org_id)
    }

    async fn count_nodes(&self) -> Result<u64, StoreError> {
        self.count_nodes()
    }

    async fn list_nodes_in_environment(
        &self,
        org_id: &str,
        environment: &str,
    ) -> Result<Vec<StoredNode>, StoreError> {
        self.list_nodes_in_environment(org_id, environment)
    }

    async fn update_node(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        environment: &str,
        policy_name: Option<&str>,
        policy_group: Option<&str>,
    ) -> Result<StoredNode, StoreError> {
        self.update_node(org_id, name, body, environment, policy_name, policy_group)
    }

    async fn delete_node(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_node(org_id, name)
    }

    async fn create_environment(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        self.create_environment(org_id, name, body, creator_authz_id)
    }

    async fn find_environment(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        self.find_environment(org_id, name)
    }

    async fn list_environments(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError> {
        self.list_environments(org_id)
    }

    async fn update_environment(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError> {
        self.update_environment(org_id, name, body)
    }

    async fn delete_environment(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_environment(org_id, name)
    }

    async fn create_role(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        self.create_role(org_id, name, body, creator_authz_id)
    }

    async fn find_role(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        self.find_role(org_id, name)
    }

    async fn list_roles(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError> {
        self.list_roles(org_id)
    }

    async fn update_role(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
    ) -> Result<StoredObject, StoreError> {
        self.update_role(org_id, name, body)
    }

    async fn delete_role(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_role(org_id, name)
    }

    async fn create_data_bag(
        &self,
        org_id: &str,
        name: &str,
        creator_authz_id: &str,
    ) -> Result<StoredObject, StoreError> {
        self.create_data_bag(org_id, name, creator_authz_id)
    }

    async fn find_data_bag(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredObject>, StoreError> {
        self.find_data_bag(org_id, name)
    }

    async fn list_data_bags(&self, org_id: &str) -> Result<Vec<StoredObject>, StoreError> {
        self.list_data_bags(org_id)
    }

    async fn delete_data_bag(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_data_bag(org_id, name)
    }

    async fn create_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
        body: &str,
    ) -> Result<StoredDataBagItem, StoreError> {
        self.create_data_bag_item(org_id, data_bag_name, item_name, body)
    }

    async fn find_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
    ) -> Result<Option<StoredDataBagItem>, StoreError> {
        self.find_data_bag_item(org_id, data_bag_name, item_name)
    }

    async fn list_data_bag_items(
        &self,
        org_id: &str,
        data_bag_name: &str,
    ) -> Result<Vec<StoredDataBagItem>, StoreError> {
        self.list_data_bag_items(org_id, data_bag_name)
    }

    async fn update_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
        body: &str,
    ) -> Result<StoredDataBagItem, StoreError> {
        self.update_data_bag_item(org_id, data_bag_name, item_name, body)
    }

    async fn delete_data_bag_item(
        &self,
        org_id: &str,
        data_bag_name: &str,
        item_name: &str,
    ) -> Result<(), StoreError> {
        self.delete_data_bag_item(org_id, data_bag_name, item_name)
    }
}

#[async_trait]
impl SandboxRepository for SqliteStore {
    async fn create_sandbox(
        &self,
        org_id: &str,
        checksums: &[String],
    ) -> Result<(String, Vec<SandboxCheck>), StoreError> {
        self.create_sandbox(org_id, checksums)
    }

    async fn sandbox_checksums(
        &self,
        org_id: &str,
        sandbox_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        self.sandbox_checksums(org_id, sandbox_id)
    }

    async fn sandbox_exists(&self, org_id: &str, sandbox_id: &str) -> Result<bool, StoreError> {
        self.sandbox_exists(org_id, sandbox_id)
    }

    async fn sandbox_created_at(
        &self,
        org_id: &str,
        sandbox_id: &str,
    ) -> Result<Option<String>, StoreError> {
        self.sandbox_created_at(org_id, sandbox_id)
    }

    async fn commit_sandbox(&self, org_id: &str, sandbox_id: &str) -> Result<(), StoreError> {
        self.commit_sandbox(org_id, sandbox_id)
    }

    async fn checksum_exists(&self, org_id: &str, checksum: &str) -> Result<bool, StoreError> {
        self.checksum_exists(org_id, checksum)
    }

    async fn find_checksum(
        &self,
        org_id: &str,
        checksum: &str,
    ) -> Result<Option<String>, StoreError> {
        self.find_checksum(org_id, checksum)
    }
}

#[async_trait]
impl CookbookRepository for SqliteStore {
    async fn create_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
        manifest: &str,
        checksums: &[String],
        frozen: bool,
        creator_authz_id: &str,
    ) -> Result<StoredCookbookVersion, StoreError> {
        self.create_cookbook_version(
            org_id,
            name,
            version,
            manifest,
            checksums,
            frozen,
            creator_authz_id,
        )
    }

    async fn find_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<StoredCookbookVersion>, StoreError> {
        self.find_cookbook_version(org_id, name, version)
    }

    async fn list_cookbook_versions(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredCookbookVersion>, StoreError> {
        self.list_cookbook_versions(org_id)
    }

    async fn update_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
        manifest: &str,
        checksums: &[String],
        frozen: bool,
        updater_authz_id: &str,
    ) -> Result<(StoredCookbookVersion, Vec<String>), StoreError> {
        self.update_cookbook_version(
            org_id,
            name,
            version,
            manifest,
            checksums,
            frozen,
            updater_authz_id,
        )
    }

    async fn delete_cookbook_version(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<(StoredCookbookVersion, Vec<String>), StoreError> {
        self.delete_cookbook_version(org_id, name, version)
    }
}

#[async_trait]
impl ArtifactRepository for SqliteStore {
    async fn list_cookbook_artifacts(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredCookbookArtifact>, StoreError> {
        self.list_cookbook_artifacts(org_id)
    }

    async fn find_cookbook_artifact(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredCookbookArtifact>, StoreError> {
        self.find_cookbook_artifact(org_id, name)
    }

    async fn find_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
    ) -> Result<Option<StoredCookbookArtifactVersion>, StoreError> {
        self.find_cookbook_artifact_version(org_id, name, identifier)
    }

    async fn create_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
        manifest: &str,
        checksums: &[String],
        creator_authz_id: &str,
    ) -> Result<StoredCookbookArtifactVersion, StoreError> {
        self.create_cookbook_artifact_version(
            org_id,
            name,
            identifier,
            manifest,
            checksums,
            creator_authz_id,
        )
    }

    async fn delete_cookbook_artifact_version(
        &self,
        org_id: &str,
        name: &str,
        identifier: &str,
    ) -> Result<(StoredCookbookArtifactVersion, Vec<String>), StoreError> {
        self.delete_cookbook_artifact_version(org_id, name, identifier)
    }
}

#[async_trait]
impl PolicyRepository for SqliteStore {
    async fn create_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredPolicyRevision, StoreError> {
        self.create_policy_revision(org_id, name, revision_id, body, creator_authz_id)
    }

    async fn find_policy(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredPolicy>, StoreError> {
        self.find_policy(org_id, name)
    }

    async fn list_policies(&self, org_id: &str) -> Result<Vec<StoredPolicy>, StoreError> {
        self.list_policies(org_id)
    }

    async fn find_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
    ) -> Result<Option<StoredPolicyRevision>, StoreError> {
        self.find_policy_revision(org_id, name, revision_id)
    }

    async fn list_policy_revisions(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError> {
        self.list_policy_revisions(org_id, name)
    }

    async fn list_all_policy_revisions(
        &self,
        org_id: &str,
    ) -> Result<Vec<StoredPolicyRevision>, StoreError> {
        self.list_all_policy_revisions(org_id)
    }

    async fn delete_policy(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_policy(org_id, name)
    }

    async fn delete_policy_revision(
        &self,
        org_id: &str,
        name: &str,
        revision_id: &str,
    ) -> Result<(), StoreError> {
        self.delete_policy_revision(org_id, name, revision_id)
    }

    async fn create_policy_group(
        &self,
        org_id: &str,
        name: &str,
        body: &str,
        creator_authz_id: &str,
    ) -> Result<StoredPolicyGroup, StoreError> {
        self.create_policy_group(org_id, name, body, creator_authz_id)
    }

    async fn find_policy_group(
        &self,
        org_id: &str,
        name: &str,
    ) -> Result<Option<StoredPolicyGroup>, StoreError> {
        self.find_policy_group(org_id, name)
    }

    async fn list_policy_groups(&self, org_id: &str) -> Result<Vec<StoredPolicyGroup>, StoreError> {
        self.list_policy_groups(org_id)
    }

    async fn delete_policy_group(&self, org_id: &str, name: &str) -> Result<(), StoreError> {
        self.delete_policy_group(org_id, name)
    }

    async fn associate_policy_revision(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
        revision_id: &str,
        body: &str,
        group_body: &str,
        creator_authz_id: &str,
    ) -> Result<(StoredPolicyGroupAssociation, bool), StoreError> {
        self.associate_policy_revision(
            org_id,
            group_name,
            policy_name,
            revision_id,
            body,
            group_body,
            creator_authz_id,
        )
    }

    async fn find_policy_group_association(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
    ) -> Result<Option<StoredPolicyGroupAssociation>, StoreError> {
        self.find_policy_group_association(org_id, group_name, policy_name)
    }

    async fn list_policy_group_associations(
        &self,
        org_id: &str,
        group_name: &str,
    ) -> Result<Vec<StoredPolicyGroupAssociation>, StoreError> {
        self.list_policy_group_associations(org_id, group_name)
    }

    async fn list_policy_groups_for_revision(
        &self,
        org_id: &str,
        policy_name: &str,
        revision_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        self.list_policy_groups_for_revision(org_id, policy_name, revision_id)
    }

    async fn delete_policy_group_association(
        &self,
        org_id: &str,
        group_name: &str,
        policy_name: &str,
    ) -> Result<(), StoreError> {
        self.delete_policy_group_association(org_id, group_name, policy_name)
    }
}

#[async_trait]
impl MembershipRepository for SqliteStore {
    async fn list_org_users(&self, org_id: &str) -> Result<Vec<User>, StoreError> {
        self.list_org_users(org_id)
    }

    async fn is_user_in_org(&self, org_id: &str, user_id: &str) -> Result<bool, StoreError> {
        self.is_user_in_org(org_id, user_id)
    }

    async fn add_user_to_org(&self, org_id: &str, user_id: &str) -> Result<(), StoreError> {
        self.add_user_to_org(org_id, user_id)
    }

    async fn remove_user_from_org(
        &self,
        org_id: &str,
        user_id: &str,
        username: &str,
    ) -> Result<(), StoreError> {
        self.remove_user_from_org(org_id, user_id, username)
    }

    async fn list_user_orgs(&self, user_id: &str) -> Result<Vec<Org>, StoreError> {
        self.list_user_orgs(user_id)
    }

    async fn user_in_group(
        &self,
        group_authz_id: &str,
        actor_authz_id: &str,
    ) -> Result<bool, StoreError> {
        self.user_in_group(group_authz_id, actor_authz_id)
    }

    async fn is_org_admin_for_user(
        &self,
        actor_authz_id: &str,
        user_id: &str,
    ) -> Result<bool, StoreError> {
        self.is_org_admin_for_user(actor_authz_id, user_id)
    }

    async fn create_invitation(
        &self,
        org_id: &str,
        user_id: &str,
        last_updated_by: &str,
    ) -> Result<Invitation, StoreError> {
        self.create_invitation(org_id, user_id, last_updated_by)
    }

    async fn find_invitation(&self, id: &str) -> Result<Option<Invitation>, StoreError> {
        self.find_invitation(id)
    }

    async fn list_org_invitations(&self, org_id: &str) -> Result<Vec<Invitation>, StoreError> {
        self.list_org_invitations(org_id)
    }

    async fn list_user_invitations(&self, user_id: &str) -> Result<Vec<Invitation>, StoreError> {
        self.list_user_invitations(user_id)
    }

    async fn count_user_invitations(&self, user_id: &str) -> Result<usize, StoreError> {
        self.count_user_invitations(user_id)
    }

    async fn delete_invitation(&self, id: &str) -> Result<(), StoreError> {
        self.delete_invitation(id)
    }

    async fn accept_invitation(&self, id: &str) -> Result<Invitation, StoreError> {
        self.accept_invitation(id)
    }

    async fn reject_invitation(&self, id: &str) -> Result<Invitation, StoreError> {
        self.reject_invitation(id)
    }
}

#[async_trait]
impl OAuthRepository for SqliteStore {
    async fn create_oauth_application(
        &self,
        name: &str,
        redirect_uri: &str,
        scopes: &str,
        confidential: bool,
    ) -> Result<OAuthApplication, StoreError> {
        self.create_oauth_application(name, redirect_uri, scopes, confidential)
    }

    async fn find_oauth_application(
        &self,
        id: i64,
    ) -> Result<Option<OAuthApplication>, StoreError> {
        self.find_oauth_application(id)
    }

    async fn find_oauth_application_by_uid(
        &self,
        uid: &str,
    ) -> Result<Option<OAuthApplication>, StoreError> {
        self.find_oauth_application_by_uid(uid)
    }

    async fn list_oauth_applications(&self) -> Result<Vec<OAuthApplication>, StoreError> {
        self.list_oauth_applications()
    }

    async fn update_oauth_application(
        &self,
        id: i64,
        name: Option<&str>,
        redirect_uri: Option<&str>,
    ) -> Result<OAuthApplication, StoreError> {
        self.update_oauth_application(id, name, redirect_uri)
    }

    async fn delete_oauth_application(&self, id: i64) -> Result<(), StoreError> {
        self.delete_oauth_application(id)
    }

    async fn create_oauth_access_grant(
        &self,
        resource_owner_id: &str,
        application_id: i64,
        redirect_uri: &str,
        scopes: &str,
        expires_in: i64,
    ) -> Result<OAuthAccessGrant, StoreError> {
        self.create_oauth_access_grant(
            resource_owner_id,
            application_id,
            redirect_uri,
            scopes,
            expires_in,
        )
    }

    async fn find_oauth_access_grant(
        &self,
        token: &str,
    ) -> Result<Option<OAuthAccessGrant>, StoreError> {
        self.find_oauth_access_grant(token)
    }

    async fn revoke_oauth_access_grant(&self, token: &str) -> Result<(), StoreError> {
        self.revoke_oauth_access_grant(token)
    }

    async fn create_oauth_access_token(
        &self,
        resource_owner_id: &str,
        application_id: i64,
        scopes: &str,
        expires_in: Option<i64>,
        include_refresh_token: bool,
    ) -> Result<OAuthAccessToken, StoreError> {
        self.create_oauth_access_token(
            resource_owner_id,
            application_id,
            scopes,
            expires_in,
            include_refresh_token,
        )
    }

    async fn find_oauth_access_token(
        &self,
        token: &str,
    ) -> Result<Option<OAuthAccessToken>, StoreError> {
        self.find_oauth_access_token(token)
    }

    async fn find_oauth_access_token_by_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<Option<OAuthAccessToken>, StoreError> {
        self.find_oauth_access_token_by_refresh(refresh_token)
    }

    async fn revoke_oauth_access_token(&self, token: &str) -> Result<(), StoreError> {
        self.revoke_oauth_access_token(token)
    }

    async fn revoke_oauth_access_token_by_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<(), StoreError> {
        self.revoke_oauth_access_token_by_refresh(refresh_token)
    }

    async fn create_id_session(&self, username: &str) -> Result<String, StoreError> {
        self.create_id_session(username)
    }

    async fn find_id_session(&self, token: &str) -> Result<Option<String>, StoreError> {
        self.find_id_session(token)
    }

    async fn delete_id_session(&self, token: &str) -> Result<(), StoreError> {
        self.delete_id_session(token)
    }

    async fn create_password_reset_token(&self, username: &str) -> Result<String, StoreError> {
        self.create_password_reset_token(username)
    }

    async fn find_password_reset_token(&self, token: &str) -> Result<Option<String>, StoreError> {
        self.find_password_reset_token(token)
    }

    async fn consume_password_reset_token(
        &self,
        token: &str,
    ) -> Result<Option<String>, StoreError> {
        self.consume_password_reset_token(token)
    }
}

#[async_trait]
impl KvRepository for SqliteStore {
    async fn get_kv(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.get_kv(key)
    }

    async fn set_kv(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.set_kv(key, value)
    }

    async fn remove_kv(&self, key: &str) -> Result<bool, StoreError> {
        self.remove_kv(key)
    }

    async fn maintenance_enabled(&self) -> Result<bool, StoreError> {
        self.maintenance_enabled()
    }

    async fn set_maintenance_enabled(&self, enabled: bool) -> Result<(), StoreError> {
        self.set_maintenance_enabled(enabled)
    }
}

#[async_trait]
impl SecretsRepository for SqliteStore {
    async fn get_secret(&self, group: &str, name: &str) -> Result<Option<String>, StoreError> {
        self.get_secret(group, name)
    }

    async fn set_secret(&self, group: &str, name: &str, value: &str) -> Result<(), StoreError> {
        self.set_secret(group, name, value)
    }

    async fn remove_secret(&self, group: &str, name: &str) -> Result<(), StoreError> {
        self.remove_secret(group, name)
    }
}
