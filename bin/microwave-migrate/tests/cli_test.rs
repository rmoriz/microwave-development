use assert_cmd::Command;
use microwave_store::{NewNode, SqliteStore};
use std::path::Path;
use std::process::Output;

fn migrate() -> Command {
    let mut command = Command::cargo_bin("microwave-migrate").unwrap();
    // Keep the suite independent of the developer/CI environment. Tests that
    // need an override pass `--compression`/`--config` explicitly.
    command
        .env_remove("MW_STORAGE__COMPRESSION")
        .env_remove("MW_CONFIG");
    command
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

fn seed_node(store: &SqliteStore, suffix: &str) -> (String, String, String) {
    let org = store
        .create_org(&format!("import-org-{suffix}"), "Import Org")
        .unwrap();
    let creator = store.create_actor().unwrap();
    store
        .create_container_for_org_with_acl(&org.id, "nodes", &creator)
        .unwrap();
    let name = format!("import-node-{suffix}");
    let body = format!(r#"{{"name":"{name}","run_list":[]}}"#);
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
        .unwrap();
    (org.id, name, body)
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
fn archive_export_and_import_round_trip_bookshelf_and_snapshot() {
    let source = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(source.path())).unwrap();
    let org = store.create_org("archive-cli", "Archive CLI").unwrap();
    store
        .create_user(
            "archive-user",
            "archive@example.com",
            "bcrypt",
            "archive-hash",
            "archive-salt",
            false,
        )
        .unwrap();
    drop(store);

    let blob_dir = source
        .path()
        .join("bookshelf")
        .join(format!("organization-{}", org.id));
    std::fs::create_dir_all(&blob_dir).unwrap();
    std::fs::write(blob_dir.join("checksum-feedface"), b"blob").unwrap();

    let archive = source.path().join("migration.tar.gz");
    let exported = run(&[
        "export",
        "--data-dir",
        source.path().to_str().unwrap(),
        "--archive",
        archive.to_str().unwrap(),
    ]);
    assert!(
        exported.status.success(),
        "archive export failed: {}",
        stderr(&exported)
    );
    assert!(stdout(&exported).contains("Exported archive"));
    assert!(stdout(&exported).contains("Secrets: excluded"));
    assert!(archive.is_file());

    let target = tempfile::tempdir().unwrap();
    let refused = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--archive",
        archive.to_str().unwrap(),
    ]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));
    assert!(!database(target.path()).exists());

    let imported = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--archive",
        archive.to_str().unwrap(),
        "--yes",
    ]);
    assert!(
        imported.status.success(),
        "archive import failed: {}",
        stderr(&imported)
    );
    assert!(stdout(&imported).contains("Imported archive"));

    let restored = SqliteStore::open(&database(target.path())).unwrap();
    assert!(restored.find_user("archive-user").unwrap().is_some());
    assert_eq!(
        std::fs::read(
            target
                .path()
                .join("bookshelf")
                .join(format!("organization-{}", org.id))
                .join("checksum-feedface")
        )
        .unwrap(),
        b"blob"
    );
}

#[test]
fn export_include_secrets_controls_kvpair_content() {
    let source = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(source.path())).unwrap();
    store
        .set_secret("bookshelf", "secret_access_key", "cli-secret-value")
        .unwrap();
    drop(store);

    let snapshots = tempfile::tempdir().unwrap();
    let safe = snapshots.path().join("safe.json");
    let full = snapshots.path().join("full.json");
    let exported = run(&[
        "export",
        "--data-dir",
        source.path().to_str().unwrap(),
        "--output",
        safe.to_str().unwrap(),
    ]);
    assert!(exported.status.success(), "{}", stderr(&exported));
    let safe_text = std::fs::read_to_string(&safe).unwrap();
    assert!(!safe_text.contains("cli-secret-value"));
    assert!(safe_text.contains("\"secrets_included\": false"));

    let exported = run(&[
        "export",
        "--data-dir",
        source.path().to_str().unwrap(),
        "--output",
        full.to_str().unwrap(),
        "--include-secrets",
    ]);
    assert!(exported.status.success(), "{}", stderr(&exported));
    assert!(stdout(&exported).contains("Secrets: included"));
    let full_text = std::fs::read_to_string(&full).unwrap();
    assert!(full_text.contains("cli-secret-value"));
    assert!(full_text.contains("\"secrets_included\": true"));
}

fn write_private_fixture(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn archive_entries(path: &Path) -> Vec<(String, Vec<u8>, u32)> {
    use std::io::Read;
    let file = std::fs::File::open(path).unwrap();
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let mode = entry.header().mode().unwrap();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        entries.push((name, data, mode));
    }
    entries
}

