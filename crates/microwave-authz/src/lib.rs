//! Authorization and ACL semantics for Microwave Server.

use microwave_store::{SqliteStore, StoreError};

pub use microwave_store::Permission;

/// Check whether an actor has a permission on a target.
pub fn check(
    store: &SqliteStore,
    actor_authz_id: &str,
    target_authz_id: &str,
    permission: Permission,
) -> Result<bool, StoreError> {
    store.check_permission(actor_authz_id, target_authz_id, permission)
}

/// Reject a group edge that would create a cycle.
pub fn ensure_acyclic(
    store: &SqliteStore,
    parent_group_authz_id: &str,
    child_group_authz_id: &str,
) -> Result<(), StoreError> {
    store.ensure_acyclic(parent_group_authz_id, child_group_authz_id)
}
