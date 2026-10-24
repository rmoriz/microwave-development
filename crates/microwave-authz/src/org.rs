use microwave_store::{
    AuthzRepository, Container, Group, IdentityRepository, ObjectRepository, Org, Permission,
    StoreError,
};

pub const DEFAULT_CONTAINERS: [&str; 12] = [
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

pub const DEFAULT_GROUPS: [&str; 5] = [
    "admins",
    "billing-admins",
    "clients",
    "users",
    "public_key_read_access",
];

/// Create an organization and apply the default container/group/ACL policy.
/// The organization row is removed again if any policy step fails.
pub async fn create_organization<S>(
    store: &S,
    name: &str,
    full_name: &str,
    creator_user_id: &str,
    creator_authz_id: &str,
) -> Result<Org, StoreError>
where
    S: IdentityRepository + AuthzRepository + ObjectRepository + ?Sized,
{
    let org = store.create_org(name, full_name).await?;

    if let Err(error) = apply_default_policy(store, &org, creator_user_id, creator_authz_id).await {
        let _ = store.delete_org(&org.id).await;
        return Err(error);
    }

    Ok(org)
}

async fn apply_default_policy<S>(
    store: &S,
    org: &Org,
    creator_user_id: &str,
    creator_authz_id: &str,
) -> Result<(), StoreError>
where
    S: IdentityRepository + AuthzRepository + ObjectRepository + ?Sized,
{
    let mut containers = Vec::with_capacity(DEFAULT_CONTAINERS.len());
    for name in DEFAULT_CONTAINERS {
        containers.push(store.create_container_for_org(&org.id, name).await?);
    }

    let mut groups = Vec::with_capacity(DEFAULT_GROUPS.len());
    for name in DEFAULT_GROUPS {
        groups.push(store.create_group_for_org(&org.id, name).await?);
    }

    // Upstream creates a global `<org>_read_access_group` and makes the org's
    // admins and users groups members of it. It is rendered as
    // `::<org>_read_access_group` in ACL responses.
    let read_access_group = store
        .ensure_global_group(&format!("{}_read_access_group", org.name))
        .await?;
    // Keep the global server-admins group available even on servers where the
    // superuser was created by an older bootstrap path.
    store.ensure_global_group("server-admins").await?;

    store.associate_user_org(&org.id, creator_user_id).await?;

    let admins = group(&groups, "admins")?;
    let users = group(&groups, "users")?;
    let clients = group(&groups, "clients")?;
    let public_key_read_access = group(&groups, "public_key_read_access")?;

    store
        .add_group_member(&admins.authz_id, creator_authz_id)
        .await?;
    store
        .add_group_member(&users.authz_id, creator_authz_id)
        .await?;
    store
        .add_group_child(&read_access_group.authz_id, &admins.authz_id)
        .await?;
    store
        .add_group_child(&read_access_group.authz_id, &users.authz_id)
        .await?;
    store
        .add_group_child(&public_key_read_access.authz_id, &users.authz_id)
        .await?;
    store
        .add_group_child(&public_key_read_access.authz_id, &clients.authz_id)
        .await?;

    // The creator gets every permission on every object created by the policy.
    for container in &containers {
        grant(
            store,
            &container.authz_id,
            &Permission::ALL,
            Some(creator_authz_id),
            None,
        )
        .await?;
        grant(
            store,
            &container.authz_id,
            &Permission::ALL,
            None,
            Some(&admins.authz_id),
        )
        .await?;
    }
    for group in &groups {
        grant(
            store,
            &group.authz_id,
            &Permission::ALL,
            Some(creator_authz_id),
            None,
        )
        .await?;
    }
    grant(
        store,
        &read_access_group.authz_id,
        &Permission::ALL,
        Some(creator_authz_id),
        None,
    )
    .await?;
    for name in ["admins", "clients", "users"] {
        grant(
            store,
            &group(&groups, name)?.authz_id,
            &Permission::ALL,
            None,
            Some(&admins.authz_id),
        )
        .await?;
    }
    grant(
        store,
        &public_key_read_access.authz_id,
        &[Permission::Read, Permission::Update],
        None,
        Some(&admins.authz_id),
    )
    .await?;
    grant(
        store,
        &org.authz_id,
        &Permission::ALL,
        Some(creator_authz_id),
        None,
    )
    .await?;
    grant(
        store,
        &org.authz_id,
        &Permission::ALL,
        None,
        Some(&admins.authz_id),
    )
    .await?;

    // Normal users can manage cookbooks, data, nodes, roles, environments and policies.
    let users_manage = [
        "cookbooks",
        "data",
        "nodes",
        "roles",
        "environments",
        "policies",
        "policy_groups",
        "cookbook_artifacts",
    ];
    for name in users_manage {
        grant(
            store,
            &container(&containers, name)?.authz_id,
            &[
                Permission::Create,
                Permission::Read,
                Permission::Update,
                Permission::Delete,
            ],
            None,
            Some(&users.authz_id),
        )
        .await?;
    }
    grant(
        store,
        &container(&containers, "clients")?.authz_id,
        &[Permission::Read, Permission::Delete],
        None,
        Some(&users.authz_id),
    )
    .await?;
    grant(
        store,
        &container(&containers, "sandboxes")?.authz_id,
        &[Permission::Create],
        None,
        Some(&users.authz_id),
    )
    .await?;
    for name in ["groups", "containers"] {
        grant(
            store,
            &container(&containers, name)?.authz_id,
            &[Permission::Read],
            None,
            Some(&users.authz_id),
        )
        .await?;
    }
    grant(
        store,
        &org.authz_id,
        &[Permission::Read],
        None,
        Some(&users.authz_id),
    )
    .await?;

    // Clients may update nodes and read the objects needed for a converge.
    grant(
        store,
        &container(&containers, "nodes")?.authz_id,
        &[Permission::Read, Permission::Create],
        None,
        Some(&clients.authz_id),
    )
    .await?;
    for name in [
        "policies",
        "policy_groups",
        "cookbook_artifacts",
        "data",
        "cookbooks",
        "environments",
        "roles",
    ] {
        grant(
            store,
            &container(&containers, name)?.authz_id,
            &[Permission::Read],
            None,
            Some(&clients.authz_id),
        )
        .await?;
    }

    // Billing admins can read and update the billing-admins group.
    let billing_admins = group(&groups, "billing-admins")?;
    grant(
        store,
        &billing_admins.authz_id,
        &[Permission::Read, Permission::Update],
        Some(creator_authz_id),
        None,
    )
    .await?;
    grant(
        store,
        &billing_admins.authz_id,
        &[Permission::Read, Permission::Update],
        None,
        Some(&billing_admins.authz_id),
    )
    .await?;

    // Create the default environment only after every container ACL has been
    // applied. Object ACLs are copied from their container at creation time,
    // so creating it earlier leaves clients without the read grant they need
    // for a converge.
    let default_environment = store.create_environment(
        &org.id,
        "_default",
        r#"{"name":"_default","description":"The default Chef environment","json_class":"Chef::Environment","chef_type":"environment","cookbook_versions":{},"default_attributes":{},"override_attributes":{}}"#,
        creator_authz_id,
    ).await?;
    grant(
        store,
        &default_environment.authz_id,
        &Permission::ALL,
        None,
        Some(&admins.authz_id),
    )
    .await?;
    grant(
        store,
        &default_environment.authz_id,
        &[
            Permission::Create,
            Permission::Read,
            Permission::Update,
            Permission::Delete,
        ],
        None,
        Some(&users.authz_id),
    )
    .await?;
    grant(
        store,
        &default_environment.authz_id,
        &[Permission::Read],
        None,
        Some(&clients.authz_id),
    )
    .await?;

    Ok(())
}

async fn grant<S>(
    store: &S,
    target_authz_id: &str,
    permissions: &[Permission],
    actor_authz_id: Option<&str>,
    group_authz_id: Option<&str>,
) -> Result<(), StoreError>
where
    S: AuthzRepository + ?Sized,
{
    for permission in permissions {
        store
            .set_acl(target_authz_id, *permission, actor_authz_id, group_authz_id)
            .await?;
    }
    Ok(())
}

fn container<'a>(containers: &'a [Container], name: &str) -> Result<&'a Container, StoreError> {
    containers
        .iter()
        .find(|container| container.name == name)
        .ok_or_else(|| StoreError::NotFound(format!("container {name}")))
}

fn group<'a>(groups: &'a [Group], name: &str) -> Result<&'a Group, StoreError> {
    groups
        .iter()
        .find(|group| group.name == name)
        .ok_or_else(|| StoreError::NotFound(format!("group {name}")))
}