#[test]
fn archive_private_keys_require_the_flag_and_force_on_import() {
    let source = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(source.path())).unwrap();
    store.create_org("private-cli", "Private CLI").unwrap();
    drop(store);
    write_private_fixture(&source.path().join("pivotal.pem"), b"CLI-KEY-MARKER");
    write_private_fixture(
        &source.path().join("private-chef-secrets.json"),
        b"CLI-CHEF-SECRET-MARKER",
    );
    write_private_fixture(&source.path().join("notes.txt"), b"NOT-A-KEY");

    let safe = source.path().join("safe.tar.gz");
    let exported = run(&[
        "export",
        "--data-dir",
        source.path().to_str().unwrap(),
        "--archive",
        safe.to_str().unwrap(),
    ]);
    assert!(exported.status.success(), "{}", stderr(&exported));
    assert!(stdout(&exported).contains("Private keys: excluded"));
    let entries = archive_entries(&safe);
    assert!(!entries
        .iter()
        .any(|(name, _, _)| name.starts_with("secrets/")));
    let safe_bytes = entries
        .iter()
        .flat_map(|(_, data, _)| data.iter().copied())
        .collect::<Vec<_>>();
    let safe_payload = String::from_utf8_lossy(&safe_bytes);
    assert!(!safe_payload.contains("CLI-KEY-MARKER"));

    let full = source.path().join("full.tar.gz");
    let exported = run(&[
        "export",
        "--data-dir",
        source.path().to_str().unwrap(),
        "--archive",
        full.to_str().unwrap(),
        "--include-private-keys",
    ]);
    assert!(exported.status.success(), "{}", stderr(&exported));
    assert!(stdout(&exported).contains("Private keys: included"));
    let entries = archive_entries(&full);
    let key_entry = entries
        .iter()
        .find(|(name, _, _)| name.starts_with("secrets/files/") && name.ends_with("pivotal.pem"))
        .expect("pivotal.pem is in the private archive");
    assert_eq!(key_entry.1, b"CLI-KEY-MARKER");
    assert_eq!(key_entry.2, 0o600);
    assert!(entries
        .iter()
        .any(|(name, _, _)| name == "secrets/manifest.json"));
    assert!(!entries
        .iter()
        .any(|(name, _, _)| name.ends_with("notes.txt")));

    let target = tempfile::tempdir().unwrap();
    let imported = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--archive",
        full.to_str().unwrap(),
        "--yes",
    ]);
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert_eq!(
        std::fs::read(target.path().join("pivotal.pem")).unwrap(),
        b"CLI-KEY-MARKER"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(target.path().join("pivotal.pem"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    let refused = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--archive",
        full.to_str().unwrap(),
        "--yes",
    ]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--force"));
    assert_eq!(
        std::fs::read(target.path().join("pivotal.pem")).unwrap(),
        b"CLI-KEY-MARKER"
    );

    let forced = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--archive",
        full.to_str().unwrap(),
        "--yes",
        "--force",
    ]);
    assert!(forced.status.success(), "{}", stderr(&forced));

    let secrets_target = tempfile::tempdir().unwrap();
    let separate_data = tempfile::tempdir().unwrap();
    let imported = run(&[
        "import",
        "--data-dir",
        separate_data.path().to_str().unwrap(),
        "--archive",
        full.to_str().unwrap(),
        "--secrets-dir",
        secrets_target.path().to_str().unwrap(),
        "--yes",
    ]);
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert_eq!(
        std::fs::read(secrets_target.path().join("pivotal.pem")).unwrap(),
        b"CLI-KEY-MARKER"
    );
    assert!(!separate_data.path().join("pivotal.pem").exists());
}

