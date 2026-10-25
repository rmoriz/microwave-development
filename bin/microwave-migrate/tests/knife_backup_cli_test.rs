use assert_cmd::Command;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Output;

fn migrate() -> Command {
    let mut command = Command::cargo_bin("microwave-migrate").unwrap();
    command
        .env_remove("MW_STORAGE__COMPRESSION")
        .env_remove("MW_CONFIG");
    command
}

fn run(args: &[&str]) -> Output {
    migrate().args(args).output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write_json(path: &Path, value: &serde_json::Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn fixture(root: &Path) -> PathBuf {
    let backup = root.join("backup");
    write_json(
        &backup.join("users/alice.json"),
        &json!({
            "username": "alice",
            "email": "alice@example.com",
            "display_name": "Alice"
        }),
    );
    write_json(
        &backup.join("key_dump.json"),
        &json!([
            {
                "username": "alice",
                "hashed_password": "alice-hash",
                "salt": "alice-salt",
                "hash_type": "bcrypt",
                "admin": false
            }
        ]),
    );
    write_json(
        &backup.join("organizations/acme/org.json"),
        &json!({ "name": "acme", "full_name": "Acme", "guid": "acme-guid" }),
    );
    backup
}

fn database(dir: &Path) -> PathBuf {
    dir.join("microwave.db")
}

#[test]
fn help_lists_the_knife_backup_options() {
    let output = run(&["import-knife-backup", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let message = stdout(&output);
    assert!(message.contains("--input"));
    assert!(message.contains("--dry-run"));
    assert!(message.contains("--bookshelf-dir"));
    assert!(message.contains("--yes"));
}

#[test]
fn dry_run_parses_without_creating_the_target_store() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path());
    let target = root.path().join("target");
    let output = run(&[
        "import-knife-backup",
        "--input",
        backup.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Backup contents:"));
    assert!(stdout(&output).contains("Dry run"));
    assert!(!database(&target).exists());
    assert!(!target.join("bookshelf").exists());
}

#[test]
fn import_requires_yes_and_then_writes_users_and_orgs() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path());
    let target = root.path().join("target");
    let refused = run(&[
        "import-knife-backup",
        "--input",
        backup.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
    ]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));
    assert!(!database(&target).exists());

    let imported = run(&[
        "import-knife-backup",
        "--input",
        backup.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
        "--yes",
    ]);
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert!(stdout(&imported).contains("Backup contents:"));
    assert!(stdout(&imported).contains("Imported knife backup"));

    let store = microwave_store::SqliteStore::open(&database(&target)).unwrap();
    let user = store.find_user("alice").unwrap().unwrap();
    assert_eq!(user.password_hash, "alice-hash");
    let org = store.find_org("acme").unwrap().unwrap();
    assert_eq!(org.id, "acme-guid");
    assert_eq!(org.full_name, "Acme");
}
