use microwave_authz::create_organization;
use microwave_migrate::apply_default_policy;
use microwave_store::{OrgRow, SqliteStore, StoreSnapshot};
use std::collections::{BTreeSet, HashMap};

const NOW: &str = "2026-11-02T00:00:00Z";

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Map every authz id in a snapshot to the name of the row that owns it, so
/// random uuids from the live server and synthetic ids from the builder compare
/// equal.
fn name_map(snapshot: &StoreSnapshot, extra: &[(&str, &str)]) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for (authz_id, name) in extra {
        names.insert((*authz_id).to_string(), (*name).to_string());
    }
    for org in &snapshot.orgs {
        names.insert(org.authz_id.clone(), org.name.clone());
    }
    for user in &snapshot.users {
        names.insert(user.authz_id.clone(), user.username.clone());
    }
    for client in &snapshot.clients {
        names.insert(client.authz_id.clone(), client.name.clone());
    }
    for container in &snapshot.containers {
        names.insert(container.authz_id.clone(), container.name.clone());
    }
    for group in &snapshot.groups {
        names.insert(group.authz_id.clone(), group.name.clone());
    }
    for group in &snapshot.global_groups {
        names.insert(group.authz_id.clone(), group.name.clone());
    }
    for node in &snapshot.nodes {
        names.insert(node.authz_id.clone(), node.name.clone());
    }
    for role in &snapshot.roles {
        names.insert(role.authz_id.clone(), role.name.clone());
    }
    for environment in &snapshot.environments {
        names.insert(environment.authz_id.clone(), environment.name.clone());
    }
    for data_bag in &snapshot.data_bags {
        names.insert(data_bag.authz_id.clone(), data_bag.name.clone());
    }
    for cookbook in &snapshot.cookbooks {
        names.insert(cookbook.authz_id.clone(), cookbook.name.clone());
    }
    names
}

fn name(names: &HashMap<String, String>, authz_id: &str) -> String {
    names
        .get(authz_id)
        .cloned()
        .unwrap_or_else(|| authz_id.to_string())
}

/// Every ACE as `(target, permission, kind, member)` with ids replaced by
/// names, sorted so row order does not matter.
fn canonical_grants(
    snapshot: &StoreSnapshot,
    extra: &[(&str, &str)],
) -> BTreeSet<(String, String, &'static str, String)> {
    let names = name_map(snapshot, extra);
    let mut grants = BTreeSet::new();
    for row in &snapshot.authz_acl_actor {
        grants.insert((
            name(&names, &row.target_authz_id),
            row.permission.clone(),
            "actor",
            name(&names, &row.actor_authz_id),
        ));
    }
    for row in &snapshot.authz_acl_group {
        grants.insert((
            name(&names, &row.target_authz_id),
            row.permission.clone(),
            "group",
            name(&names, &row.group_authz_id),
        ));
    }
    grants
}

/// Every group membership as `(group, member)` names, sorted.
fn canonical_memberships(
    snapshot: &StoreSnapshot,
    extra: &[(&str, &str)],
) -> BTreeSet<(String, String)> {
    let names = name_map(snapshot, extra);
    let mut memberships = BTreeSet::new();
    for row in &snapshot.authz_group_actors {
        memberships.insert((
            name(&names, &row.group_authz_id),
            name(&names, &row.actor_authz_id),
        ));
    }
    for row in &snapshot.authz_group_groups {
        memberships.insert((
            name(&names, &row.parent_group_authz_id),
            name(&names, &row.child_group_authz_id),
        ));
    }
    memberships
}

fn created_org() -> (
    tempfile::TempDir,
    SqliteStore,
    microwave_store::User,
    microwave_store::Org,
) {
    let dir = tempdir();
    let store = SqliteStore::open(&dir.path().join("policy.db")).unwrap();
    let creator = store
        .create_user("admin", "ops@example.com", "bcrypt", "", "", true)
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let org = runtime
        .block_on(create_organization(
            &store,
            "acme",
            "Acme",
            &creator.id,
            &creator.authz_id,
            None,
        ))
        .unwrap();
    (dir, store, creator, org)
}

