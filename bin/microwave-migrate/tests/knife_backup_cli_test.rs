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

fn write_secret(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
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
    assert!(message.contains("--secrets-dir"));
    assert!(message.contains("--secrets-archive"));
    assert!(message.contains("--tls-cert"));
    assert!(message.contains("--tls-key"));
    assert!(message.contains("--force"));
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
    assert!(!stdout(&imported).contains("Server secrets"));
    assert!(!target.join("pivotal.pem").exists());

    let store = microwave_store::SqliteStore::open(&database(&target)).unwrap();
    let user = store.find_user("alice").unwrap().unwrap();
    assert_eq!(user.password_hash, "alice-hash");
    let org = store.find_org("acme").unwrap().unwrap();
    assert_eq!(org.id, "acme-guid");
    assert_eq!(org.full_name, "Acme");
}

#[test]
fn imports_secrets_without_printing_contents_and_dry_run_lists_targets() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path());
    let target = root.path().join("target");
    let secrets = root.path().join("secrets");
    let private_marker = "PRIVATE-MARKER-DO-NOT-LOG";
    let public_marker = "PUBLIC-MARKER";
    write_secret(&secrets.join("pivotal.pem"), private_marker.as_bytes());
    write_secret(&secrets.join("webui_pub.pem"), public_marker.as_bytes());
    write_secret(&secrets.join("ssl/key.pem"), b"TLS-KEY-MARKER");
    write_secret(&secrets.join("ssl/cert.pem"), b"TLS-CERT-MARKER");
    write_secret(&secrets.join("ca/root.pem"), b"CA-MARKER");

    let dry_run = run(&[
        "import-knife-backup",
        "--input",
        backup.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
        "--secrets-dir",
        secrets.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(dry_run.status.success(), "{}", stderr(&dry_run));
    let dry_stdout = stdout(&dry_run);
    assert!(dry_stdout.contains("Server secrets: 5 files"));
    assert!(dry_stdout.contains("mode 0600"));
    assert!(dry_stdout.contains("mode 0644"));
    for marker in [private_marker, public_marker, "TLS-KEY-MARKER", "CA-MARKER"] {
        assert!(!dry_stdout.contains(marker));
        assert!(!stderr(&dry_run).contains(marker));
    }
    assert!(!target.join("pivotal.pem").exists());

    let imported = run(&[
        "import-knife-backup",
        "--input",
        backup.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
        "--secrets-dir",
        secrets.to_str().unwrap(),
        "--yes",
    ]);
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert!(stdout(&imported).contains("Imported 5 server secret files"));
    for marker in [private_marker, public_marker, "TLS-KEY-MARKER", "CA-MARKER"] {
        assert!(!stdout(&imported).contains(marker));
        assert!(!stderr(&imported).contains(marker));
    }
    assert_eq!(
        std::fs::read(target.join("pivotal.pem")).unwrap(),
        private_marker.as_bytes()
    );
    assert_eq!(
        std::fs::read(target.join("ssl/key.pem")).unwrap(),
        b"TLS-KEY-MARKER"
    );
    #[cfg(unix)]
    {
        assert_eq!(mode(&target.join("pivotal.pem")), 0o600);
        assert_eq!(mode(&target.join("webui_pub.pem")), 0o644);
        assert_eq!(mode(&target.join("ssl")), 0o700);
    }
}

#[test]
fn secrets_refuse_overwrite_without_force_before_importing_metadata() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path());
    let target = root.path().join("target");
    let secrets = root.path().join("secrets");
    write_secret(&secrets.join("pivotal.pem"), b"new\n");
    write_secret(&target.join("pivotal.pem"), b"old\n");

    let refused = run(&[
        "import-knife-backup",
        "--input",
        backup.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
        "--secrets-dir",
        secrets.to_str().unwrap(),
        "--yes",
    ]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--force"), "{}", stderr(&refused));
    assert_eq!(std::fs::read(target.join("pivotal.pem")).unwrap(), b"old\n");
    assert!(!database(&target).exists(), "metadata must not be imported");

    let imported = run(&[
        "import-knife-backup",
        "--input",
        backup.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
        "--secrets-dir",
        secrets.to_str().unwrap(),
        "--force",
        "--yes",
    ]);
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert_eq!(std::fs::read(target.join("pivotal.pem")).unwrap(), b"new\n");
}

#[test]
fn configured_tls_paths_receive_the_certificate_and_key() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path());
    let target = root.path().join("target");
    let secrets = root.path().join("secrets");
    write_secret(&secrets.join("cert.pem"), b"configured cert\n");
    write_secret(&secrets.join("key.pem"), b"configured key\n");
    let tls_cert = root.path().join("tls/server.crt");
    let tls_key = root.path().join("tls/server.key");
    let config = root.path().join("microwave.toml");
    std::fs::write(
        &config,
        format!(
            "[server.tls]\ncert_path = {:?}\nkey_path = {:?}\n",
            tls_cert.display().to_string(),
            tls_key.display().to_string()
        ),
    )
    .unwrap();

    let imported = run(&[
        "import-knife-backup",
        "--input",
        backup.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
        "--secrets-dir",
        secrets.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
        "--yes",
    ]);
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert_eq!(std::fs::read(&tls_cert).unwrap(), b"configured cert\n");
    assert_eq!(std::fs::read(&tls_key).unwrap(), b"configured key\n");
    #[cfg(unix)]
    {
        assert_eq!(mode(&tls_cert), 0o644);
        assert_eq!(mode(&tls_key), 0o600);
        assert_eq!(mode(tls_key.parent().unwrap()), 0o700);
    }
}
