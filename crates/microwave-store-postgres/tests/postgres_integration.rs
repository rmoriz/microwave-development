//! PostgreSQL integration tests for the first metadata-store slice.
//!
//! Set `MICROWAVE_TEST_POSTGRES_URL` (or run `scripts/run-postgres-tests.sh`)
//! to execute these tests. Without a reachable database every test skips with
//! a clear message so `cargo test --workspace` stays green on developer
//! machines without Docker.

use microwave_store::{
    ActorType, AuthzRepository, CookbookRepository, GroupMembers, IdentityRepository, NewNode,
    ObjectRepository, Permission, PolicyRepository, SandboxRepository, StoreError, UserProfile,
};
use microwave_store_postgres::PostgresStore;

fn test_url() -> Option<String> {
    std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

/// Returns `None` (after printing a skip message) when no test database was
/// configured.
async fn test_store() -> Option<PostgresStore> {
    let Some(url) = test_url() else {
        eprintln!(
            "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; start a disposable \
             PostgreSQL with scripts/run-postgres-tests.sh"
        );
        return None;
    };
    match PostgresStore::connect(&url).await {
        Ok(store) => Some(store),
        Err(error) => {
            eprintln!(
                "SKIP: PostgreSQL at MICROWAVE_TEST_POSTGRES_URL is not reachable ({error}); \
                 start a disposable PostgreSQL with scripts/run-postgres-tests.sh"
            );
            None
        }
    }
}

fn suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

struct Fixture {
    org_id: String,
    creator_authz_id: String,
}

/// Create an organization plus the standard org containers.
async fn fixture(store: &PostgresStore) -> Fixture {
    let org = store
        .create_org(&format!("org-{}", suffix()), "Test Organization")
        .await
        .expect("create org");
    let creator_authz_id = store.create_actor().await.expect("create creator actor");
    for container in [
        "clients",
        "containers",
        "cookbooks",
        "cookbook_artifacts",
        "data",
        "environments",
        "groups",
        "nodes",
        "policies",
        "policy_groups",
        "roles",
        "sandboxes",
    ] {
        store
            .create_container_for_org_with_acl(&org.id, container, &creator_authz_id)
            .await
            .expect("create org container");
    }
    Fixture {
        org_id: org.id,
        creator_authz_id,
    }
}

fn is_32_hex(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[tokio::test]
async fn creates_orgs_users_clients_and_keys() {
    let Some(store) = test_store().await else {
        return;
    };

    let org = store
        .create_org(&format!("org-{}", suffix()), "Identity Org")
        .await
        .expect("create org");
    assert!(is_32_hex(&org.id));
    assert!(is_32_hex(&org.authz_id));
    assert_eq!(
        store.find_org(&org.name).await.expect("find org").as_ref(),
        Some(&org)
    );

    let username = format!("user-{}", suffix());
    let profile = UserProfile {
        display_name: Some("Ada Lovelace".into()),
        first_name: Some("Ada".into()),
        last_name: Some("Lovelace".into()),
        ..Default::default()
    };
    let user = store
        .create_user_with_profile(
            &username,
            Some("ada@example.test"),
            "bcrypt",
            "$2b$12$hash",
            "salt",
            false,
            &profile,
        )
        .await
        .expect("create user");
    assert!(is_32_hex(&user.id));
    assert_eq!(user.display_name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(
        store
            .find_user(&username)
            .await
            .expect("find user")
            .as_ref(),
        Some(&user)
    );
    assert_eq!(
        store
            .find_user_by_id(&user.id)
            .await
            .expect("find user by id")
            .as_ref(),
        Some(&user)
    );
    assert_eq!(store.list_users().await.expect("list users").len(), 1);

    // Emails are unique case-insensitively.
    let duplicate = store
        .create_user(
            &format!("other-{}", suffix()),
            "ADA@example.test",
            "bcrypt",
            "$2b$12$other",
            "salt",
            false,
        )
        .await;
    assert!(matches!(duplicate, Err(StoreError::Conflict(_))));

    let client = store
        .create_client(&org.id, &format!("client-{}", suffix()), true, false)
        .await
        .expect("create client");
    assert!(is_32_hex(&client.authz_id));
    assert_eq!(
        store
            .find_client(&org.id, &client.name)
            .await
            .expect("find client")
            .as_ref(),
        Some(&client)
    );

    let key = store
        .create_actor_key(
            &user.id,
            ActorType::User,
            "default",
            "-----BEGIN PUBLIC KEY-----\nuser\n-----END PUBLIC KEY-----",
            "infinity",
        )
        .await
        .expect("create user key");
    assert!(!key.expired);
    assert_eq!(
        store
            .find_actor_key(&user.id, ActorType::User, "default")
            .await
            .expect("find key")
            .as_ref(),
        Some(&key)
    );

    let rotated_user_key = store
        .update_or_create_default_key(
            &user.id,
            ActorType::User,
            "-----BEGIN PUBLIC KEY-----\nrotated user\n-----END PUBLIC KEY-----",
        )
        .await
        .expect("rotate user key");
    assert_eq!(
        rotated_user_key.public_key,
        "-----BEGIN PUBLIC KEY-----\nrotated user\n-----END PUBLIC KEY-----"
    );

    let rotated = store
        .update_or_create_default_key(
            &client.id,
            ActorType::Client,
            "-----BEGIN PUBLIC KEY-----\nclient\n-----END PUBLIC KEY-----",
        )
        .await
        .expect("create default client key");
    assert_eq!(rotated.actor_id, client.id);
    assert_eq!(rotated.key_name, "default");
    let keys = store
        .list_actor_keys(&client.id, ActorType::Client)
        .await
        .expect("list client keys");
    assert_eq!(keys.len(), 1);

    let principals = store
        .find_principals(Some(&org.id), &username)
        .await
        .expect("find principals");
    assert_eq!(principals.len(), 1);
    assert_eq!(principals[0].actor_type, ActorType::User);

    store
        .delete_client(&org.id, &client.name)
        .await
        .expect("delete client");
    assert!(store
        .find_client(&org.id, &client.name)
        .await
        .expect("find deleted client")
        .is_none());
}

#[tokio::test]
async fn checks_acls_through_nested_groups() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;

    let actor = store.create_actor().await.expect("create actor");
    let parent = store
        .create_group_for_org(&fixture.org_id, &format!("parent-{}", suffix()))
        .await
        .expect("create parent group");
    let child = store
        .create_group_for_org(&fixture.org_id, &format!("child-{}", suffix()))
        .await
        .expect("create child group");
    store
        .add_group_member(&child.authz_id, &actor)
        .await
        .expect("add actor to child group");
    store
        .add_group_child(&parent.authz_id, &child.authz_id)
        .await
        .expect("nest child under parent");

    let target = store.create_object().await.expect("create ACL target");
    store
        .set_acl(&target, Permission::Read, None, Some(&parent.authz_id))
        .await
        .expect("grant read to parent group");

    assert!(store
        .check_permission(&actor, &target, Permission::Read)
        .await
        .expect("recursive read check"));
    assert!(!store
        .check_permission(&actor, &target, Permission::Update)
        .await
        .expect("denied update check"));

    let username = format!("member-{}", suffix());
    let user = store
        .create_user(
            &username,
            &format!("{username}@example.test"),
            "bcrypt",
            "$2b$12$hash",
            "salt",
            false,
        )
        .await
        .expect("create group member user");
    store
        .add_group_member(&child.authz_id, &user.authz_id)
        .await
        .expect("add user to child group");
    let members = store
        .list_group_members(&fixture.org_id, &child.authz_id)
        .await
        .expect("list group members");
    assert_eq!(
        members,
        GroupMembers {
            users: vec![username],
            clients: Vec::new(),
            groups: Vec::new(),
        }
    );
    store
        .remove_group_member(&child.authz_id, &user.authz_id)
        .await
        .expect("remove user from child group");
    assert_eq!(
        store
            .list_group_members(&fixture.org_id, &child.authz_id)
            .await
            .expect("list group members after removal"),
        GroupMembers::default()
    );

    // A direct actor grant also satisfies the check.
    store
        .set_acl(&target, Permission::Update, Some(&actor), None)
        .await
        .expect("grant update to actor");
    assert!(store
        .check_permission(&actor, &target, Permission::Update)
        .await
        .expect("direct update check"));

    // Cycles are rejected.
    let cycle = store
        .add_group_child(&child.authz_id, &parent.authz_id)
        .await;
    assert!(matches!(cycle, Err(StoreError::Invalid(_))));

    let entries = store
        .get_acl_entries(&target)
        .await
        .expect("read ACL entries");
    assert_eq!(entries.len(), 5);
    assert!(entries
        .iter()
        .any(|entry| entry.permission == Permission::Read && !entry.groups.is_empty()));

    let found = store
        .find_acl_target(&fixture.org_id, "nodes", "does-not-exist")
        .await
        .expect("find ACL target");
    assert!(found.is_none());
}

#[tokio::test]
async fn performs_nodes_roles_environments_and_data_bag_crud() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let creator = &fixture.creator_authz_id;

    let node = store
        .create_node(
            &fixture.org_id,
            NewNode {
                name: "node-1",
                body: r#"{"name":"node-1"}"#,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: creator,
            },
        )
        .await
        .expect("create node");
    assert!(is_32_hex(&node.id));
    assert_eq!(
        store
            .find_node(&fixture.org_id, "node-1")
            .await
            .expect("find node")
            .as_ref(),
        Some(&node)
    );
    let updated = store
        .update_node(
            &fixture.org_id,
            "node-1",
            r#"{"name":"node-1","run_list":["role[web]"]}"#,
            "production",
            Some("base"),
            Some("prod"),
        )
        .await
        .expect("update node");
    assert_eq!(updated.environment, "production");
    assert_eq!(
        store
            .list_nodes_in_environment(&fixture.org_id, "production")
            .await
            .expect("list nodes by environment")
            .len(),
        1
    );
    assert_eq!(
        store
            .list_nodes(&fixture.org_id)
            .await
            .expect("list nodes")
            .len(),
        1
    );

    let environment = store
        .create_environment(
            &fixture.org_id,
            "production",
            r#"{"name":"production"}"#,
            creator,
        )
        .await
        .expect("create environment");
    assert_eq!(
        store
            .find_environment(&fixture.org_id, "production")
            .await
            .expect("find environment")
            .as_ref(),
        Some(&environment)
    );
    let environment = store
        .update_environment(
            &fixture.org_id,
            "production",
            r#"{"name":"production","description":"prod"}"#,
        )
        .await
        .expect("update environment");
    assert!(environment.serialized_object.contains("prod"));
    assert_eq!(
        store
            .list_environments(&fixture.org_id)
            .await
            .expect("list environments")
            .len(),
        1
    );

    let role = store
        .create_role(
            &fixture.org_id,
            "web",
            r#"{"name":"web","run_list":[]}"#,
            creator,
        )
        .await
        .expect("create role");
    assert_eq!(
        store
            .find_role(&fixture.org_id, "web")
            .await
            .expect("find role")
            .as_ref(),
        Some(&role)
    );
    assert_eq!(
        store
            .update_role(
                &fixture.org_id,
                "web",
                r#"{"name":"web","run_list":["recipe[apache]"]}"#
            )
            .await
            .expect("update role")
            .serialized_object,
        r#"{"name":"web","run_list":["recipe[apache]"]}"#
    );
    assert_eq!(
        store
            .list_roles(&fixture.org_id)
            .await
            .expect("list roles")
            .len(),
        1
    );

    let data_bag = store
        .create_data_bag(&fixture.org_id, "secrets", creator)
        .await
        .expect("create data bag");
    assert_eq!(
        store
            .find_data_bag(&fixture.org_id, "secrets")
            .await
            .expect("find data bag")
            .as_ref(),
        Some(&data_bag)
    );
    let item = store
        .create_data_bag_item(
            &fixture.org_id,
            "secrets",
            "database",
            r#"{"id":"database","password":"hunter2"}"#,
        )
        .await
        .expect("create data bag item");
    assert_eq!(
        store
            .find_data_bag_item(&fixture.org_id, "secrets", "database")
            .await
            .expect("find data bag item")
            .as_ref(),
        Some(&item)
    );
    assert_eq!(
        store
            .update_data_bag_item(
                &fixture.org_id,
                "secrets",
                "database",
                r#"{"id":"database","password":"rotated"}"#
            )
            .await
            .expect("update data bag item")
            .serialized_object,
        r#"{"id":"database","password":"rotated"}"#
    );
    assert_eq!(
        store
            .list_data_bag_items(&fixture.org_id, "secrets")
            .await
            .expect("list data bag items")
            .len(),
        1
    );

    store
        .delete_data_bag_item(&fixture.org_id, "secrets", "database")
        .await
        .expect("delete data bag item");
    store
        .delete_data_bag(&fixture.org_id, "secrets")
        .await
        .expect("delete data bag");
    store
        .delete_role(&fixture.org_id, "web")
        .await
        .expect("delete role");
    store
        .delete_environment(&fixture.org_id, "production")
        .await
        .expect("delete environment");
    store
        .delete_node(&fixture.org_id, "node-1")
        .await
        .expect("delete node");
    assert!(store
        .find_node(&fixture.org_id, "node-1")
        .await
        .expect("find deleted node")
        .is_none());
    assert_eq!(
        store
            .list_data_bags(&fixture.org_id)
            .await
            .expect("list data bags")
            .len(),
        0
    );
}

#[tokio::test]
async fn commits_sandbox_checksums() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let checksums = vec![format!("{:032x}", 1), format!("{:032x}", 2)];

    let (sandbox_id, checks) = store
        .create_sandbox(&fixture.org_id, &checksums)
        .await
        .expect("create sandbox");
    assert!(is_32_hex(&sandbox_id));
    assert_eq!(checks.len(), 2);
    assert!(checks.iter().all(|check| check.needs_upload));
    assert!(store
        .sandbox_exists(&fixture.org_id, &sandbox_id)
        .await
        .expect("sandbox exists"));
    assert!(store
        .sandbox_created_at(&fixture.org_id, &sandbox_id)
        .await
        .expect("sandbox timestamp")
        .is_some());
    assert_eq!(
        store
            .sandbox_checksums(&fixture.org_id, &sandbox_id)
            .await
            .expect("sandbox checksums"),
        {
            let mut sorted = checksums.clone();
            sorted.sort();
            sorted
        }
    );
    assert!(!store
        .checksum_exists(&fixture.org_id, &checksums[0])
        .await
        .expect("checksum not committed"));

    store
        .commit_sandbox(&fixture.org_id, &sandbox_id)
        .await
        .expect("commit sandbox");
    assert!(!store
        .sandbox_exists(&fixture.org_id, &sandbox_id)
        .await
        .expect("sandbox consumed"));
    for checksum in &checksums {
        assert!(store
            .checksum_exists(&fixture.org_id, checksum)
            .await
            .expect("checksum committed"));
        assert_eq!(
            store
                .find_checksum(&fixture.org_id, checksum)
                .await
                .expect("find checksum")
                .as_deref(),
            Some(checksum.as_str())
        );
    }

    // A second sandbox reuses the committed checksums.
    let (_, checks) = store
        .create_sandbox(&fixture.org_id, &checksums)
        .await
        .expect("create second sandbox");
    assert!(checks.iter().all(|check| !check.needs_upload));
}

