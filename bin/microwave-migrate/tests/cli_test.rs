use assert_cmd::Command;
use microwave_store::SqliteStore;
use std::path::Path;
use std::process::Output;

fn migrate() -> Command {
    Command::cargo_bin("microwave-migrate").unwrap()
}

fn run(args: &[&str]) -> Output {
    migrate().args(args).output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn database(dir: &Path) -> std::path::PathBuf {
    dir.join("microwave.db")
}

#[test]
fn export_and_import_round_trip_through_the_cli() {
    let source = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(source.path())).unwrap();
    let user = store
        .create_user(
            "legacy",
            "legacy@example.com",
            "bcrypt",
            "legacy-hash",
            "legacy-salt",
            true,
        )
        .unwrap();
    drop(store);

    let snapshots = tempfile::tempdir().unwrap();
    let snapshot = snapshots.path().join("migration.json");
    let exported = run(&[
        "export",
        "--data-dir",
        source.path().to_str().unwrap(),
        "--output",
        snapshot.to_str().unwrap(),
    ]);
    assert!(
        exported.status.success(),
        "export failed: {}",
        stderr(&exported)
    );
    assert!(stdout(&exported).contains("Exported snapshot"));
    assert!(snapshot.is_file());

    let target = tempfile::tempdir().unwrap();
    let refused = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--input",
        snapshot.to_str().unwrap(),
    ]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));
    assert!(!database(target.path()).exists());

    let imported = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--input",
        snapshot.to_str().unwrap(),
        "--yes",
    ]);
    assert!(
        imported.status.success(),
        "import failed: {}",
        stderr(&imported)
    );
    assert!(stdout(&imported).contains("Imported snapshot"));

    let restored = SqliteStore::open(&database(target.path())).unwrap();
    let restored_user = restored.find_user("legacy").unwrap().unwrap();
    assert_eq!(restored_user.id, user.id);
    assert_eq!(restored_user.password_hash, "legacy-hash");
    assert!(restored_user.admin);
}

#[test]
fn import_rejects_an_unsupported_snapshot_version() {
    let snapshots = tempfile::tempdir().unwrap();
    let snapshot = snapshots.path().join("future.json");
    std::fs::write(
        &snapshot,
        r#"{"format":"microwave-server-snapshot","version":2,"generated_at":"2026-10-24T00:00:00Z","source":{"kind":"local-store"},"data":{}}"#,
    )
    .unwrap();

    let target = tempfile::tempdir().unwrap();
    let output = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--input",
        snapshot.to_str().unwrap(),
        "--yes",
    ]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("unsupported snapshot version 2"));
    assert!(!database(target.path()).exists());
}

#[test]
fn from_api_reports_a_missing_key_without_writing_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("snapshot.json");
    let result = run(&[
        "from-api",
        "--url",
        "https://chef.example.com",
        "--client-name",
        "migration",
        "--key",
        dir.path().join("migration.pem").to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    let message = stderr(&result);
    assert!(message.contains("migration.pem"));
    assert!(!output.exists());
}

#[test]
fn from_api_help_lists_org_and_api_version() {
    let output = run(&["from-api", "--help"]);
    assert!(output.status.success());
    let message = stdout(&output);
    assert!(message.contains("--org"));
    assert!(message.contains("--api-version"));
    assert!(message.contains("Crawl a live legacy Chef Infra Server"));
}

// ---------------------------------------------------------------------------
// PostgreSQL-gated snapshot coverage
// ---------------------------------------------------------------------------

fn postgres_url() -> Option<String> {
    std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{}-{nanos}", std::process::id())
}

#[test]
fn postgres_export_and_import_round_trip_through_the_cli() {
    use microwave_store::{IdentityRepository, OAuthRepository};

    let Some(url) = postgres_url() else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect to configured PostgreSQL");
    let username = unique("pg-migrate-user");
    let user = runtime
        .block_on(store.create_user(
            &username,
            &format!("{username}@example.com"),
            "bcrypt",
            "pg-hash",
            "pg-salt",
            true,
        ))
        .expect("create user");
    let source_application = runtime
        .block_on(store.create_oauth_application(
            &unique("pg-oauth-source"),
            "https://example.com/source",
            "",
            true,
        ))
        .expect("create source OAuth application");
    drop(store);

    let snapshots = tempfile::tempdir().unwrap();
    let snapshot = snapshots.path().join("pg-snapshot.json");
    let exported = run(&[
        "export",
        "--database-url",
        &url,
        "--output",
        snapshot.to_str().unwrap(),
    ]);
    assert!(
        exported.status.success(),
        "export failed: {}",
        stderr(&exported)
    );
    assert!(stdout(&exported).contains("Exported snapshot"));
    assert!(snapshot.is_file());

    // Import into a fresh database so a missing sequence reset is detectable:
    // the source database's sequences are already past the exported ids.
    let scratch_suffix = unique("target");
    let (scratch_name, target_url) = scratch_database(&url, &scratch_suffix);
    for attempt in 0..2 {
        let imported = run(&[
            "import",
            "--database-url",
            &target_url,
            "--input",
            snapshot.to_str().unwrap(),
            "--yes",
        ]);
        assert!(
            imported.status.success(),
            "import attempt {attempt} failed: {}",
            stderr(&imported)
        );
        assert!(stdout(&imported).contains("Imported snapshot"));
    }

    let target = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(
            &target_url,
        ))
        .expect("connect to fresh target");
    let restored = runtime
        .block_on(target.find_user(&username))
        .expect("find imported user")
        .expect("imported user exists");
    assert_eq!(restored.id, user.id);
    assert_eq!(restored.password_hash, "pg-hash");
    assert!(restored.admin);

    let max: i64 = runtime
        .block_on(
            sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM oauth_applications")
                .fetch_one(target.pool()),
        )
        .expect("read imported OAuth max id");
    assert!(max >= source_application.id);
    let next: i64 = runtime
        .block_on(
            sqlx::query_scalar(
                "SELECT nextval(pg_get_serial_sequence('oauth_applications', 'id'))",
            )
            .fetch_one(target.pool()),
        )
        .expect("read OAuth sequence");
    assert!(
        next > max,
        "fresh target sequence must advance beyond imported max id {max}, got {next}"
    );
    let application = runtime
        .block_on(target.create_oauth_application(
            &unique("pg-oauth-after-import"),
            "https://example.com/callback",
            "",
            true,
        ))
        .expect("create OAuth application after snapshot import");
    assert!(application.id > max);
    drop(target);
    drop_scratch_database(&url, &scratch_name);
}

#[test]
fn postgres_snapshot_source_flag_is_required() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("unused.json");
    let output = run(&["export", "--output", output.to_str().unwrap()]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("--data-dir") || message.contains("--database-url"));
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

fn scratch_database(base_url: &str, suffix: &str) -> (String, String) {
    let name = format!("migrate_{suffix}").replace('-', "_");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let pool = sqlx::PgPool::connect(base_url)
            .await
            .expect("connect to configured PostgreSQL");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&pool)
            .await
            .expect("create scratch database");
        pool.close().await;
    });
    let url = database_url_with_name(base_url, &name);
    (name, url)
}

fn drop_scratch_database(base_url: &str, name: &str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
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
    });
}