/// The builder's deterministic id for a scope, mirroring the crawl scheme.
fn test_id(scope: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("microwave-migrate/from-api/v1/{scope}").as_bytes());
    hex::encode(&digest[..16])
}

fn empty_org(name: &str, full_name: &str) -> OrgRow {
    OrgRow {
        id: test_id(&format!("org:{name}")),
        authz_id: test_id(&format!("authz:org:{name}")),
        name: name.to_string(),
        full_name: full_name.to_string(),
        created_at: NOW.to_string(),
        updated_at: NOW.to_string(),
        ..Default::default()
    }
}

#[test]
fn builder_matches_create_organization() {
    let (_dir, store, creator, org) = created_org();
    // A second, non-admin user and a client, created the way the server API
    // creates them, so both `create_user` and `create_client_with_acl` branches
    // are pinned, not only the creator.
    let dev = store
        .create_user("dev", "dev@example.com", "bcrypt", "", "", false)
        .unwrap();
    let client = store
        .create_client_with_acl(&org.id, "web1", false, false, Some(&creator.authz_id))
        .unwrap();
    // Validators get the creator grant but no self-service ACL.
    let validator = store
        .create_client_with_acl(&org.id, "validator1", true, false, Some(&creator.authz_id))
        .unwrap();
    let clients_group = store.find_group(&org.id, "clients").unwrap().unwrap();
    for client in [&client, &validator] {
        store
            .add_group_member(&clients_group.authz_id, &client.authz_id)
            .unwrap();
    }
    let client_authz = vec![
        (client.authz_id.clone(), false),
        (validator.authz_id.clone(), true),
    ];

    // The reference is the real server policy, canonicalized from uuids to names.
    let reference_snapshot = store.export_snapshot().unwrap();
    let reference = canonical_grants(&reference_snapshot, &[]);

    let mut built = StoreSnapshot::default();
    built.orgs.push(OrgRow {
        id: org.id.clone(),
        authz_id: org.authz_id.clone(),
        name: "acme".to_string(),
        full_name: "Acme".to_string(),
        created_at: NOW.to_string(),
        updated_at: NOW.to_string(),
        ..Default::default()
    });
    // The crawler imports the users and clients before the policy runs; the
    // builder needs the same rows.
    for user in [&creator, &dev] {
        built.users.push(microwave_store::UserRow {
            id: user.id.clone(),
            authz_id: user.authz_id.clone(),
            username: user.username.clone(),
            email: user.email.clone(),
            hash_type: "bcrypt".to_string(),
            admin: user.admin,
            created_at: NOW.to_string(),
            updated_at: NOW.to_string(),
            ..Default::default()
        });
    }
    for client in [&client, &validator] {
        built.clients.push(microwave_store::ClientRow {
            id: client.id.clone(),
            org_id: org.id.clone(),
            authz_id: client.authz_id.clone(),
            name: client.name.clone(),
            validator: client.validator,
            created_at: NOW.to_string(),
            updated_at: NOW.to_string(),
            ..Default::default()
        });
    }

    let org_row = built.orgs[0].clone();
    apply_default_policy(
        &mut built,
        &org_row,
        &creator.authz_id,
        &client_authz,
        &[],
        NOW,
        true,
    );

    assert_eq!(canonical_grants(&built, &[]), reference);
    assert_eq!(
        canonical_memberships(&built, &[]),
        canonical_memberships(&reference_snapshot, &[])
    );

    // The non-admin user gets exactly the `create_user` template: ALL on
    // itself, create/read/update/delete for `server-admins`, no membership.
    let grants = canonical_grants(&built, &[]);
    let memberships = canonical_memberships(&built, &[]);
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert!(grants.contains(&(
            "dev".to_string(),
            permission.to_string(),
            "actor",
            "dev".to_string()
        )));
    }
    for permission in ["create", "read", "update", "delete"] {
        assert!(grants.contains(&(
            "dev".to_string(),
            permission.to_string(),
            "group",
            "server-admins".to_string()
        )));
    }
    assert!(!memberships.contains(&("server-admins".to_string(), "dev".to_string())));
    assert!(!grants.contains(&(
        "dev".to_string(),
        "grant".to_string(),
        "group",
        "server-admins".to_string()
    )));

    // The client owns itself, inherits the `clients` container ACL and joins
    // the `clients` group.
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert!(grants.contains(&(
            "web1".to_string(),
            permission.to_string(),
            "actor",
            "web1".to_string()
        )));
        assert!(grants.contains(&(
            "web1".to_string(),
            permission.to_string(),
            "actor",
            "admin".to_string()
        )));
    }
    for permission in ["read", "delete"] {
        assert!(grants.contains(&(
            "web1".to_string(),
            permission.to_string(),
            "group",
            "users".to_string()
        )));
    }
    assert!(memberships.contains(&("clients".to_string(), "web1".to_string())));

    // A validator inherits the container ACL and joins the group, but unlike
    // the non-validator client above it does not own itself.
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert!(grants.contains(&(
            "validator1".to_string(),
            permission.to_string(),
            "actor",
            "admin".to_string()
        )));
        assert!(!grants.contains(&(
            "validator1".to_string(),
            permission.to_string(),
            "actor",
            "validator1".to_string()
        )));
    }
    for permission in ["read", "delete"] {
        assert!(grants.contains(&(
            "validator1".to_string(),
            permission.to_string(),
            "group",
            "users".to_string()
        )));
    }
    assert!(memberships.contains(&("clients".to_string(), "validator1".to_string())));
    // Nodes-only ACEs do not leak onto the client.
    assert!(!grants.contains(&(
        "web1".to_string(),
        "create".to_string(),
        "group",
        "users".to_string()
    )));
    assert!(!grants.contains(&(
        "web1".to_string(),
        "read".to_string(),
        "group",
        "clients".to_string()
    )));

    // `associate_user_org` links the creator to the org.
    let associations: BTreeSet<(String, String)> = built
        .org_user_associations
        .iter()
        .map(|row| (row.org_id.clone(), row.user_id.clone()))
        .collect();
    assert_eq!(
        associations,
        BTreeSet::from([(org.id.clone(), creator.id.clone())])
    );
    assert_eq!(built.containers.len(), 12);
    assert_eq!(built.groups.len(), 5);
    assert_eq!(built.environments.len(), 1);
    assert!(built.environments[0].name == "_default");
    // No object rows in this fixture, so no creator object grants yet.
    assert!(built.authz_acl_actor.iter().all(|row| {
        !built
            .containers
            .iter()
            .any(|container| container.authz_id == row.target_authz_id)
            || row.actor_authz_id == creator.authz_id
    }));

    // The container, group and global group names are the upstream defaults,
    // not just the right count of anonymous rows.
    let mut containers: Vec<_> = built
        .containers
        .iter()
        .map(|container| container.name.as_str())
        .collect();
    containers.sort_unstable();
    let mut expected: Vec<_> = microwave_authz::DEFAULT_CONTAINERS.to_vec();
    expected.sort_unstable();
    assert_eq!(containers, expected);

    let mut groups: Vec<_> = built
        .groups
        .iter()
        .map(|group| group.name.as_str())
        .collect();
    groups.sort_unstable();
    let mut expected: Vec<_> = microwave_authz::DEFAULT_GROUPS.to_vec();
    expected.sort_unstable();
    assert_eq!(groups, expected);

    let mut globals: Vec<_> = built
        .global_groups
        .iter()
        .map(|group| group.name.as_str())
        .collect();
    globals.sort_unstable();
    assert_eq!(globals, ["acme_read_access_group", "server-admins"]);

    // Every ACL target has an authz entity, so an import can resolve it.
    for authz_id in built
        .containers
        .iter()
        .map(|row| &row.authz_id)
        .chain(built.groups.iter().map(|row| &row.authz_id))
        .chain(built.global_groups.iter().map(|row| &row.authz_id))
        .chain(built.environments.iter().map(|row| &row.authz_id))
    {
        assert!(
            built
                .authz_entities
                .iter()
                .any(|entity| entity.authz_id == *authz_id),
            "missing authz entity for {authz_id}"
        );
    }
}

