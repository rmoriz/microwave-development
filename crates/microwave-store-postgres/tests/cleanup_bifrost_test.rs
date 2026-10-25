//! PostgreSQL coverage for `cleanup-bifrost`.
//!
//! Set `MICROWAVE_TEST_POSTGRES_URL` (or run `scripts/run-postgres-tests.sh`)
//! to execute this test. Without a reachable database it skips with a clear
//! message so `cargo test --workspace` stays green on developer machines.
//!
//! Unlike the shared-schema integration tests, this test creates a private
//! schema and points its pool at it. That keeps the global cleanup assertion
//! (`delete` returns exactly one row) deterministic even when other test
//! binaries run concurrently against the same database.

use microwave_store::{AuthzRepository, IdentityRepository, NewNode, ObjectRepository, Permission};
use microwave_store_postgres::PostgresStore;
use sqlx::postgres::PgPoolOptions;
use sqlx::AssertSqlSafe;

fn test_url() -> Option<String> {
    std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

#[tokio::test]
async fn cleanup_bifrost_removes_only_unreferenced_entities() {
    let Some(url) = test_url() else {
        eprintln!(
            "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; start a disposable \
             PostgreSQL with scripts/run-postgres-tests.sh"
        );
        return;
    };

    let schema = format!(
        "cleanup_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    );
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect to configured PostgreSQL");
    sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"")))
        .execute(&admin)
        .await
        .expect("create isolated test schema");

    let store = {
        let schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _metadata| {
                let statement = format!("SET search_path TO \"{schema}\"");
                Box::pin(async move {
                    sqlx::query(AssertSqlSafe(statement))
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .expect("connect to isolated test schema");
        let store = PostgresStore::from_pool(pool);
        store
            .migrate()
            .await
            .expect("apply schema in isolated test schema");
        store
    };

    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..12];
    let org = store
        .create_org(&format!("org-{suffix}"), "Cleanup Org")
        .await
        .expect("create org");
    let user = store
        .create_user(
            &format!("user-{suffix}"),
            &format!("user-{suffix}@example.test"),
            "bcrypt",
            "$2b$12$hash",
            "salt",
            false,
        )
        .await
        .expect("create user");
    let client = store
        .create_client(&org.id, &format!("client-{suffix}"), false, false)
        .await
        .expect("create client");
    let group = store
        .create_group_for_org(&org.id, "admins")
        .await
        .expect("create group");
    store
        .create_container_for_org(&org.id, "nodes")
        .await
        .expect("create nodes container");
    let node = store
        .create_node(
            &org.id,
            NewNode {
                name: "node1",
                body: "{}",
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &user.authz_id,
            },
        )
        .await
        .expect("create node");
    let orphan_authz_id = store.create_actor().await.expect("create orphan actor");

    assert_eq!(
        store.count_orphan_authz_entities().await.unwrap(),
        1,
        "only the unreferenced actor is an orphan"
    );
    for authz_id in [
        &user.authz_id,
        &client.authz_id,
        &group.authz_id,
        &node.authz_id,
    ] {
        store
            .get_acl_entries(authz_id)
            .await
            .unwrap_or_else(|error| panic!("referenced authz entity {authz_id}: {error}"));
    }

    // A dangling grant on the orphan must not keep it alive and must be
    // removed with it.
    store
        .set_acl(
            &orphan_authz_id,
            Permission::Read,
            Some(&user.authz_id),
            None,
        )
        .await
        .expect("grant read on orphan target");
    assert_eq!(store.count_orphan_authz_entities().await.unwrap(), 1);

    assert_eq!(store.delete_orphan_authz_entities().await.unwrap(), 1);
    assert_eq!(store.count_orphan_authz_entities().await.unwrap(), 0);
    assert!(
        store.get_acl_entries(&orphan_authz_id).await.is_err(),
        "the orphan entity must be gone"
    );
    let acl_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authz_acl_actor
         WHERE target_authz_id = $1 OR actor_authz_id = $1",
    )
    .bind(&orphan_authz_id)
    .fetch_one(store.pool())
    .await
    .expect("count leftover ACL rows");
    assert_eq!(
        acl_rows, 0,
        "ACL rows pointing at an orphan must be removed"
    );

    // Referenced entities and their rows are untouched.
    assert!(store.find_org(&org.name).await.unwrap().is_some());
    assert!(store.find_user(&user.username).await.unwrap().is_some());
    assert!(store
        .find_client(&org.id, &client.name)
        .await
        .unwrap()
        .is_some());
    assert!(store
        .find_group(&org.id, &group.name)
        .await
        .unwrap()
        .is_some());
    assert!(store
        .find_node(&org.id, &node.name)
        .await
        .unwrap()
        .is_some());

    // A second run is idempotent in the isolated schema.
    assert_eq!(store.delete_orphan_authz_entities().await.unwrap(), 0);

    store.pool().close().await;
    sqlx::query(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE")))
        .execute(&admin)
        .await
        .expect("drop isolated test schema");
    admin.close().await;
}