#[tokio::test]
async fn stores_cookbook_versions_with_checksums() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let checksums = vec![format!("{:032x}", 42)];
    let (sandbox_id, _) = store
        .create_sandbox(&fixture.org_id, &checksums)
        .await
        .expect("create sandbox");
    store
        .commit_sandbox(&fixture.org_id, &sandbox_id)
        .await
        .expect("commit sandbox checksums");

    let version = store
        .create_cookbook_version(
            &fixture.org_id,
            "apache",
            "1.2.3",
            r#"{"recipes":[{"name":"default.rb","checksum":"0000000000000000000000000000002a","path":"recipes/default.rb","specificity":"default"}]}"#,
            &checksums,
            &fixture.creator_authz_id,
        )
        .await
        .expect("create cookbook version");
    assert!(is_32_hex(&version.id));
    assert_eq!(version.version, "1.2.3");
    assert_eq!(version.checksums, checksums);
    assert!(is_32_hex(&version.authz_id));

    assert_eq!(
        store
            .find_cookbook_version(&fixture.org_id, "apache", "1.2.3")
            .await
            .expect("find cookbook version")
            .as_ref(),
        Some(&version)
    );
    assert_eq!(
        store
            .list_cookbook_versions(&fixture.org_id)
            .await
            .expect("list cookbook versions"),
        vec![version.clone()]
    );

    // Same version again is a conflict.
    let duplicate = store
        .create_cookbook_version(
            &fixture.org_id,
            "apache",
            "1.2.3",
            "{}",
            &checksums,
            &fixture.creator_authz_id,
        )
        .await;
    assert!(matches!(duplicate, Err(StoreError::Conflict(_))));

    store
        .delete_cookbook_version(&fixture.org_id, "apache", "1.2.3")
        .await
        .expect("delete cookbook version");
    assert!(store
        .find_cookbook_version(&fixture.org_id, "apache", "1.2.3")
        .await
        .expect("find deleted cookbook version")
        .is_none());
}

