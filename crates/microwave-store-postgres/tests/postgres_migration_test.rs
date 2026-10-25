//! Snapshot export/import coverage for the PostgreSQL backend.
//!
//! Set `MICROWAVE_TEST_POSTGRES_URL` (or run `scripts/run-postgres-tests.sh`)
//! to execute this test. Without a configured database it skips with a clear
//! message. A configured database that cannot be reached is a hard failure.

use microwave_store::{
    ActorType, ArtifactRepository, AuthzRepository, CookbookRepository, IdentityRepository,
    MembershipRepository, NewNode, OAuthRepository, ObjectRepository, PolicyRepository,
    SandboxRepository,
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
    store
        .create_invitation(&org.id, &user.id, &creator)
        .await
        .unwrap();
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
        .create_cookbook_artifact_version(
            &org.id,
            &format!("artifact-{suffix}"),
            "identifier-1",
            r#"{"name":"artifact"}"#,
            std::slice::from_ref(&checksum),
            &creator,
        )
        .await
        .unwrap();
    let policy_name = format!("policy-{suffix}");
    let policy_body =
        format!(r#"{{"name":"{policy_name}","revision_id":"revision-1","run_list":[]}}"#);
    store
        .create_policy_revision(&org.id, &policy_name, "revision-1", &policy_body, &creator)
        .await
        .unwrap();
    store
        .associate_policy_revision(
            &org.id,
            &format!("group-{suffix}"),
            &policy_name,
            "revision-1",
            &policy_body,
            &format!(r#"{{"name":"group-{suffix}"}}"#),
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
        .cookbook_artifact_versions
        .iter()
        .any(|row| row.identifier == "identifier-1"));
    assert!(before
        .org_user_invites
        .iter()
        .any(|row| row.user_id == user.id));
    assert!(before
        .policy_group_revision_associations
        .iter()
        .any(|row| row.policy_revision_name == policy_name));
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

    // Import into a *fresh* database. A missing sequence reset would still pass
    // the same-database test above because the original inserts already
    // advanced the sequence, so this is the probative check.
    let (scratch_name, scratch_url) = create_scratch_database(&url, &suffix).await;
    let target = PostgresStore::connect(&scratch_url)
        .await
        .expect("connect to fresh snapshot target");
    target.import_snapshot(&before).await.unwrap();
    let target_snapshot = target.export_snapshot().await.unwrap();
    assert_eq!(before, target_snapshot);

    for table in [
        "cookbooks",
        "cookbook_artifacts",
        "cookbook_artifact_versions",
        "oauth_applications",
    ] {
        let max: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COALESCE(MAX(id), 0) FROM {table}"
        )))
        .fetch_one(target.pool())
        .await
        .expect("read imported max id");
        assert!(
            max > 0,
            "{table} should contain an imported snapshot row for this check"
        );
        let next: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT nextval(pg_get_serial_sequence('{table}', 'id'))"
        )))
        .fetch_one(target.pool())
        .await
        .expect("read next sequence value");
        assert!(
            next > max,
            "{table} sequence must advance beyond imported max id {max}, got {next}"
        );
    }

    // A normal insert after import must not collide with an imported id.
    let next = target
        .create_oauth_application(
            &format!("app-next-{suffix}"),
            "https://example.com/next",
            "",
            true,
        )
        .await
        .unwrap();
    assert!(next.id > application.id);
    drop(target);
    drop_scratch_database(&url, &scratch_name).await;
}

fn database_url_with_name(url: &str, name: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, format!("?{query}")),
        None => (url, String::new()),
    };
    let prefix = match base.rfind('/') {
        Some(index) => &base[..=index],
        None => base,
    };
    format!("{prefix}{name}{query}")
}

async fn create_scratch_database(base_url: &str, suffix: &str) -> (String, String) {
    let name = format!("snapshot_{suffix}").replace('-', "_");
    let pool = sqlx::PgPool::connect(base_url)
        .await
        .expect("connect to configured PostgreSQL");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&pool)
        .await
        .expect("create scratch database");
    pool.close().await;
    let url = database_url_with_name(base_url, &name);
    (name, url)
}

async fn drop_scratch_database(base_url: &str, name: &str) {
    let pool = sqlx::PgPool::connect(base_url)
        .await
        .expect("connect to configured PostgreSQL");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE \"{name}\" WITH (FORCE)"
    )))
    .execute(&pool)
    .await
    .expect("drop scratch database");
    pool.close().await;
}
