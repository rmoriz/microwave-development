//! PostgreSQL integration tests for the first metadata-store slice.
//!
//! Set `MICROWAVE_TEST_POSTGRES_URL` (or run `scripts/run-postgres-tests.sh`)
//! to execute these tests. Without a reachable database every test skips with
//! a clear message so `cargo test --workspace` stays green on developer
//! machines without Docker.

use microwave_store::{
    ActorType, ArtifactRepository, AuthzRepository, Compression, CookbookRepository, GroupMembers,
    HealthRepository, IdentityRepository, KvRepository, MembershipRepository, NewNode,
    OAuthRepository, ObjectRepository, Permission, PolicyRepository, SandboxRepository,
    SecretsRepository, StoreError, UserProfile, COMPRESSED_PREFIX,
};
use microwave_store_postgres::PostgresStore;
use sqlx::Row;
use std::sync::OnceLock;
use std::time::Duration;

fn assert_metadata_store<T: microwave_store::MetadataStore>() {}

#[test]
fn postgres_satisfies_metadata_store() {
    assert_metadata_store::<PostgresStore>();
}

#[tokio::test]
async fn postgres_health_check_runs_select_one_and_reports_closed_pools() {
    let Some(store) = test_store().await else {
        return;
    };
    store.health_check().await.expect("healthy PostgreSQL");

    let pool = store.pool().clone();
    pool.close().await;
    assert!(
        store.health_check().await.is_err(),
        "a closed pool must surface as an unhealthy metadata probe"
    );
}

fn test_url() -> Option<String> {
    std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

/// Apply the embedded schema exactly once per test binary.
///
/// Each test owns its runtime and its own pool. Running migration DDL
/// concurrently from every test can deadlock against another test's DML on
/// PostgreSQL, so the first caller migrates and later callers use
/// `connect_existing`.
fn ensure_postgres_schema(url: &str) {
    static MIGRATED: OnceLock<Result<(), String>> = OnceLock::new();
    let result = MIGRATED.get_or_init(|| {
        let url = url.to_string();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("build migration runtime: {error}"))?;
            runtime.block_on(async move {
                PostgresStore::connect(&url)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        })
        .join()
        .unwrap_or_else(|_| Err("PostgreSQL migration thread panicked".to_string()))
    });
    if let Err(error) = result {
        panic!("configured PostgreSQL migration failed: {error}");
    }
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
    ensure_postgres_schema(&url);
    Some(
        PostgresStore::connect_existing(&url)
            .await
            .expect("connect to configured PostgreSQL"),
    )
}

fn suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