#[tokio::test]
async fn stores_policy_revisions_and_group_associations() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let body = r#"{"name":"base","revision_id":"1.0.0","run_list":[]}"#;

    let revision = store
        .create_policy_revision(
            &fixture.org_id,
            "base",
            "1.0.0",
            body,
            &fixture.creator_authz_id,
        )
        .await
        .expect("create policy revision");
    assert!(is_32_hex(&revision.id));
    assert!(is_32_hex(&revision.policy_authz_id));
    assert_eq!(
        store
            .find_policy_revision(&fixture.org_id, "base", "1.0.0")
            .await
            .expect("find policy revision")
            .as_ref(),
        Some(&revision)
    );
    assert!(store
        .find_policy(&fixture.org_id, "base")
        .await
        .expect("find policy")
        .is_some());

    let (association, created) = store
        .associate_policy_revision(
            &fixture.org_id,
            "prod",
            "base",
            "1.0.0",
            body,
            r#"{"name":"prod"}"#,
            &fixture.creator_authz_id,
        )
        .await
        .expect("associate policy revision");
    assert!(created);
    assert_eq!(association.policy_group_name, "prod");
    assert_eq!(association.revision_id, "1.0.0");
    assert!(store
        .list_policy_groups_for_revision(&fixture.org_id, "base", "1.0.0")
        .await
        .expect("policy groups for revision")
        .contains(&"prod".to_string()));

    store
        .delete_policy_revision(&fixture.org_id, "base", "1.0.0")
        .await
        .expect("delete policy revision");
    assert!(store
        .find_policy_revision(&fixture.org_id, "base", "1.0.0")
        .await
        .expect("find deleted policy revision")
        .is_none());
}

#[tokio::test]
async fn schema_contains_the_unified_tables() {
    let Some(store) = test_store().await else {
        return;
    };
    let names = store.table_names().await.expect("list tables");
    for expected in [
        "orgs",
        "users",
        "clients",
        "actor_keys",
        "authz_entities",
        "authz_acl_actor",
        "authz_acl_group",
        "authz_group_actors",
        "authz_group_groups",
        "containers",
        "groups",
        "nodes",
        "roles",
        "environments",
        "data_bags",
        "data_bag_items",
        "checksums",
        "sandboxed_checksums",
        "cookbooks",
        "cookbook_versions",
        "cookbook_version_checksums",
        "policies",
        "policy_groups",
        "policy_revisions",
        "policy_group_revision_associations",
        "oauth_applications",
        "oauth_access_grants",
        "oauth_access_tokens",
        "kvpair",
        "telemetry",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing table {expected}"
        );
    }
}