#[test]
fn object_acls_copy_the_container_acl_and_add_the_creator() {
    let mut built = StoreSnapshot::default();
    let org = empty_org("acme", "Acme");
    built.orgs.push(org.clone());
    let creator = "creator-authz";
    let node = "node-authz";
    let other_node = "other-node-authz";
    let object_authz = vec![
        ("nodes".to_string(), node.to_string()),
        ("nodes".to_string(), other_node.to_string()),
    ];
    built.users.push(microwave_store::UserRow {
        id: "creator-id".to_string(),
        authz_id: creator.to_string(),
        username: "admin".to_string(),
        admin: true,
        ..Default::default()
    });
    // `client-a` owns the node named like itself; `client-b` owns nothing.
    for (name, authz_id) in [("client-a", node), ("other-node", other_node)] {
        built.nodes.push(microwave_store::NodeRow {
            id: format!("node-id-{name}"),
            authz_id: authz_id.to_string(),
            org_id: org.id.clone(),
            name: name.to_string(),
            environment: "_default".to_string(),
            created_at: NOW.to_string(),
            updated_at: NOW.to_string(),
            ..Default::default()
        });
    }
    for name in ["client-a", "client-b"] {
        built.clients.push(microwave_store::ClientRow {
            id: format!("client-id-{name}"),
            org_id: org.id.clone(),
            authz_id: name.to_string(),
            name: name.to_string(),
            created_at: NOW.to_string(),
            updated_at: NOW.to_string(),
            ..Default::default()
        });
    }

    apply_default_policy(
        &mut built,
        &org,
        creator,
        &[
            ("client-a".to_string(), false),
            ("client-b".to_string(), false),
        ],
        &object_authz,
        NOW,
        true,
    );

    let nodes = built
        .containers
        .iter()
        .find(|container| container.name == "nodes")
        .unwrap()
        .authz_id
        .clone();

    let rows_for = |target: &str| -> BTreeSet<(String, String)> {
        built
            .authz_acl_actor
            .iter()
            .filter(|row| row.target_authz_id == target)
            .map(|row| (row.permission.clone(), row.actor_authz_id.clone()))
            .collect()
    };
    let groups_for = |target: &str| -> BTreeSet<(String, String)> {
        built
            .authz_acl_group
            .iter()
            .filter(|row| row.target_authz_id == target)
            .map(|row| (row.permission.clone(), row.group_authz_id.clone()))
            .collect()
    };

    // The node carries a copy of every `nodes` container ACE.
    let container_actors = rows_for(&nodes);
    assert!(!container_actors.is_empty());
    for (permission, actor) in &container_actors {
        assert!(
            rows_for(node).contains(&(permission.clone(), actor.clone())),
            "node is missing nodes container ACE ({permission}, {actor})"
        );
    }
    assert_eq!(groups_for(node), groups_for(&nodes));

    // ...plus Grant/Read/Update/Delete/Create for the creator, already part of
    // the copied container ACL.
    for permission in ["grant", "read", "update", "delete", "create"] {
        assert!(
            rows_for(node).contains(&(permission.to_string(), creator.to_string())),
            "node is missing creator {permission}"
        );
    }
    assert_eq!(
        rows_for(node)
            .into_iter()
            .filter(|(_, actor)| actor != "client-a")
            .collect::<BTreeSet<_>>(),
        container_actors
    );

    // The node named like a client gets that client's creator grant, so the
    // client can save it during a converge without an extra container ACE.
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert!(
            rows_for(node).contains(&(permission.to_string(), "client-a".to_string())),
            "node is missing its client's {permission}"
        );
    }
    assert!(!rows_for(other_node)
        .iter()
        .any(|(_, actor)| actor == "client-a" || actor == "client-b"));
    // The `nodes` container itself stays at read and create for clients.
    let clients_group = built
        .groups
        .iter()
        .find(|group| group.name == "clients")
        .unwrap()
        .authz_id
        .clone();
    let node_client_grants: BTreeSet<String> = groups_for(&nodes)
        .into_iter()
        .filter(|(_, group)| group == &clients_group)
        .map(|(permission, _)| permission)
        .collect();
    assert_eq!(
        node_client_grants,
        BTreeSet::from(["read".to_string(), "create".to_string()])
    );

    // No other container's ACL leaks onto the object: the `sandboxes` container
    // grants users Create only, the `groups` container grants clients nothing,
    // while `nodes` grants the `clients` group read and create.
    let sandboxes = built
        .containers
        .iter()
        .find(|container| container.name == "sandboxes")
        .unwrap()
        .authz_id
        .clone();
    let groups_container = built
        .containers
        .iter()
        .find(|container| container.name == "groups")
        .unwrap()
        .authz_id
        .clone();
    assert!(groups_for(node).contains(&("read".to_string(), clients_group.clone())));
    assert!(!groups_for(&sandboxes).contains(&("read".to_string(), clients_group.clone())));
    assert!(!groups_for(&groups_container).contains(&("create".to_string(), clients_group.clone())));

    // The object authz entity exists and is an org-scoped object.
    let entity = built
        .authz_entities
        .iter()
        .find(|entity| entity.authz_id == node)
        .expect("object authz entity");
    assert_eq!(entity.entity_type, "object");
    assert_eq!(entity.org_id.as_deref(), Some(org.id.as_str()));

    // Both clients are members of the `clients` group.
    for client in ["client-a", "client-b"] {
        assert!(built
            .authz_group_actors
            .iter()
            .any(|row| { row.group_authz_id == clients_group && row.actor_authz_id == client }));
    }

    // Deterministic ids: the same input builds the same rows twice.
    let mut again = StoreSnapshot::default();
    again.orgs.push(org.clone());
    again.nodes = built.nodes.clone();
    again.clients = built.clients.clone();
    again.users.push(microwave_store::UserRow {
        id: "creator-id".to_string(),
        authz_id: creator.to_string(),
        username: "admin".to_string(),
        admin: true,
        ..Default::default()
    });
    apply_default_policy(
        &mut again,
        &org,
        creator,
        &[
            ("client-a".to_string(), false),
            ("client-b".to_string(), false),
        ],
        &object_authz,
        NOW,
        true,
    );
    assert_eq!(canonical_grants(&again, &[]), canonical_grants(&built, &[]));
}

