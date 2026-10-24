use assert_cmd::Command;
use microwave_crypto::{verify_password, PasswordHash};
use microwave_store::SqliteStore;
use std::path::Path;
use std::process::Output;

fn ctl(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("microwave-ctl").unwrap();
    command
        .env_remove("MW_CONFIG")
        .env_remove("MW_DATABASE__DRIVER")
        .env_remove("MW_DATABASE__URL")
        .env_remove("MW_AUTH__SUPERUSER_NAME")
        .env("MW_PATHS__DATA_DIR", dir);
    command
}

fn run(dir: &Path, args: &[&str]) -> Output {
    ctl(dir).args(args).output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
}

#[test]
fn version_prints_a_semver() {
    let output = Command::cargo_bin("microwave-ctl")
        .unwrap()
        .arg("version")
        .output()
        .unwrap();
    assert_success(&output);
    assert!(stdout(&output).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn user_create_list_show_and_delete() {
    let dir = tempfile::tempdir().unwrap();

    let created = run(
        dir.path(),
        &[
            "user-create",
            "alice",
            "--email",
            "alice@example.com",
            "--password",
            "hunter2",
            "--admin",
        ],
    );
    assert_success(&created);
    assert!(stdout(&created).contains("alice"));

    let listed = run(dir.path(), &["user-list"]);
    assert_success(&listed);
    assert!(stdout(&listed).contains("alice"));

    let shown = run(dir.path(), &["user-show", "alice"]);
    assert_success(&shown);
    let text = stdout(&shown);
    assert!(text.contains("username: alice"));
    assert!(text.contains("email: alice@example.com"));
    assert!(text.contains("admin: true"));
    assert!(!text.contains("password_hash"));

    let deleted = run(dir.path(), &["user-delete", "alice"]);
    assert_success(&deleted);

    let listed = run(dir.path(), &["user-list"]);
    assert_success(&listed);
    assert!(!stdout(&listed).contains("alice"));

    let missing = run(dir.path(), &["user-show", "alice"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("not found"));
}

#[test]
fn org_create_list_show_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));

    let created = run(
        dir.path(),
        &[
            "org-create",
            "acme",
            "--full-name",
            "Acme Corp",
            "--creator",
            "bob",
        ],
    );
    assert_success(&created);

    let listed = run(dir.path(), &["org-list"]);
    assert_success(&listed);
    assert!(stdout(&listed).contains("acme"));

    let shown = run(dir.path(), &["org-show", "acme"]);
    assert_success(&shown);
    let text = stdout(&shown);
    assert!(text.contains("name: acme"));
    assert!(text.contains("full_name: Acme Corp"));
    assert!(text.contains("bob"));

    let deleted = run(dir.path(), &["org-delete", "acme"]);
    assert_success(&deleted);

    let listed = run(dir.path(), &["org-list"]);
    assert_success(&listed);
    assert!(!stdout(&listed).contains("acme"));

    let missing = run(dir.path(), &["org-show", "acme"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("not found"));
}

#[test]
fn org_user_add_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["user-create", "carol", "--password", "carol-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["org-create", "acme", "--creator", "bob"],
    ));

    let added = run(dir.path(), &["org-user-add", "acme", "carol"]);
    assert_success(&added);
    assert!(stdout(&added).contains("carol"));

    let shown = run(dir.path(), &["org-show", "acme"]);
    assert_success(&shown);
    assert!(stdout(&shown).contains("carol"));

    let store = SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
    let org = store.find_org("acme").unwrap().unwrap();
    assert_eq!(store.list_org_users(&org.id).unwrap().len(), 2);
    drop(store);

    let removed = run(dir.path(), &["org-user-remove", "acme", "carol"]);
    assert_success(&removed);

    let shown = run(dir.path(), &["org-show", "acme"]);
    assert_success(&shown);
    assert!(!stdout(&shown).contains("carol"));

    let removed_again = run(dir.path(), &["org-user-remove", "acme", "carol"]);
    assert!(!removed_again.status.success());
    assert!(stderr(&removed_again).contains("not found"));
}

#[test]
fn password_reset_prints_a_working_password() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "alice", "--password", "original-password"],
    ));

    let reset = run(dir.path(), &["password", "alice"]);
    assert_success(&reset);
    let text = stdout(&reset);
    let prefix = "New password for alice: ";
    let password = text
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .expect("password output line")
        .trim()
        .to_string();
    assert!(!password.is_empty());
    assert_ne!(password, "original-password");

    let store = SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
    let user = store.find_user("alice").unwrap().unwrap();
    let stored = PasswordHash {
        hash: user.password_hash,
        salt: user.password_salt,
        hash_type: user.hash_type,
    };
    assert!(verify_password(&password, &stored));
    assert!(!verify_password("original-password", &stored));
}

