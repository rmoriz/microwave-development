//! Snapshot export/import coverage for the PostgreSQL backend.
//!
//! Set `MICROWAVE_TEST_POSTGRES_URL` (or run `scripts/run-postgres-tests.sh`)
//! to execute this test. Without a configured database it skips with a clear
//! message. A configured database that cannot be reached is a hard failure.

use microwave_store::{
    ActorType, ArtifactRepository, AuthzRepository, Compression, CookbookRepository,
    IdentityRepository, KvRepository, MembershipRepository, NewNode, OAuthRepository,
    ObjectRepository, PolicyRepository, SandboxRepository, SecretsRepository, COMPRESSED_PREFIX,
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
    let grant = store
        .create_oauth_access_grant(
            &format!("owner-{suffix}"),
            application.id,
            "https://example.com/callback",
            "openid",
            300,
        )
        .await
        .unwrap();
    let access_token = store
        .create_oauth_access_token(
            &format!("owner-{suffix}"),
            application.id,
            "openid",
            Some(3600),
            true,
        )
        .await
        .unwrap();
    store
        .set_kv(&format!("snapshot:flag-{suffix}"), "snapshot-value")
        .await
        .unwrap();
    store
        .set_secret("bookshelf", "secret_access_key", "snapshot-secret")
        .await
        .unwrap();
    store.set_maintenance_enabled(true).await.unwrap();
    let (empty_sandbox, _) = store.create_sandbox(&org.id, &[]).await.unwrap();
    sqlx::query("INSERT INTO telemetry (property, event_timestamp) VALUES ($1, $2)")
        .bind(format!("snapshot-{suffix}"))
        .bind("2026-10-25T00:00:00Z")
        .execute(store.pool())
        .await
        .unwrap();

    let before = store.export_snapshot().await.unwrap();
    assert!(before
        .kvpair
        .iter()
        .any(|row| row.key == format!("snapshot:flag-{suffix}")
            && row.value.as_deref() == Some("snapshot-value")));
    assert!(before
        .oauth_access_grants
        .iter()
        .any(|row| row.token == grant.token));
    assert!(before
        .oauth_access_tokens
        .iter()
        .any(|row| row.token == access_token.token));
    assert!(before
        .sandboxes
        .iter()
        .any(|row| row.sandbox_id == empty_sandbox));
    assert!(before
        .telemetry
        .iter()
        .any(|row| row.property == format!("snapshot-{suffix}")));

    // A safe export is a separate view: it must not disturb the store and it
    // must omit every secret-bearing row/value.
    let safe = store.export_snapshot_with_options(false).await.unwrap();
    assert!(safe.kvpair.is_empty());
    assert!(safe.oauth_access_grants.is_empty());
    assert!(safe.oauth_access_tokens.is_empty());
    assert!(safe
        .oauth_applications
        .iter()
        .filter(|row| row.id == application.id)
        .all(|row| row.secret.is_empty()));
    assert!(safe
        .sandboxes
        .iter()
        .any(|row| row.sandbox_id == empty_sandbox));
    assert!(safe
        .telemetry
        .iter()
        .any(|row| row.property == format!("snapshot-{suffix}")));
    assert!(before.orgs.iter().any(|row| row.name == org_name));
    let exported_node = before
        .nodes
        .iter()
        .find(|row| row.name == node_name)
        .expect("node is in the snapshot");
    assert_eq!(
        exported_node.serialized_object.as_deref(),
        Some(r#"{"name":"node","chef_environment":"_default"}"#)
    );
    assert!(before
        .cookbook_artifact_versions
        .iter()
        .find(|row| row.identifier == "identifier-1")
        .unwrap()
        .serialized_object
        .as_deref()
        .is_some_and(|value| !value.starts_with(COMPRESSED_PREFIX)));
    let raw_node: String =
        sqlx::query_scalar("SELECT serialized_object FROM nodes WHERE org_id = $1 AND name = $2")
            .bind(&org.id)
            .bind(&node_name)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert!(raw_node.starts_with(COMPRESSED_PREFIX));
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
    assert_eq!(
        target
            .get_secret("bookshelf", "secret_access_key")
            .await
            .unwrap()
            .as_deref(),
        Some("snapshot-secret")
    );
    assert!(target.maintenance_enabled().await.unwrap());
    assert_eq!(
        target
            .find_oauth_access_grant(&grant.token)
            .await
            .unwrap()
            .unwrap()
            .id,
        grant.id
    );
    assert_eq!(
        target
            .find_oauth_access_token(&access_token.token)
            .await
            .unwrap()
            .unwrap()
            .id,
        access_token.id
    );
    assert!(target
        .sandbox_exists(&org.id, &empty_sandbox)
        .await
        .unwrap());
    let telemetry_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM telemetry WHERE property = $1")
            .bind(format!("snapshot-{suffix}"))
            .fetch_one(target.pool())
            .await
            .unwrap();
    assert_eq!(telemetry_count, 1);

    for table in [
        "cookbooks",
        "cookbook_artifacts",
        "cookbook_artifact_versions",
        "oauth_applications",
        "oauth_access_grants",
        "oauth_access_tokens",
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
    // Maintenance mode is global to the shared test database; restore it so
    // later HTTP runtime/CLI suites do not see 503 responses.
    store.set_maintenance_enabled(false).await.unwrap();
}

#[tokio::test]
async fn safe_snapshot_import_restores_metadata_without_secrets() {
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
    let org = store
        .create_org(&format!("safe-org-{suffix}"), "Safe Org")
        .await
        .unwrap();
    let application = store
        .create_oauth_application(
            &format!("safe-app-{suffix}"),
            "https://example.com/safe",
            "",
            true,
        )
        .await
        .unwrap();
    let grant = store
        .create_oauth_access_grant(
            &format!("safe-owner-{suffix}"),
            application.id,
            "https://example.com/safe",
            "",
            300,
        )
        .await
        .unwrap();
    let access_token = store
        .create_oauth_access_token(
            &format!("safe-owner-{suffix}"),
            application.id,
            "",
            Some(300),
            false,
        )
        .await
        .unwrap();
    store
        .set_secret("bookshelf", "secret_access_key", "safe-secret")
        .await
        .unwrap();
    let (sandbox, _) = store.create_sandbox(&org.id, &[]).await.unwrap();

    let snapshot = store.export_snapshot_with_options(false).await.unwrap();
    let (scratch_name, scratch_url) = create_scratch_database(&url, &suffix).await;
    let target = PostgresStore::connect(&scratch_url)
        .await
        .expect("connect to safe snapshot target");
    let local_telemetry = format!("local-telemetry-{suffix}");
    sqlx::query("INSERT INTO telemetry (property, event_timestamp) VALUES ($1, $2)")
        .bind(&local_telemetry)
        .bind("2026-10-24T00:00:00Z")
        .execute(target.pool())
        .await
        .unwrap();
    target.import_snapshot(&snapshot).await.unwrap();
    let local_telemetry_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM telemetry WHERE property = $1")
            .bind(&local_telemetry)
            .fetch_one(target.pool())
            .await
            .unwrap();
    assert_eq!(local_telemetry_count, 1);

    assert!(target
        .get_secret("bookshelf", "secret_access_key")
        .await
        .unwrap()
        .is_none());
    assert!(target
        .find_oauth_access_grant(&grant.token)
        .await
        .unwrap()
        .is_none());
    assert!(target
        .find_oauth_access_token(&access_token.token)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        target
            .find_oauth_application(application.id)
            .await
            .unwrap()
            .unwrap()
            .secret,
        ""
    );
    assert!(target.sandbox_exists(&org.id, &sandbox).await.unwrap());

    drop(target);
    drop_scratch_database(&url, &scratch_name).await;
}

#[tokio::test]
async fn imports_snapshot_with_compression_none() {
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
    let org = store
        .create_org(&format!("plain-import-org-{suffix}"), "Plain Import Org")
        .await
        .unwrap();
    let creator = store.create_actor().await.unwrap();
    for container in ["nodes"] {
        store
            .create_container_for_org(&org.id, container)
            .await
            .unwrap();
    }
    let name = format!("plain-import-node-{suffix}");
    let body = format!(r#"{{"name":"{name}"}}"#);
    store
        .create_node(
            &org.id,
            NewNode {
                name: &name,
                body: &body,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &creator,
            },
        )
        .await
        .unwrap();
    let snapshot = store.export_snapshot().await.unwrap();

    let (scratch_name, scratch_url) = create_scratch_database(&url, &suffix).await;
    let target = PostgresStore::connect_with_compression(&scratch_url, Compression::None)
        .await
        .expect("connect to plain-import target");
    target.import_snapshot(&snapshot).await.unwrap();

    let raw: String =
        sqlx::query_scalar("SELECT serialized_object FROM nodes WHERE org_id = $1 AND name = $2")
            .bind(&org.id)
            .bind(&name)
            .fetch_one(target.pool())
            .await
            .expect("read raw imported value");
    assert_eq!(raw, body);
    assert!(!raw.starts_with(COMPRESSED_PREFIX));
    let node = target
        .find_node(&org.id, &name)
        .await
        .unwrap()
        .expect("imported node exists");
    assert_eq!(node.serialized_object, body);

    drop(target);
    drop_scratch_database(&url, &scratch_name).await;
}

#[tokio::test]
async fn migration_adds_the_environment_constraints_column() {
    let Some(url) = test_url() else {
        eprintln!(
            "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; start a disposable \
             PostgreSQL with scripts/run-postgres-tests.sh"
        );
        return;
    };
    let suffix = suffix();

    // A fresh database gets the column straight from the embedded schema.
    let (name, fresh_url) = create_scratch_database(&url, &format!("{suffix}-fresh")).await;
    let store = PostgresStore::connect(&fresh_url)
        .await
        .expect("migrate fresh scratch database");
    assert!(
        environment_constraints_column_exists(store.pool()).await,
        "fresh database is missing environments.cookbook_constraints"
    );

    // A database created before the column existed gains it on the next
    // migration run, which must stay idempotent.
    sqlx::query("ALTER TABLE environments DROP COLUMN cookbook_constraints")
        .execute(store.pool())
        .await
        .expect("drop the column to simulate an older database");
    assert!(!environment_constraints_column_exists(store.pool()).await);
    store.migrate().await.expect("re-run the migration");
    assert!(
        environment_constraints_column_exists(store.pool()).await,
        "re-running the migration did not restore the column"
    );
    store.migrate().await.expect("migration stays idempotent");

    drop(store);
    drop_scratch_database(&url, &name).await;
}

async fn environment_constraints_column_exists(pool: &sqlx::PgPool) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM information_schema.columns
             WHERE table_schema = current_schema()
               AND table_name = 'environments'
               AND column_name = 'cookbook_constraints'
         )",
    )
    .fetch_one(pool)
    .await
    .expect("query the environment columns")
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