#[test]
fn archive_private_keys_use_configured_tls_paths() {
    let source = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(source.path())).unwrap();
    store
        .create_org("private-tls-cli", "Private TLS CLI")
        .unwrap();
    drop(store);
    write_private_fixture(&source.path().join("pivotal.pem"), b"CLI-PIVOTAL-MARKER");
    let source_tls = tempfile::tempdir().unwrap();
    let source_cert = source_tls.path().join("certs/site.crt");
    let source_key = source_tls.path().join("certs/site.key");
    write_private_fixture(&source_cert, b"CLI-CERT-MARKER");
    write_private_fixture(&source_key, b"CLI-TLS-KEY-MARKER");
    let source_config = source.path().join("source.toml");
    std::fs::write(
        &source_config,
        format!(
            "[server.tls]\ncert_path = {:?}\nkey_path = {:?}\n",
            source_cert.display().to_string(),
            source_key.display().to_string()
        ),
    )
    .unwrap();

    let archive = source.path().join("tls.tar.gz");
    let exported = run(&[
        "export",
        "--data-dir",
        source.path().to_str().unwrap(),
        "--archive",
        archive.to_str().unwrap(),
        "--include-private-keys",
        "--config",
        source_config.to_str().unwrap(),
    ]);
    assert!(exported.status.success(), "{}", stderr(&exported));
    let entries = archive_entries(&archive);
    for expected in ["site.crt", "site.key", "pivotal.pem"] {
        assert!(entries.iter().any(|(name, _, _)| {
            name.starts_with("secrets/files/") && name.ends_with(expected)
        }));
    }

    let target_data = tempfile::tempdir().unwrap();
    let target_tls = tempfile::tempdir().unwrap();
    let target_cert = target_tls.path().join("restored/site.crt");
    let target_key = target_tls.path().join("restored/site.key");
    let target_config = source.path().join("target.toml");
    std::fs::write(
        &target_config,
        format!(
            "[server.tls]\ncert_path = {:?}\nkey_path = {:?}\n",
            target_cert.display().to_string(),
            target_key.display().to_string()
        ),
    )
    .unwrap();
    let imported = run(&[
        "import",
        "--data-dir",
        target_data.path().to_str().unwrap(),
        "--archive",
        archive.to_str().unwrap(),
        "--config",
        target_config.to_str().unwrap(),
        "--yes",
    ]);
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert_eq!(std::fs::read(target_cert).unwrap(), b"CLI-CERT-MARKER");
    assert_eq!(std::fs::read(target_key).unwrap(), b"CLI-TLS-KEY-MARKER");
    assert_eq!(
        std::fs::read(target_data.path().join("pivotal.pem")).unwrap(),
        b"CLI-PIVOTAL-MARKER"
    );
}

#[test]
fn import_with_compression_none_stores_plain_serialized_objects() {
    let source = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(source.path())).unwrap();
    let (_org_id, name, body) = seed_node(&store, "none");
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

    let target = tempfile::tempdir().unwrap();
    let imported = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--input",
        snapshot.to_str().unwrap(),
        "--compression",
        "none",
        "--yes",
    ]);
    assert!(
        imported.status.success(),
        "import failed: {}",
        stderr(&imported)
    );
    assert!(stdout(&imported).contains("Import compression: none"));

    let connection = rusqlite::Connection::open(database(target.path())).unwrap();
    let raw: String = connection
        .query_row(
            "SELECT serialized_object FROM nodes WHERE name = ?1",
            [&name],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, body);
    assert!(!raw.starts_with("mwz1:"));
}

#[test]
fn import_compression_defaults_from_config_file() {
    let source = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(source.path())).unwrap();
    let (_org_id, name, _body) = seed_node(&store, "config");
    drop(store);

    let snapshots = tempfile::tempdir().unwrap();
    let snapshot = snapshots.path().join("migration.json");
    run(&[
        "export",
        "--data-dir",
        source.path().to_str().unwrap(),
        "--output",
        snapshot.to_str().unwrap(),
    ]);

    let config = snapshots.path().join("microwave.toml");
    std::fs::write(&config, "[storage]\ncompression = \"none\"\n").unwrap();

    let target = tempfile::tempdir().unwrap();
    let imported = run(&[
        "import",
        "--data-dir",
        target.path().to_str().unwrap(),
        "--input",
        snapshot.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
        "--yes",
    ]);
    assert!(
        imported.status.success(),
        "import failed: {}",
        stderr(&imported)
    );
    assert!(stdout(&imported).contains("Import compression: none"));

    let connection = rusqlite::Connection::open(database(target.path())).unwrap();
    let raw: String = connection
        .query_row(
            "SELECT serialized_object FROM nodes WHERE name = ?1",
            [&name],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!raw.starts_with("mwz1:"));
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
fn import_help_lists_compression_options() {
    let output = run(&["import", "--help"]);
    assert!(output.status.success());
    let message = stdout(&output);
    assert!(message.contains("--compression"));
    assert!(message.contains("--config"));
    assert!(message.contains("--secrets-dir"));
    assert!(message.contains("--force"));
    assert!(message.contains("zlib"));
    assert!(message.contains("none"));
}