/// The `data` container is read-only for clients, with or without client data
/// bag writes; only the bag objects gain write ACEs, and this fixture has none.
#[test]
fn data_container_matches_create_organization() {
    let dir = tempdir();
    let store = SqliteStore::open(&dir.path().join("read-only.db")).unwrap();
    let creator = store
        .create_user("admin", "ops@example.com", "bcrypt", "", "", true)
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let org = runtime
        .block_on(microwave_authz::create_organization(
            &store,
            "acme",
            "Acme",
            &creator.id,
            &creator.authz_id,
            None,
        ))
        .unwrap();
    let client = store
        .create_client_with_acl(&org.id, "web1", false, false, Some(&creator.authz_id))
        .unwrap();
    let clients_group = store.find_group(&org.id, "clients").unwrap().unwrap();
    store
        .add_group_member(&clients_group.authz_id, &client.authz_id)
        .unwrap();

    let reference = canonical_grants(&store.export_snapshot().unwrap(), &[]);

    let mut built = StoreSnapshot::default();
    built.orgs.push(empty_org("acme", "Acme"));
    built.users.push(microwave_store::UserRow {
        id: creator.id.clone(),
        authz_id: creator.authz_id.clone(),
        username: "admin".to_string(),
        email: creator.email.clone(),
        hash_type: "bcrypt".to_string(),
        admin: true,
        created_at: NOW.to_string(),
        updated_at: NOW.to_string(),
        ..Default::default()
    });
    built.clients.push(microwave_store::ClientRow {
        id: client.id.clone(),
        org_id: built.orgs[0].id.clone(),
        authz_id: client.authz_id.clone(),
        name: client.name.clone(),
        validator: false,
        created_at: NOW.to_string(),
        updated_at: NOW.to_string(),
        ..Default::default()
    });
    let org_row = built.orgs[0].clone();
    apply_default_policy(
        &mut built,
        &org_row,
        &creator.authz_id,
        &[(client.authz_id.clone(), false)],
        &[],
        NOW,
        false,
    );

    assert_eq!(canonical_grants(&built, &[]), reference);

    // Only `read` on the data container, even with client data bag writes on.
    let data = built
        .containers
        .iter()
        .find(|container| container.name == "data")
        .unwrap()
        .authz_id
        .clone();
    let built_clients = built
        .groups
        .iter()
        .find(|group| group.name == "clients")
        .unwrap()
        .authz_id
        .clone();
    let grants: Vec<String> = built
        .authz_acl_group
        .iter()
        .filter(|row| row.target_authz_id == data && row.group_authz_id == built_clients)
        .map(|row| row.permission.clone())
        .collect();
    assert_eq!(grants, vec!["read".to_string()]);
    assert!(!built
        .authz_acl_actor
        .iter()
        .any(|row| row.target_authz_id == data && row.actor_authz_id == client.authz_id));
}

