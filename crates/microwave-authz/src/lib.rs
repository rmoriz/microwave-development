//! Authorization and ACL semantics for Microwave Server.

use microwave_store::{AuthzRepository, StoreError};

pub mod org;

pub use microwave_store::Permission;
pub use org::{create_organization, DEFAULT_CONTAINERS, DEFAULT_GROUPS};

/// Check whether an actor has a permission on a target.
pub async fn check<S>(
    store: &S,
    actor_authz_id: &str,
    target_authz_id: &str,
    permission: Permission,
) -> Result<bool, StoreError>
where
    S: AuthzRepository + ?Sized,
{
    store
        .check_permission(actor_authz_id, target_authz_id, permission)
        .await
}

/// Reject a group edge that would create a cycle.
pub async fn ensure_acyclic<S>(
    store: &S,
    parent_group_authz_id: &str,
    child_group_authz_id: &str,
) -> Result<(), StoreError>
where
    S: AuthzRepository + ?Sized,
{
    store
        .ensure_acyclic(parent_group_authz_id, child_group_authz_id)
        .await
}