#[test]
fn export_help_lists_private_key_options() {
    let output = run(&["export", "--help"]);
    assert!(output.status.success());
    let message = stdout(&output);
    assert!(message.contains("--include-secrets"));
    assert!(message.contains("--include-private-keys"));
    assert!(message.contains("--archive"));
    assert!(message.contains("--bookshelf-dir"));
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
fn postgres_import_with_compression_none_stores_plain_serialized_objects() {
    use microwave_store::{AuthzRepository, IdentityRepository, ObjectRepository};

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
    let suffix = unique("pg-plain");
    let org = runtime
        .block_on(store.create_org(&format!("org-{suffix}"), "Plain Import Org"))
        .expect("create org");
    let creator = runtime
        .block_on(store.create_actor())
        .expect("create actor");
    runtime
        .block_on(store.create_container_for_org(&org.id, "nodes"))
        .expect("create nodes container");
    let name = format!("node-{suffix}");
    let body = format!(r#"{{"name":"{name}","run_list":[]}}"#);
    runtime
        .block_on(store.create_node(
            &org.id,
            NewNode {
                name: &name,
                body: &body,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &creator,
            },
        ))
        .expect("create node");
    drop(store);

    let snapshots = tempfile::tempdir().unwrap();
    let snapshot = snapshots.path().join("pg-plain.json");
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

    let (scratch_name, target_url) = scratch_database(&url, &suffix);
    let imported = run(&[
        "import",
        "--database-url",
        &target_url,
        "--input",
        snapshot.to_str().unwrap(),
        "--compression",
        "none",
        "--yes",
    ]);
    assert!(
        imported.status.success(),
        "import failed: {}",
        stderr(&imported)
    );
    assert!(stdout(&imported).contains("Import compression: none"));

    let target = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(
            &target_url,
        ))
        .expect("connect to plain import target");
    let raw: String = runtime
        .block_on(
            sqlx::query_scalar(
                "SELECT serialized_object FROM nodes WHERE org_id = $1 AND name = $2",
            )
            .bind(&org.id)
            .bind(&name)
            .fetch_one(target.pool()),
        )
        .expect("read raw imported value");
    assert_eq!(raw, body);
    assert!(!raw.starts_with("mwz1:"));
    let node = runtime
        .block_on(target.find_node(&org.id, &name))
        .expect("find imported node")
        .expect("imported node exists");
    assert_eq!(node.serialized_object, body);

    drop(target);
    drop_scratch_database(&url, &scratch_name);
}

#[test]
fn postgres_archive_export_and_import_round_trip_bookshelf() {
    use microwave_store::IdentityRepository;

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
    let username = unique("pg-archive-user");
    let user = runtime
        .block_on(store.create_user(
            &username,
            &format!("{username}@example.com"),
            "bcrypt",
            "pg-archive-hash",
            "pg-archive-salt",
            false,
        ))
        .expect("create archive user");
    drop(store);

    let source = tempfile::tempdir().unwrap();
    let bookshelf = source.path().join("bookshelf");
    let blob_dir = bookshelf.join("organization-archive");
    std::fs::create_dir_all(&blob_dir).unwrap();
    std::fs::write(blob_dir.join("checksum-feedface"), b"pg-blob").unwrap();
    let archive = source.path().join("pg-archive.tar.gz");
    let exported = run(&[
        "export",
        "--database-url",
        &url,
        "--bookshelf-dir",
        bookshelf.to_str().unwrap(),
        "--archive",
        archive.to_str().unwrap(),
    ]);
    assert!(
        exported.status.success(),
        "archive export failed: {}",
        stderr(&exported)
    );
    assert!(archive.is_file());

    let suffix = unique("pg-archive-target");
    let (scratch_name, target_url) = scratch_database(&url, &suffix);
    let target_root = tempfile::tempdir().unwrap();
    let target_bookshelf = target_root.path().join("bookshelf");
    let imported = run(&[
        "import",
        "--database-url",
        &target_url,
        "--bookshelf-dir",
        target_bookshelf.to_str().unwrap(),
        "--archive",
        archive.to_str().unwrap(),
        "--yes",
    ]);
    assert!(
        imported.status.success(),
        "archive import failed: {}",
        stderr(&imported)
    );

    let target = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(
            &target_url,
        ))
        .expect("connect to archive target");
    let restored = runtime
        .block_on(target.find_user(&username))
        .expect("find archive user")
        .expect("archive user exists");
    assert_eq!(restored.id, user.id);
    assert_eq!(
        std::fs::read(
            target_bookshelf
                .join("organization-archive")
                .join("checksum-feedface")
        )
        .unwrap(),
        b"pg-blob"
    );
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