async fn wait_for_blocked_reset_deletes(store: &PostgresStore, expected: i64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pg_stat_activity
                 WHERE datname = current_database()
                   AND pid <> pg_backend_pid()
                   AND state = 'active'
                   AND wait_event_type = 'Lock'
                   AND query LIKE 'DELETE FROM kvpair%'",
            )
            .fetch_one(store.pool())
            .await
            .expect("count blocked reset-token deletes");
            if blocked == expected {
                break;
            }
            assert!(
                blocked < expected,
                "expected at most {expected} blocked reset-token deletes, found {blocked}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {expected} overlapping consumers"));
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
async fn enforces_authz_entity_policy_and_artifact_constraints() {
    let Some(store) = test_store().await else {
        return;
    };

    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT indexname FROM pg_indexes WHERE schemaname = current_schema()")
            .fetch_all(store.pool())
            .await
            .expect("list indexes");
    for expected in [
        "users_lower_email_index",
        "users_external_authentication_uid_index",
        "authz_entities_org_index",
        "authz_acl_actor_actor_index",
        "authz_group_actors_actor_index",
        "sandboxed_checksums_sandbox_index",
    ] {
        assert!(
            indexes.iter().any(|name| name == expected),
            "missing index {expected}"
        );
    }

    let actor = store.create_actor().await.expect("create actor");
    let group = store
        .create_group(&format!("group-{}", suffix()))
        .await
        .expect("create group");
    let object = store.create_object().await.expect("create object");

    for (statement, left, right) in [
        (
            "INSERT INTO authz_group_actors (group_authz_id, actor_authz_id) VALUES ($1, $2)",
            group.as_str(),
            group.as_str(),
        ),
        (
            "INSERT INTO authz_group_groups
                 (parent_group_authz_id, child_group_authz_id) VALUES ($1, $2)",
            actor.as_str(),
            group.as_str(),
        ),
        (
            "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
                 VALUES ($1, 'read', $2)",
            object.as_str(),
            group.as_str(),
        ),
        (
            "INSERT INTO authz_acl_group (target_authz_id, permission, group_authz_id)
                 VALUES ($1, 'read', $2)",
            object.as_str(),
            actor.as_str(),
        ),
    ] {
        let error = sqlx::query(statement)
            .bind(left)
            .bind(right)
            .execute(store.pool())
            .await
            .expect_err("row with the wrong entity type must be rejected");
        assert!(
            error.to_string().contains("must reference"),
            "unexpected error: {error}"
        );
    }

    sqlx::query("INSERT INTO authz_group_actors (group_authz_id, actor_authz_id) VALUES ($1, $2)")
        .bind(&group)
        .bind(&actor)
        .execute(store.pool())
        .await
        .expect("valid actor membership");

    let org = store
        .create_org(&format!("integrity-{}", suffix()), "Integrity Org")
        .await
        .expect("create org");
    let creator = store.create_actor().await.expect("create creator");
    for container in ["policies", "policy_groups"] {
        store
            .create_container_for_org(&org.id, container)
            .await
            .expect("create policy container");
    }

    let artifact_id: i64 = sqlx::query_scalar(
        "INSERT INTO cookbook_artifacts (org_id, name, authz_id)
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(&org.id)
    .bind(format!("artifact-{}", suffix()))
    .bind(&object)
    .fetch_one(store.pool())
    .await
    .expect("insert artifact");
    sqlx::query(
        "INSERT INTO cookbook_artifact_versions
             (cookbook_artifact_id, org_id, identifier, metadata,
              serialized_object, created_at, created_by)
         VALUES ($1, $2, '1.0.0', NULL, NULL, $3, '')",
    )
    .bind(artifact_id)
    .bind(&org.id)
    .bind("2026-01-01T00:00:00Z")
    .execute(store.pool())
    .await
    .expect("insert artifact version");
    let error = sqlx::query("DELETE FROM cookbook_artifacts WHERE id = $1")
        .bind(artifact_id)
        .execute(store.pool())
        .await
        .expect_err("artifact with versions must be restricted");
    assert!(
        error.to_string().contains("foreign key")
            || error.to_string().contains("violates")
            || error.to_string().contains("RESTRICT"),
        "unexpected artifact delete error: {error}"
    );

    let revision = store
        .create_policy_revision(
            &org.id,
            "base",
            "1.0.0",
            r#"{"name":"base","revision_id":"1.0.0"}"#,
            &creator,
        )
        .await
        .expect("create policy revision");
    let policy_group = store
        .create_policy_group(&org.id, "prod", r#"{"name":"prod"}"#, &creator)
        .await
        .expect("create policy group");

    let error = sqlx::query(
        "INSERT INTO policy_group_revision_associations
             (id, org_id, policy_revision_id, policy_group_authz_id,
              policy_revision_name, policy_revision_revision_id, policy_group_name,
              last_updated_by, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, '', $8, $8)",
    )
    .bind(format!("bad-{}", suffix()))
    .bind(&org.id)
    .bind(&revision.id)
    .bind(&policy_group.authz_id)
    .bind(&revision.name)
    .bind(&revision.revision_id)
    .bind("missing")
    .bind("2026-01-01T00:00:00Z")
    .execute(store.pool())
    .await
    .expect_err("unknown policy group name must be rejected");
    assert!(
        error.to_string().contains("foreign key") || error.to_string().contains("violates"),
        "unexpected policy association error: {error}"
    );

    sqlx::query(
        "INSERT INTO policy_group_revision_associations
             (id, org_id, policy_revision_id, policy_group_authz_id,
              policy_revision_name, policy_revision_revision_id, policy_group_name,
              last_updated_by, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, '', $8, $8)",
    )
    .bind(format!("ok-{}", suffix()))
    .bind(&org.id)
    .bind(&revision.id)
    .bind(&policy_group.authz_id)
    .bind(&revision.name)
    .bind(&revision.revision_id)
    .bind(&policy_group.name)
    .bind("2026-01-01T00:00:00Z")
    .execute(store.pool())
    .await
    .expect("valid policy association");
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
    let usag = store
        .find_group(&org.id, &user.id)
        .await
        .expect("find user-specific group")
        .expect("user-specific group exists");
    assert!(store
        .user_in_group(&usag.authz_id, &user.authz_id)
        .await
        .expect("user is in own USAG"));
    let members = store
        .list_group_members(&org.id, &users_group.authz_id)
        .await
        .expect("list users group members");
    assert!(members.users.is_empty());
    assert_eq!(members.groups, vec![user.id.clone()]);

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
    assert!(store
        .find_group(&org.id, &user.id)
        .await
        .expect("find removed USAG")
        .is_none());
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
    // The unrelated grant survives; the user's own default ACE also remains,
    // matching the SQLite backend's user-creation ACL.
    assert!(update.users.contains(&other.username));
    assert!(update.users.contains(&user.username));

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
    assert!(store
        .find_group(&org.id, &other.id)
        .await
        .expect("find invited USAG")
        .is_some());
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
    sqlx::query("UPDATE kvpair SET updated_at = '2000-01-01T00:00:00Z' WHERE key = $1")
        .bind(format!("id_session:{session}"))
        .execute(store.pool())
        .await
        .expect("age id session");
    assert_eq!(
        store
            .find_id_session(&session)
            .await
            .expect("expired session"),
        None
    );
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM kvpair WHERE key = $1")
        .bind(format!("id_session:{session}"))
        .fetch_one(store.pool())
        .await
        .expect("count expired session");
    assert_eq!(remaining, 0);
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
        store.consume_password_reset_token(&concurrent, 3600),
        store.consume_password_reset_token(&concurrent, 3600)
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
            .find_password_reset_token(&concurrent, 3600)
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
            .find_password_reset_token(&reset, 3600)
            .await
            .expect("find reset token"),
        Some("alice".into())
    );
    assert_eq!(
        store
            .consume_password_reset_token(&reset, 3600)
            .await
            .expect("consume reset token"),
        Some("alice".into())
    );
    assert_eq!(
        store
            .find_password_reset_token(&reset, 3600)
            .await
            .expect("consumed reset token"),
        None
    );

    // Expired password-reset tokens must not be findable or consumable,
    // and the row must be removed.
    let aged = store
        .create_password_reset_token("expired-alice")
        .await
        .expect("create aged reset token");
    sqlx::query("UPDATE kvpair SET updated_at = '2000-01-01T00:00:00Z' WHERE key = $1")
        .bind(format!("id_password_reset:{aged}"))
        .execute(store.pool())
        .await
        .expect("age reset token");
    assert_eq!(
        store
            .find_password_reset_token(&aged, 3600)
            .await
            .expect("expired reset token"),
        None
    );
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM kvpair WHERE key = $1")
        .bind(format!("id_password_reset:{aged}"))
        .fetch_one(store.pool())
        .await
        .expect("count expired reset token");
    assert_eq!(remaining, 0);
    assert_eq!(
        store
            .consume_password_reset_token(&aged, 3600)
            .await
            .expect("consume expired reset token"),
        None,
        "expired token must not be consumable"
    );
}

#[tokio::test]
async fn postgres_snapshots_exclude_password_reset_rows_in_every_export_mode() {
    let Some(store) = test_store().await else {
        return;
    };
    let username = format!("reset-user-{}", suffix());
    let token = store
        .create_password_reset_token(&username)
        .await
        .expect("create reset token");
    let ordinary_key = format!("ordinary-{}", suffix());
    store.set_kv(&ordinary_key, "ordinary-value").await.unwrap();

    for include_secrets in [false, true] {
        let snapshot = store
            .export_snapshot_with_options(include_secrets)
            .await
            .expect("export snapshot");
        assert!(
            snapshot
                .kvpair
                .iter()
                .all(|row| !row.key.starts_with("id_password_reset:")),
            "password-reset row exported with include_secrets={include_secrets}"
        );
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains(&token));
        assert!(!serialized.contains(&username));
        if include_secrets {
            assert!(snapshot.kvpair.iter().any(|row| row.key == ordinary_key));
        }
    }
}

