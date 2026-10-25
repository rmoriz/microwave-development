//! PostgreSQL integration tests for the first metadata-store slice.
//!
//! Set `MICROWAVE_TEST_POSTGRES_URL` (or run `scripts/run-postgres-tests.sh`)
//! to execute these tests. Without a reachable database every test skips with
//! a clear message so `cargo test --workspace` stays green on developer
//! machines without Docker.

use microwave_store::{
    ActorType, ArtifactRepository, AuthzRepository, Compression, CookbookRepository, GroupMembers,
    IdentityRepository, KvRepository, MembershipRepository, NewNode, OAuthRepository,
    ObjectRepository, Permission, PolicyRepository, SandboxRepository, SecretsRepository,
    StoreError, UserProfile, COMPRESSED_PREFIX,
};
use microwave_store_postgres::PostgresStore;
use sqlx::Row;

fn assert_metadata_store<T: microwave_store::MetadataStore>() {}

#[test]
fn postgres_satisfies_metadata_store() {
    assert_metadata_store::<PostgresStore>();
}

fn test_url() -> Option<String> {
    std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

/// Returns `None` (after printing a skip message) only when no test database
/// was configured. A configured database that cannot be reached or migrated is
/// a hard failure: silently skipping would hide broken migrations.
async fn test_store() -> Option<PostgresStore> {
    let Some(url) = test_url() else {
        eprintln!(
            "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; start a disposable \
             PostgreSQL with scripts/run-postgres-tests.sh"
        );
        return None;
    };
    Some(
        PostgresStore::connect(&url)
            .await
            .expect("connect to configured PostgreSQL"),
    )
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
    // Other integration tests run against the same database in parallel, so
    // only assert that this user is listed rather than a global count.
    assert!(store
        .list_users()
        .await
        .expect("list users")
        .contains(&user));

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
async fn serialized_objects_are_compressed_transparently() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let name = format!("compressed-node-{}", suffix());
    let body = format!(r#"{{"name":"{name}","run_list":[]}}"#);

    store
        .create_node(
            &fixture.org_id,
            NewNode {
                name: &name,
                body: &body,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &fixture.creator_authz_id,
            },
        )
        .await
        .expect("create compressed node");

    let raw: String =
        sqlx::query_scalar("SELECT serialized_object FROM nodes WHERE org_id = $1 AND name = $2")
            .bind(&fixture.org_id)
            .bind(&name)
            .fetch_one(store.pool())
            .await
            .expect("read raw serialized_object");
    assert!(raw.starts_with(COMPRESSED_PREFIX));
    assert!(!raw.contains("run_list"));

    let node = store
        .find_node(&fixture.org_id, &name)
        .await
        .expect("find node")
        .expect("node exists");
    assert_eq!(node.serialized_object, body);

    // Legacy plain JSON written by an older server remains readable.
    sqlx::query("UPDATE nodes SET serialized_object = $3 WHERE org_id = $1 AND name = $2")
        .bind(&fixture.org_id)
        .bind(&name)
        .bind(r#"{"name":"legacy-plain"}"#)
        .execute(store.pool())
        .await
        .expect("write legacy plain value");
    let legacy = store
        .find_node(&fixture.org_id, &name)
        .await
        .expect("read legacy node")
        .expect("node exists");
    assert_eq!(legacy.serialized_object, r#"{"name":"legacy-plain"}"#);

    // Corrupt compressed values fail as a store error rather than a panic.
    sqlx::query("UPDATE nodes SET serialized_object = $3 WHERE org_id = $1 AND name = $2")
        .bind(&fixture.org_id)
        .bind(&name)
        .bind("mwz1:not-base64!")
        .execute(store.pool())
        .await
        .expect("write corrupt compressed value");
    let error = store
        .find_node(&fixture.org_id, &name)
        .await
        .expect_err("corrupt value must fail");
    assert!(matches!(error, StoreError::Compression(_)));

    // Do not leave an undecodable row behind for the snapshot suite, which
    // exports every row in the shared test database.
    store
        .delete_node(&fixture.org_id, &name)
        .await
        .expect("delete corrupt test node");
}

#[tokio::test]
async fn compression_none_writes_plain_and_reads_compressed_values() {
    let Some(store) = test_store().await else {
        return;
    };
    let url = test_url().expect("test URL is present when test_store succeeds");
    let fixture = fixture(&store).await;

    let compressed_name = format!("compressed-source-{}", suffix());
    let compressed_body = format!(r#"{{"name":"{compressed_name}"}}"#);
    store
        .create_node(
            &fixture.org_id,
            NewNode {
                name: &compressed_name,
                body: &compressed_body,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &fixture.creator_authz_id,
            },
        )
        .await
        .expect("create compressed node");

    let plain_store = PostgresStore::connect_with_compression(&url, Compression::None)
        .await
        .expect("connect with compression disabled");

    // A none-mode store still reads rows written in the compressed format.
    let node = plain_store
        .find_node(&fixture.org_id, &compressed_name)
        .await
        .expect("read compressed node")
        .expect("node exists");
    assert_eq!(node.serialized_object, compressed_body);

    let plain_name = format!("plain-target-{}", suffix());
    let plain_body = format!(r#"{{"name":"{plain_name}"}}"#);
    plain_store
        .create_node(
            &fixture.org_id,
            NewNode {
                name: &plain_name,
                body: &plain_body,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &fixture.creator_authz_id,
            },
        )
        .await
        .expect("create plain node");
    let raw: String =
        sqlx::query_scalar("SELECT serialized_object FROM nodes WHERE org_id = $1 AND name = $2")
            .bind(&fixture.org_id)
            .bind(&plain_name)
            .fetch_one(plain_store.pool())
            .await
            .expect("read raw plain value");
    assert_eq!(raw, plain_body);
    assert_eq!(
        plain_store
            .find_node(&fixture.org_id, &plain_name)
            .await
            .expect("read plain node")
            .expect("node exists")
            .serialized_object,
        plain_body
    );

    store
        .delete_node(&fixture.org_id, &compressed_name)
        .await
        .expect("delete compressed node");
    plain_store
        .delete_node(&fixture.org_id, &plain_name)
        .await
        .expect("delete plain node");
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
            true,
            &fixture.creator_authz_id,
        )
        .await
        .expect("create cookbook version");
    assert!(is_32_hex(&version.id));
    assert_eq!(version.version, "1.2.3");
    assert_eq!(version.checksums, checksums);
    assert!(version.frozen);
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
            false,
            &fixture.creator_authz_id,
        )
        .await;
    assert!(matches!(duplicate, Err(StoreError::Conflict(_))));

    let (deleted, purged) = store
        .delete_cookbook_version(&fixture.org_id, "apache", "1.2.3")
        .await
        .expect("delete cookbook version");
    assert_eq!(deleted.id, version.id);
    assert!(deleted.frozen);
    assert_eq!(deleted.checksums, checksums);
    assert_eq!(purged, checksums);
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
        "sandboxes",
        "sandboxed_checksums",
        "cookbooks",
        "cookbook_versions",
        "cookbook_version_checksums",
        "cookbook_artifacts",
        "cookbook_artifact_versions",
        "cookbook_artifact_version_checksums",
        "policies",
        "policy_groups",
        "policy_revisions",
        "policy_group_revision_associations",
        "oauth_applications",
        "oauth_access_grants",
        "oauth_access_tokens",
        "kvpair",
        "global_groups",
        "org_user_invites",
        "telemetry",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing table {expected}"
        );
    }
}

async fn create_test_user(store: &PostgresStore) -> microwave_store::User {
    let username = format!("user-{}", suffix());
    store
        .create_user(
            &username,
            &format!("{username}@example.test"),
            "bcrypt",
            "$2b$12$hash",
            "salt",
            false,
        )
        .await
        .expect("create user")
}

#[tokio::test]
async fn manages_membership_and_invitations() {
    let Some(store) = test_store().await else {
        return;
    };
    let org = store
        .create_org(&format!("org-{}", suffix()), "Membership Org")
        .await
        .expect("create org");
    let users_group = store
        .create_group_for_org(&org.id, "users")
        .await
        .expect("create users group");
    let admins_group = store
        .create_group_for_org(&org.id, "admins")
        .await
        .expect("create admins group");
    let read_group_name = format!("{}_read_access_group", org.name);
    store
        .ensure_global_group(&read_group_name)
        .await
        .expect("ensure read access group");

    let user = create_test_user(&store).await;
    let other = create_test_user(&store).await;
    let actor = store.create_actor().await.expect("create actor");

    store
        .add_user_to_org(&org.id, &user.id)
        .await
        .expect("add user to org");
    assert!(store
        .is_user_in_org(&org.id, &user.id)
        .await
        .expect("is user in org"));
    assert_eq!(
        store.list_org_users(&org.id).await.expect("list org users"),
        vec![user.clone()]
    );
    assert_eq!(
        store
            .list_user_orgs(&user.id)
            .await
            .expect("list user orgs"),
        vec![org.clone()]
    );
    assert!(store
        .user_in_group(&users_group.authz_id, &user.authz_id)
        .await
        .expect("user is in users group"));

    let entries = store
        .get_acl_entries(&user.authz_id)
        .await
        .expect("read user ACL");
    let read = entries
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .expect("read entry");
    assert!(read.groups.contains(&format!("::{read_group_name}")));

    assert!(!store
        .is_org_admin_for_user(&actor, &user.id)
        .await
        .expect("is not admin"));
    store
        .add_group_member(&admins_group.authz_id, &user.authz_id)
        .await
        .expect("add user to admins");
    assert!(store
        .is_org_admin_for_user(&user.authz_id, &user.id)
        .await
        .expect("is admin"));

    // The admin guard rejects removal before the admins membership is gone.
    let forbidden = store
        .remove_user_from_org(&org.id, &user.id, &user.username)
        .await;
    assert!(matches!(forbidden, Err(StoreError::Forbidden(_))));
    store
        .remove_group_member(&admins_group.authz_id, &user.authz_id)
        .await
        .expect("remove user from admins");

    // Unrelated ACL entries survive removal.
    store
        .set_acl(
            &user.authz_id,
            Permission::Update,
            Some(&other.authz_id),
            None,
        )
        .await
        .expect("grant unrelated update");
    store
        .remove_user_from_org(&org.id, &user.id, &user.username)
        .await
        .expect("remove user from org");
    assert!(!store
        .is_user_in_org(&org.id, &user.id)
        .await
        .expect("user left org"));
    assert!(!store
        .user_in_group(&users_group.authz_id, &user.authz_id)
        .await
        .expect("left users group"));
    let entries = store
        .get_acl_entries(&user.authz_id)
        .await
        .expect("read user ACL after removal");
    let read = entries
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .expect("read entry");
    assert!(!read.groups.contains(&format!("::{read_group_name}")));
    let update = entries
        .iter()
        .find(|entry| entry.permission == Permission::Update)
        .expect("update entry");
    assert_eq!(update.users, vec![other.username.clone()]);

    // Accept an invitation: association, users group and read ACE.
    let invite = store
        .create_invitation(&org.id, &other.id, &user.id)
        .await
        .expect("create invitation");
    assert_eq!(
        store
            .find_invitation(&invite.id)
            .await
            .expect("find invite"),
        Some(invite.clone())
    );
    assert_eq!(
        store
            .list_org_invitations(&org.id)
            .await
            .expect("list org invites"),
        vec![invite.clone()]
    );
    assert_eq!(
        store
            .list_user_invitations(&other.id)
            .await
            .expect("list user invites"),
        vec![invite.clone()]
    );
    assert_eq!(
        store
            .count_user_invitations(&other.id)
            .await
            .expect("count invites"),
        1
    );
    let accepted = store
        .accept_invitation(&invite.id)
        .await
        .expect("accept invitation");
    assert_eq!(accepted, invite);
    assert!(store
        .is_user_in_org(&org.id, &other.id)
        .await
        .expect("invited user associated"));
    assert!(store
        .user_in_group(&users_group.authz_id, &other.authz_id)
        .await
        .expect("invited user in users group"));
    assert_eq!(
        store
            .count_user_invitations(&other.id)
            .await
            .expect("invite consumed"),
        0
    );

    // Reject an invitation without creating an association.
    let third = create_test_user(&store).await;
    let rejected = store
        .create_invitation(&org.id, &third.id, &user.id)
        .await
        .expect("create rejected invitation");
    assert_eq!(
        store
            .reject_invitation(&rejected.id)
            .await
            .expect("reject invitation"),
        rejected
    );
    assert!(!store
        .is_user_in_org(&org.id, &third.id)
        .await
        .expect("rejected user not associated"));

    // Explicit delete is also supported.
    let deleted = store
        .create_invitation(&org.id, &third.id, &user.id)
        .await
        .expect("create deletable invitation");
    store
        .delete_invitation(&deleted.id)
        .await
        .expect("delete invitation");
    assert!(store
        .find_invitation(&deleted.id)
        .await
        .expect("deleted invitation")
        .is_none());
    assert!(matches!(
        store.delete_invitation(&deleted.id).await,
        Err(StoreError::NotFound(_))
    ));

    // `associate_user_org` (used by the organization bootstrap flow) also
    // grants the read-access ACE, like SQLite.
    let direct = create_test_user(&store).await;
    store
        .associate_user_org(&org.id, &direct.id)
        .await
        .expect("associate user directly");
    assert!(store
        .is_user_in_org(&org.id, &direct.id)
        .await
        .expect("direct association"));
    let entries = store
        .get_acl_entries(&direct.authz_id)
        .await
        .expect("read direct user ACL");
    let read = entries
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .expect("direct read entry");
    assert!(read.groups.contains(&format!("::{read_group_name}")));
}

#[tokio::test]
async fn manages_cookbook_artifacts() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let checksum = format!("{:032x}", 7);
    let checksums = vec![checksum.clone()];
    let (sandbox_id, _) = store
        .create_sandbox(&fixture.org_id, &checksums)
        .await
        .expect("create sandbox");
    store
        .commit_sandbox(&fixture.org_id, &sandbox_id)
        .await
        .expect("commit sandbox");

    let manifest = r#"{"name":"apache","version":"1.0.0"}"#;
    let version = store
        .create_cookbook_artifact_version(
            &fixture.org_id,
            "apache",
            "1.0.0",
            manifest,
            &checksums,
            &fixture.creator_authz_id,
        )
        .await
        .expect("create artifact version");
    assert!(is_32_hex(&version.authz_id));
    assert_eq!(version.identifier, "1.0.0");
    assert_eq!(version.created_by, fixture.creator_authz_id);
    assert_eq!(
        store
            .find_cookbook_artifact_version(&fixture.org_id, "apache", "1.0.0")
            .await
            .expect("find artifact version")
            .as_ref(),
        Some(&version)
    );

    let second = store
        .create_cookbook_artifact_version(
            &fixture.org_id,
            "apache",
            "2.0.0",
            r#"{"name":"apache","version":"2.0.0"}"#,
            &[],
            &fixture.creator_authz_id,
        )
        .await
        .expect("create second artifact version");
    assert_eq!(
        store
            .find_cookbook_artifact(&fixture.org_id, "apache")
            .await
            .expect("find artifact")
            .expect("artifact exists")
            .identifiers,
        vec!["1.0.0".to_string(), "2.0.0".to_string()]
    );
    assert_eq!(
        store
            .list_cookbook_artifacts(&fixture.org_id)
            .await
            .expect("list artifacts")
            .len(),
        1
    );

    // Deleting one version keeps the shared checksum because another artifact
    // version references it? Here only the first version referenced it, so the
    // checksum becomes purgeable once no version references it.
    let (deleted, purged) = store
        .delete_cookbook_artifact_version(&fixture.org_id, "apache", "1.0.0")
        .await
        .expect("delete artifact version");
    assert_eq!(deleted.id, version.id);
    assert_eq!(purged, checksums);
    let remaining = store
        .find_cookbook_artifact(&fixture.org_id, "apache")
        .await
        .expect("find remaining artifact")
        .expect("artifact remains");
    assert_eq!(remaining.identifiers, vec!["2.0.0".to_string()]);

    let (deleted, purged) = store
        .delete_cookbook_artifact_version(&fixture.org_id, "apache", "2.0.0")
        .await
        .expect("delete last artifact version");
    assert_eq!(deleted.id, second.id);
    assert!(purged.is_empty());
    assert!(store
        .find_cookbook_artifact(&fixture.org_id, "apache")
        .await
        .expect("find deleted artifact")
        .is_none());
    assert!(matches!(
        store
            .delete_cookbook_artifact_version(&fixture.org_id, "apache", "2.0.0")
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn manages_oauth_applications_tokens_sessions_and_resets() {
    let Some(store) = test_store().await else {
        return;
    };
    let name = format!("app-{}", suffix());
    let app = store
        .create_oauth_application(&name, "https://example.test/callback", "profile", false)
        .await
        .expect("create oauth application");
    assert!(!app.confidential);
    assert_eq!(
        store
            .find_oauth_application(app.id)
            .await
            .expect("find application")
            .as_ref(),
        Some(&app)
    );
    assert_eq!(
        store
            .find_oauth_application_by_uid(&app.uid)
            .await
            .expect("find application by uid")
            .as_ref(),
        Some(&app)
    );
    assert!(store
        .list_oauth_applications()
        .await
        .expect("list applications")
        .contains(&app));
    let updated = store
        .update_oauth_application(app.id, Some(&format!("{name}-renamed")), None)
        .await
        .expect("update application");
    assert_eq!(updated.name, format!("{name}-renamed"));
    assert_eq!(updated.redirect_uri, app.redirect_uri);

    let grant = store
        .create_oauth_access_grant(&app.uid, app.id, &app.redirect_uri, "profile", 3600)
        .await
        .expect("create grant");
    assert_eq!(grant.expires_in, 3600);
    assert!(grant.revoked_at.is_none());
    store
        .revoke_oauth_access_grant(&grant.token)
        .await
        .expect("revoke grant");
    assert!(store
        .find_oauth_access_grant(&grant.token)
        .await
        .expect("find revoked grant")
        .expect("grant exists")
        .revoked_at
        .is_some());

    let token = store
        .create_oauth_access_token(&app.uid, app.id, "profile", Some(7200), true)
        .await
        .expect("create token");
    assert_eq!(token.expires_in, Some(7200));
    let refresh = token.refresh_token.clone().expect("refresh token");
    assert_eq!(
        store
            .find_oauth_access_token(&token.token)
            .await
            .expect("find token")
            .as_ref(),
        Some(&token)
    );
    assert_eq!(
        store
            .find_oauth_access_token_by_refresh(&refresh)
            .await
            .expect("find token by refresh")
            .as_ref(),
        Some(&token)
    );
    store
        .revoke_oauth_access_token_by_refresh(&refresh)
        .await
        .expect("revoke token by refresh");
    assert!(store
        .find_oauth_access_token(&token.token)
        .await
        .expect("find revoked token")
        .expect("token exists")
        .revoked_at
        .is_some());

    let session_user = format!("user-{}", suffix());
    let session = store
        .create_id_session(&session_user)
        .await
        .expect("create id session");
    assert_eq!(
        store.find_id_session(&session).await.expect("find session"),
        Some(session_user)
    );
    store
        .delete_id_session(&session)
        .await
        .expect("delete id session");
    assert_eq!(
        store
            .find_id_session(&session)
            .await
            .expect("deleted session"),
        None
    );

    // Two concurrent consumers must not both use the same reset token.
    let concurrent = store
        .create_password_reset_token("bob")
        .await
        .expect("create concurrent reset token");
    let (left, right) = tokio::join!(
        store.consume_password_reset_token(&concurrent),
        store.consume_password_reset_token(&concurrent)
    );
    let results = [
        left.expect("first concurrent consume"),
        right.expect("second concurrent consume"),
    ];
    assert_eq!(
        results.iter().filter(|value| value.is_some()).count(),
        1,
        "exactly one consumer must win: {results:?}"
    );
    assert!(results.contains(&Some("bob".into())));
    assert_eq!(
        store
            .find_password_reset_token(&concurrent)
            .await
            .expect("concurrent reset token consumed"),
        None
    );

    let reset = store
        .create_password_reset_token("alice")
        .await
        .expect("create reset token");
    assert_eq!(
        store
            .find_password_reset_token(&reset)
            .await
            .expect("find reset token"),
        Some("alice".into())
    );
    assert_eq!(
        store
            .consume_password_reset_token(&reset)
            .await
            .expect("consume reset token"),
        Some("alice".into())
    );
    assert_eq!(
        store
            .find_password_reset_token(&reset)
            .await
            .expect("consumed reset token"),
        None
    );
}

#[tokio::test]
async fn stores_kv_and_secrets() {
    let Some(store) = test_store().await else {
        return;
    };
    let key = format!("test:{}", suffix());
    assert_eq!(store.get_kv(&key).await.expect("missing kv"), None);
    store.set_kv(&key, "value").await.expect("set kv");
    assert_eq!(
        store.get_kv(&key).await.expect("get kv"),
        Some("value".into())
    );
    store.set_kv(&key, "updated").await.expect("update kv");
    assert_eq!(
        store.get_kv(&key).await.expect("get updated kv"),
        Some("updated".into())
    );
    assert!(store.remove_kv(&key).await.expect("remove kv"));
    assert!(!store.remove_kv(&key).await.expect("remove missing kv"));

    assert!(!store
        .maintenance_enabled()
        .await
        .expect("maintenance default"));
    store
        .set_maintenance_enabled(true)
        .await
        .expect("enable maintenance");
    assert!(store.maintenance_enabled().await.expect("maintenance on"));
    store
        .set_maintenance_enabled(false)
        .await
        .expect("disable maintenance");
    assert!(!store.maintenance_enabled().await.expect("maintenance off"));

    let group = "bookshelf";
    let name = format!("key-{}", suffix());
    assert_eq!(
        store
            .get_secret(group, &name)
            .await
            .expect("missing secret"),
        None
    );
    store
        .set_secret(group, &name, "s3cr3t")
        .await
        .expect("set secret");
    assert_eq!(
        store.get_secret(group, &name).await.expect("get secret"),
        Some("s3cr3t".into())
    );
    store
        .remove_secret(group, &name)
        .await
        .expect("remove secret");
    assert!(matches!(
        store.remove_secret(group, &name).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.set_secret("bad group", &name, "x").await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.set_secret(group, &name, "").await,
        Err(StoreError::Invalid(_))
    ));

    // HA-safe bootstrap insert: concurrent writers must agree on one value.
    let absent = format!("bootstrap-{}", suffix());
    let (left, right) = tokio::join!(
        store.set_secret_if_absent(group, &absent, "left"),
        store.set_secret_if_absent(group, &absent, "right"),
    );
    let left = left.expect("left bootstrap insert");
    let right = right.expect("right bootstrap insert");
    assert_eq!(left, right, "both callers must observe the winning value");
    assert_eq!(
        store
            .get_secret(group, &absent)
            .await
            .expect("bootstrap secret"),
        Some(left.clone())
    );
    assert_eq!(
        store
            .set_secret_if_absent(group, &absent, "other")
            .await
            .expect("repeat bootstrap insert"),
        left
    );
    assert!(matches!(
        store.set_secret_if_absent("bad group", &absent, "x").await,
        Err(StoreError::Invalid(_))
    ));
    store.remove_secret(group, &absent).await.expect("cleanup");

    // A legacy row with a SQL NULL value must be healed instead of panicking
    // or returning a value the store did not retain.
    let null_name = format!("null-{}", suffix());
    let null_key = microwave_store::secret_kv_key(group, &null_name);
    sqlx::query("INSERT INTO kvpair (key, value, updated_at) VALUES ($1, NULL, $2)")
        .bind(&null_key)
        .bind("2026-10-25T00:00:00Z")
        .execute(store.pool())
        .await
        .expect("insert NULL secret row");
    assert_eq!(
        store
            .set_secret_if_absent(group, &null_name, "healed")
            .await
            .expect("heal NULL secret"),
        "healed"
    );
    assert_eq!(
        store
            .get_secret(group, &null_name)
            .await
            .expect("read healed secret"),
        Some("healed".into())
    );
    store
        .remove_secret(group, &null_name)
        .await
        .expect("cleanup NULL secret");
}

#[tokio::test]
async fn resolves_global_groups_in_acls_and_membership() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let global = store
        .ensure_global_group(&format!("global-{}", suffix()))
        .await
        .expect("ensure global group");
    let actor = store.create_actor().await.expect("create actor");
    store
        .add_group_member(&global.authz_id, &actor)
        .await
        .expect("add actor to global group");
    let target = store.create_object().await.expect("create ACL target");
    let global_name = format!("::{}", global.name);

    store
        .set_acl_for_permission(
            &target,
            Permission::Read,
            &[],
            std::slice::from_ref(&global_name),
        )
        .await
        .expect("grant read to global group");
    assert!(store
        .check_permission(&actor, &target, Permission::Read)
        .await
        .expect("global read check"));
    let entries = store.get_acl_entries(&target).await.expect("read ACL");
    let read = entries
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .expect("read entry");
    assert_eq!(read.groups, vec![global_name.clone()]);

    store
        .set_acl_for_permission_split(
            &target,
            Permission::Update,
            &[],
            &[],
            std::slice::from_ref(&global_name),
        )
        .await
        .expect("grant split update to global group");
    assert!(store
        .check_permission(&actor, &target, Permission::Update)
        .await
        .expect("global update check"));

    let local = store
        .create_group_for_org(&fixture.org_id, &format!("local-{}", suffix()))
        .await
        .expect("create local group");
    store
        .add_group_child(&local.authz_id, &global.authz_id)
        .await
        .expect("nest global group");
    let members = store
        .list_group_members(&fixture.org_id, &local.authz_id)
        .await
        .expect("list group members");
    assert_eq!(members.groups, vec![global_name]);
}

#[tokio::test]
async fn keeps_empty_sandboxes_and_supports_later_uploads() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let (sandbox_id, checks) = store
        .create_sandbox(&fixture.org_id, &[])
        .await
        .expect("create empty sandbox");
    assert!(checks.is_empty());
    assert!(store
        .sandbox_exists(&fixture.org_id, &sandbox_id)
        .await
        .expect("empty sandbox exists"));
    assert!(store
        .sandbox_created_at(&fixture.org_id, &sandbox_id)
        .await
        .expect("empty sandbox timestamp")
        .is_some());
    assert!(store
        .sandbox_checksums(&fixture.org_id, &sandbox_id)
        .await
        .expect("empty sandbox checksums")
        .is_empty());
    store
        .commit_sandbox(&fixture.org_id, &sandbox_id)
        .await
        .expect("commit empty sandbox");
    assert!(!store
        .sandbox_exists(&fixture.org_id, &sandbox_id)
        .await
        .expect("empty sandbox consumed"));
}

#[tokio::test]
async fn backfills_legacy_sandboxes_from_uploaded_checksums() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let sandbox_id = format!("legacy-{}", suffix());
    let earlier = "2020-01-01T00:00:00Z";
    let later = "2020-02-02T00:00:00Z";

    for (checksum, created_at) in [("legacy-a", earlier), ("legacy-b", later)] {
        sqlx::query(
            "INSERT INTO sandboxed_checksums (org_id, sandbox_id, checksum, created_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&fixture.org_id)
        .bind(&sandbox_id)
        .bind(checksum)
        .bind(created_at)
        .execute(store.pool())
        .await
        .expect("insert legacy sandbox checksum");
    }

    // This is the pre-backfill state: uploads exist but the sandbox row does
    // not, because the row was only ever inferred from checksums.
    assert!(!store
        .sandbox_exists(&fixture.org_id, &sandbox_id)
        .await
        .expect("legacy sandbox hidden before migration"));

    store.migrate().await.expect("rerun migration backfill");
    assert!(store
        .sandbox_exists(&fixture.org_id, &sandbox_id)
        .await
        .expect("legacy sandbox visible after migration"));
    assert_eq!(
        store
            .sandbox_created_at(&fixture.org_id, &sandbox_id)
            .await
            .expect("legacy sandbox timestamp"),
        Some(earlier.to_string())
    );
}

#[tokio::test]
async fn retains_checksums_shared_between_versions() {
    let Some(store) = test_store().await else {
        return;
    };
    let fixture = fixture(&store).await;
    let checksum = format!("{:032x}", 99);
    let checksums = vec![checksum.clone()];
    let (sandbox_id, _) = store
        .create_sandbox(&fixture.org_id, &checksums)
        .await
        .expect("create sandbox");
    store
        .commit_sandbox(&fixture.org_id, &sandbox_id)
        .await
        .expect("commit sandbox");

    let manifest = r#"{"name":"shared"}"#;
    store
        .create_cookbook_version(
            &fixture.org_id,
            "shared",
            "1.0.0",
            manifest,
            &checksums,
            false,
            &fixture.creator_authz_id,
        )
        .await
        .expect("create first cookbook version");
    store
        .create_cookbook_version(
            &fixture.org_id,
            "shared",
            "2.0.0",
            manifest,
            &checksums,
            false,
            &fixture.creator_authz_id,
        )
        .await
        .expect("create second cookbook version");

    let (_, purged) = store
        .delete_cookbook_version(&fixture.org_id, "shared", "1.0.0")
        .await
        .expect("delete first version");
    assert!(purged.is_empty());
    assert!(store
        .checksum_exists(&fixture.org_id, &checksum)
        .await
        .expect("checksum retained while referenced"));

    // A cookbook artifact also references the checksum, so deleting the last
    // cookbook version still keeps it until the artifact goes away.
    store
        .create_cookbook_artifact_version(
            &fixture.org_id,
            "shared",
            "1.0.0",
            manifest,
            &checksums,
            &fixture.creator_authz_id,
        )
        .await
        .expect("create artifact sharing checksum");
    let (_, purged) = store
        .delete_cookbook_version(&fixture.org_id, "shared", "2.0.0")
        .await
        .expect("delete second version");
    assert!(purged.is_empty());
    assert!(store
        .checksum_exists(&fixture.org_id, &checksum)
        .await
        .expect("checksum retained by artifact"));

    let (_, purged) = store
        .delete_cookbook_artifact_version(&fixture.org_id, "shared", "1.0.0")
        .await
        .expect("delete artifact");
    assert_eq!(purged, vec![checksum.clone()]);
    assert!(!store
        .checksum_exists(&fixture.org_id, &checksum)
        .await
        .expect("checksum purged"));
    // Cookbook metadata matches SQLite: NULL metadata, manifest in
    // serialized_object.
    let checksum = format!("{:032x}", 100);
    let checksums = vec![checksum];
    let (sandbox_id, _) = store
        .create_sandbox(&fixture.org_id, &checksums)
        .await
        .expect("create parity sandbox");
    store
        .commit_sandbox(&fixture.org_id, &sandbox_id)
        .await
        .expect("commit parity sandbox");
    let manifest = r#"{"name":"parity"}"#;
    let version = store
        .create_cookbook_version(
            &fixture.org_id,
            "parity",
            "1.0.0",
            manifest,
            &checksums,
            false,
            &fixture.creator_authz_id,
        )
        .await
        .expect("create parity version");
    let row =
        sqlx::query("SELECT metadata, serialized_object FROM cookbook_versions WHERE id = $1")
            .bind(&version.id)
            .fetch_one(store.pool())
            .await
            .expect("read parity row");
    assert_eq!(row.try_get::<Option<String>, _>(0).expect("metadata"), None);
    let stored = row.try_get::<String, _>(1).expect("manifest");
    assert!(stored.starts_with(COMPRESSED_PREFIX));
    assert_eq!(
        microwave_store::decode_serialized(&stored).expect("manifest decodes"),
        manifest
    );
}