#[test]
fn backup_and_restore_round_trip() {
    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let backup_path = backups.path().join("snapshot");

    assert_success(&run(
        data.path(),
        &[
            "user-create",
            "persisted",
            "--password",
            "persisted-password",
        ],
    ));
    let bookshelf = data.path().join("bookshelf");
    std::fs::create_dir_all(&bookshelf).unwrap();
    std::fs::write(bookshelf.join("blob"), b"cookbook-bytes").unwrap();

    let backed_up = run(data.path(), &["backup", backup_path.to_str().unwrap()]);
    assert_success(&backed_up);
    assert!(backup_path.join("microwave.db").is_file());
    assert!(backup_path.join("bookshelf").join("blob").is_file());

    assert_success(&run(
        data.path(),
        &[
            "user-create",
            "transient",
            "--password",
            "transient-password",
        ],
    ));
    std::fs::write(bookshelf.join("blob"), b"mutated").unwrap();
    std::fs::write(bookshelf.join("extra"), b"extra").unwrap();

    let restored = run(data.path(), &["restore", backup_path.to_str().unwrap()]);
    assert_success(&restored);

    let listed = run(data.path(), &["user-list"]);
    assert_success(&listed);
    let text = stdout(&listed);
    assert!(text.contains("persisted"));
    assert!(!text.contains("transient"));
    assert_eq!(
        std::fs::read(bookshelf.join("blob")).unwrap(),
        b"cookbook-bytes"
    );
    assert!(!bookshelf.join("extra").exists());

    assert!(SqliteStore::verify_database(&data.path().join("microwave.db")).is_ok());
}

#[test]
fn chef_server_ctl_alias_uses_the_same_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let alias = || {
        let mut command = Command::cargo_bin("chef-server-ctl").unwrap();
        command
            .env_remove("MW_CONFIG")
            .env_remove("MW_DATABASE__DRIVER")
            .env_remove("MW_DATABASE__URL")
            .env_remove("MW_AUTH__SUPERUSER_NAME")
            .env("MW_PATHS__DATA_DIR", dir.path());
        command
    };

    let version = alias().arg("version").output().unwrap();
    assert_success(&version);
    assert!(stdout(&version).contains(env!("CARGO_PKG_VERSION")));

    let created = alias()
        .args(["user-create", "alias-user", "--password", "alias-password"])
        .output()
        .unwrap();
    assert_success(&created);

    let listed = run(dir.path(), &["user-list"]);
    assert_success(&listed);
    assert!(stdout(&listed).contains("alias-user"));
}

#[test]
fn status_reports_storage_and_superuser_state() {
    let dir = tempfile::tempdir().unwrap();

    let fresh = run(dir.path(), &["status"]);
    assert_success(&fresh);
    let text = stdout(&fresh);
    assert!(text.contains("data_dir:"));
    assert!(text.contains("database:"));
    assert!(text.contains("database_readable: false"));
    assert!(text.contains("superuser: pivotal (absent)"));

    assert_success(&run(
        dir.path(),
        &[
            "user-create",
            "pivotal",
            "--password",
            "pivotal-password",
            "--admin",
        ],
    ));

    let ready = run(dir.path(), &["status"]);
    assert_success(&ready);
    let text = stdout(&ready);
    assert!(text.contains("database_readable: true"));
    assert!(text.contains("superuser: pivotal (present)"));
}

#[test]
fn reindex_reports_that_it_is_not_implemented() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &["reindex"]);
    assert_success(&output);
    assert!(stdout(&output).contains("not implemented"));
}

#[test]
fn migrate_from_legacy_imports_a_snapshot() {
    let source = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&source.path().join("microwave.db")).unwrap();
    let user = store
        .create_user(
            "legacy",
            "legacy@example.com",
            "sha1+bcrypt",
            "legacy-hash",
            "legacy-salt",
            true,
        )
        .unwrap();
    drop(store);

    let snapshots = tempfile::tempdir().unwrap();
    let snapshot = snapshots.path().join("migration.json");
    microwave_migrate::export(source.path(), &snapshot).unwrap();

    let target = tempfile::tempdir().unwrap();
    let imported = run(
        target.path(),
        &["migrate-from-legacy", "--input", snapshot.to_str().unwrap()],
    );
    assert_success(&imported);
    assert!(stdout(&imported).contains("Imported migration snapshot"));

    let restored = SqliteStore::open(&target.path().join("microwave.db")).unwrap();
    let restored_user = restored.find_user("legacy").unwrap().unwrap();
    assert_eq!(restored_user.id, user.id);
    assert_eq!(restored_user.password_hash, "legacy-hash");
    assert_eq!(restored_user.hash_type, "sha1+bcrypt");
    assert!(restored_user.admin);
}

#[test]
fn migrate_from_legacy_rejects_unsupported_snapshots() {
    let snapshots = tempfile::tempdir().unwrap();
    let snapshot = snapshots.path().join("future.json");
    std::fs::write(
        &snapshot,
        r#"{"format":"microwave-server-snapshot","version":99,"generated_at":"2026-10-24T00:00:00Z","source":{"kind":"local-store"},"data":{}}"#,
    )
    .unwrap();

    let target = tempfile::tempdir().unwrap();
    let imported = run(
        target.path(),
        &["migrate-from-legacy", "--input", snapshot.to_str().unwrap()],
    );
    assert!(!imported.status.success());
    assert!(stderr(&imported).contains("unsupported snapshot version 99"));
}