#[tokio::test]
async fn overlapping_postgres_consumers_use_a_reset_token_exactly_once() {
    let Some(store) = test_store().await else {
        return;
    };
    let token = store
        .create_password_reset_token("overlap-user")
        .await
        .expect("create reset token");
    let reset_key = format!("id_password_reset:{token}");
    let id = suffix();
    let function_name = format!("block_reset_delete_{id}");
    let trigger_name = format!("block_reset_delete_trigger_{id}");
    let lock_key = (uuid::Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    let ddl = format!(
        "CREATE FUNCTION {function_name}() RETURNS trigger
         LANGUAGE plpgsql AS $function$
         BEGIN
             PERFORM pg_advisory_xact_lock({lock_key});
             RETURN OLD;
         END;
         $function$;
         CREATE TRIGGER {trigger_name}
         BEFORE DELETE ON kvpair
         FOR EACH ROW
         WHEN (OLD.key = '{reset_key}')
         EXECUTE FUNCTION {function_name}();"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(ddl.as_str()))
        .execute(store.pool())
        .await
        .expect("install deterministic consumer barrier");

    // Hold the advisory lock on one dedicated session. Each consumer must
    // reach its DELETE (and therefore overlap) before this lock is released.
    let mut guard = store.pool().acquire().await.expect("acquire lock session");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(lock_key)
        .execute(&mut *guard)
        .await
        .expect("hold consumer barrier");

    let first_store = store.clone();
    let first_token = token.clone();
    let first = tokio::spawn(async move {
        first_store
            .consume_password_reset_token(&first_token, 3600)
            .await
    });
    wait_for_blocked_reset_deletes(&store, 1).await;

    let second_store = store.clone();
    let second_token = token.clone();
    let second = tokio::spawn(async move {
        second_store
            .consume_password_reset_token(&second_token, 3600)
            .await
    });
    wait_for_blocked_reset_deletes(&store, 2).await;

    let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(lock_key)
        .fetch_one(&mut *guard)
        .await
        .expect("release consumer barrier");
    assert!(unlocked);

    let (left, right) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(first, second)
    })
    .await
    .expect("consumers finish after barrier release");
    let results = [
        left.expect("first consumer task").expect("first consume"),
        right
            .expect("second consumer task")
            .expect("second consume"),
    ];

    let cleanup =
        format!("DROP TRIGGER {trigger_name} ON kvpair; DROP FUNCTION {function_name}();");
    sqlx::raw_sql(sqlx::AssertSqlSafe(cleanup.as_str()))
        .execute(store.pool())
        .await
        .expect("remove deterministic consumer barrier");

    assert_eq!(
        results.iter().filter(|value| value.is_some()).count(),
        1,
        "exactly one overlapping consumer must win: {results:?}"
    );
    assert!(results.contains(&Some("overlap-user".into())));
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
async fn deleting_org_removes_its_read_access_global_group_and_acl_rows() {
    let Some(store) = test_store().await else {
        return;
    };
    let org = store
        .create_org(&format!("delete-org-{}", suffix()), "Delete Org")
        .await
        .expect("create org");
    let read_access_name = format!("{}_read_access_group", org.name);
    let read_access = store
        .ensure_global_group(&read_access_name)
        .await
        .expect("create read access group");
    let child = store
        .create_group_for_org(&org.id, "admins")
        .await
        .expect("create child group");
    store
        .add_group_child(&read_access.authz_id, &child.authz_id)
        .await
        .expect("nest child group");
    assert!(!store
        .get_acl_entries(&read_access.authz_id)
        .await
        .expect("read access ACL rows")
        .is_empty());

    store.delete_org(&org.id).await.expect("delete org");

    assert!(store
        .find_org(&org.name)
        .await
        .expect("find deleted org")
        .is_none());
    assert!(store
        .find_global_group(&read_access_name)
        .await
        .expect("find deleted read access group")
        .is_none());
    assert!(!store
        .list_global_groups()
        .await
        .expect("list global groups")
        .iter()
        .any(|group| group.name == read_access_name));
    match store.get_acl_entries(&read_access.authz_id).await {
        Ok(entries) => assert!(entries.is_empty(), "ACL rows survived: {entries:?}"),
        Err(StoreError::NotFound(_)) => {}
        Err(error) => panic!("ACL rows after org deletion: {error}"),
    }
}

#[tokio::test]
async fn org_deletion_rolls_back_when_global_group_cleanup_fails() {
    let Some(store) = test_store().await else {
        return;
    };
    let org = store
        .create_org(&format!("rollback-org-{}", suffix()), "Rollback Org")
        .await
        .expect("create org");
    let read_access_name = format!("{}_read_access_group", org.name);
    store
        .ensure_global_group(&read_access_name)
        .await
        .expect("create read access group");
    store
        .ensure_global_group("server-admins")
        .await
        .expect("ensure server admins");
    let suffix = suffix();
    let function = format!("fail_read_group_{suffix}");
    let trigger = format!("fail_read_group_trigger_{suffix}");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE OR REPLACE FUNCTION {function}() RETURNS trigger AS $$
         BEGIN
             RAISE EXCEPTION 'injected read-access group delete failure';
         END;
         $$ LANGUAGE plpgsql"
    )))
    .execute(store.pool())
    .await
    .expect("create failure-injection function");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER {trigger}
         BEFORE DELETE ON global_groups
         FOR EACH ROW WHEN (OLD.name = '{read_access_name}')
         EXECUTE FUNCTION {function}()"
    )))
    .execute(store.pool())
    .await
    .expect("create failure-injection trigger");

    let error = store
        .delete_org(&org.id)
        .await
        .expect_err("injected global-group failure must abort delete_org");
    assert!(
        error
            .to_string()
            .contains("injected read-access group delete failure"),
        "unexpected error: {error}"
    );
    assert_eq!(
        store
            .find_org(&org.name)
            .await
            .expect("find org after rollback"),
        Some(org.clone()),
        "organization deletion must roll back"
    );
    assert!(
        store
            .find_global_group(&read_access_name)
            .await
            .expect("find read group after rollback")
            .is_some(),
        "read access group must survive the rollback"
    );
    assert!(
        store
            .find_global_group("server-admins")
            .await
            .expect("find server admins")
            .is_some(),
        "server-admins must not be touched"
    );

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP TRIGGER {trigger} ON global_groups"
    )))
    .execute(store.pool())
    .await
    .expect("drop failure-injection trigger");
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP FUNCTION {function}()")))
        .execute(store.pool())
        .await
        .expect("drop failure-injection function");

    store
        .delete_org(&org.id)
        .await
        .expect("delete org after rollback");
    assert!(store
        .find_org(&org.name)
        .await
        .expect("find deleted org")
        .is_none());
    assert!(store
        .find_global_group(&read_access_name)
        .await
        .expect("find deleted read group")
        .is_none());
    assert!(store
        .find_global_group("server-admins")
        .await
        .expect("find server admins after delete")
        .is_some());
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
