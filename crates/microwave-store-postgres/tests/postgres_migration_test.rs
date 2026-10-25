//! Snapshot export/import coverage for the PostgreSQL backend.
//!
//! Set `MICROWAVE_TEST_POSTGRES_URL` (or run `scripts/run-postgres-tests.sh`)
//! to execute this test. Without a configured database it skips with a clear
//! message. A configured database that cannot be reached is a hard failure.

use microwave_store::{
    ActorType, AuthzRepository, CookbookRepository, IdentityRepository, NewNode, OAuthRepository,
    ObjectRepository, PolicyRepository, SandboxRepository,
};
use microwave_store_postgres::PostgresStore;

fn test_url() -> Option<String> {
    std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

fn suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

#[tokio::test]
async fn snapshot_round_trips_every_seeded_collection() {
    let Some(url) = test_url() else {
        eprintln!(
            "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; start a disposable \
             PostgreSQL with scripts/run-postgres-tests.sh"
        );
        return;
    };
    let store = PostgresStore::connect(&url)
        .await
        .expect("connect to configured PostgreSQL");
    let suffix = suffix();
    let org_name = format!("snapshot-org-{suffix}");

    let org = store.create_org(&org_name, "Snapshot Org").await.unwrap();
    let creator = store.create_actor().await.unwrap();
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
            .create_container_for_org(&org.id, container)
            .await
            .unwrap();
    }
    store
        .create_group_for_org_with_acl(&org.id, "admins", &creator)
        .await
        .unwrap();
    let user = store
        .create_user(
            &format!("snapshot-user-{suffix}"),
            &format!("snapshot-user-{suffix}@example.com"),
            "bcrypt",
            "snapshot-hash",
            "snapshot-salt",
            true,
        )
        .await
        .unwrap();
    store.associate_user_org(&org.id, &user.id).await.unwrap();
    let client = store
        .create_client(&org.id, &format!("client-{suffix}"), false, false)
        .await
        .unwrap();
    store
        .create_actor_key(
            &client.id,
            ActorType::Client,
            "snapshot-key",
            "-----BEGIN PUBLIC KEY-----\nsnapshot\n-----END PUBLIC KEY-----",
            "infinity",
        )
        .await
        .unwrap();

    let node_name = format!("node-{suffix}");
    store
        .create_node(
            &org.id,
            NewNode {
                name: &node_name,
                body: r#"{"name":"node","chef_environment":"_default"}"#,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &creator,
            },
        )
        .await
        .unwrap();
    store
        .create_environment(&org.id, "staging", r#"{"name":"staging"}"#, &creator)
        .await
        .unwrap();
    store
        .create_role(&org.id, "web", r#"{"name":"web"}"#, &creator)
        .await
        .unwrap();
    store
        .create_data_bag(&org.id, "secrets", &creator)
        .await
        .unwrap();
    store
        .create_data_bag_item(
            &org.id,
            "secrets",
            "api",
            r#"{"id":"api","token":"snapshot"}"#,
        )
        .await
        .unwrap();

    let checksum = format!("{:032x}", suffix.len());
    let (sandbox_id, _) = store
        .create_sandbox(&org.id, std::slice::from_ref(&checksum))
        .await
        .unwrap();
    store.commit_sandbox(&org.id, &sandbox_id).await.unwrap();
    store
        .create_cookbook_version(
            &org.id,
            &format!("cookbook-{suffix}"),
            "1.0.0",
            r#"{"name":"cookbook"}"#,
            std::slice::from_ref(&checksum),
            false,
            &creator,
        )
        .await
        .unwrap();
    store
        .create_policy_revision(
            &org.id,
            &format!("policy-{suffix}"),
            "revision-1",
            r#"{"name":"policy","revision_id":"revision-1"}"#,
            &creator,
        )
        .await
        .unwrap();
    let application = store
        .create_oauth_application(
            &format!("app-{suffix}"),
            "https://example.com/callback",
            "",
            true,
        )
        .await
        .unwrap();

    let before = store.export_snapshot().await.unwrap();
    assert!(before.orgs.iter().any(|row| row.name == org_name));
    assert!(before.users.iter().any(|row| row.id == user.id));
    assert!(before
        .cookbook_versions
        .iter()
        .any(|row| row.name.starts_with("cookbook-")));
    assert!(before
        .oauth_applications
        .iter()
        .any(|row| row.id == application.id));

    // Importing the same snapshot twice must be idempotent and preserve every
    // source id, timestamp, hash, and key.
    store.import_snapshot(&before).await.unwrap();
    store.import_snapshot(&before).await.unwrap();
    let after = store.export_snapshot().await.unwrap();
    assert_eq!(before, after);

    // Explicit ids in the snapshot must not leave sequence-backed tables
    // behind. This insert would fail with a duplicate id otherwise.
    let next = store
        .create_oauth_application(
            &format!("app-next-{suffix}"),
            "https://example.com/next",
            "",
            true,
        )
        .await
        .unwrap();
    assert!(next.id > application.id);
}