/// A converge writes bag items, so the importer hands every client write ACEs
/// on every imported bag object - never on the `data` container, whose ACL has
/// to keep the upstream shape.
#[test]
fn client_data_bag_writes_grant_each_bag_object() {
    let build = |writes: bool| {
        let mut built = StoreSnapshot::default();
        let org = empty_org("acme", "Acme");
        built.orgs.push(org.clone());
        let bag = "bag-authz";
        let object_authz = vec![("data".to_string(), bag.to_string())];
        built.users.push(microwave_store::UserRow {
            id: "creator-id".to_string(),
            authz_id: "creator-authz".to_string(),
            username: "admin".to_string(),
            admin: true,
            ..Default::default()
        });
        apply_default_policy(
            &mut built,
            &org,
            "creator-authz",
            &[
                ("client-a".to_string(), false),
                ("client-b".to_string(), true),
            ],
            &object_authz,
            NOW,
            writes,
        );
        built
    };

    let grants = |built: &StoreSnapshot, bag: &str| -> BTreeSet<(String, String)> {
        built
            .authz_acl_actor
            .iter()
            .filter(|row| row.target_authz_id == bag)
            .map(|row| (row.permission.clone(), row.actor_authz_id.clone()))
            .collect()
    };

    let built = build(true);
    for client in ["client-a", "client-b"] {
        for permission in ["create", "update", "delete"] {
            assert!(
                grants(&built, "bag-authz").contains(&(permission.to_string(), client.to_string())),
                "bag is missing {permission} for {client}"
            );
        }
    }
    // The creator grant on the bag is untouched.
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert!(grants(&built, "bag-authz")
            .contains(&(permission.to_string(), "creator-authz".to_string())));
    }

    // Without the flag the clients keep read-only access, which the bag
    // inherits from the container.
    let built = build(false);
    for client in ["client-a", "client-b"] {
        assert!(!grants(&built, "bag-authz")
            .iter()
            .any(|(_, actor)| actor == client));
    }

    // Deterministic either way.
    assert_eq!(
        canonical_grants(&build(true), &[]),
        canonical_grants(&build(true), &[])
    );
}
