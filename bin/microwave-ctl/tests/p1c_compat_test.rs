//! End-to-end coverage for the P1c `chef-server-ctl` runbook compatibility
//! surface.

use assert_cmd::Command;
use microwave_crypto::{parse_private_key_pem, parse_public_key_pem};
use microwave_store::{ActorType, SqliteStore};
use std::path::Path;
use std::process::Output;

fn ctl(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("microwave-ctl").unwrap();
    command
        .env_remove("MW_CONFIG")
        .env_remove("MW_DATABASE__DRIVER")
        .env_remove("MW_DATABASE__URL")
        .env_remove("MW_AUTH__SUPERUSER_NAME")
        .env_remove("MW_SERVICE_UNIT")
        .env_remove("MW_SERVICE_FOREGROUND")
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

fn extract_private_key(output: &str) -> String {
    const BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
    const END: &str = "-----END PRIVATE KEY-----";
    let start = output.find(BEGIN).expect("private key start marker");
    let end = output[start..].find(END).expect("private key end marker") + start + END.len();
    output[start..end].to_string()
}

#[test]
fn user_create_accepts_the_upstream_positional_form_and_prints_a_key() {
    let dir = tempfile::tempdir().unwrap();
    let created = run(
        dir.path(),
        &[
            "user-create",
            "alice",
            "Alice",
            "Marie",
            "Smith",
            "alice@example.com",
            "correct-horse-password",
        ],
    );
    assert_success(&created);
    let text = stdout(&created);
    assert!(text.contains("Created user alice"));
    let private_pem = extract_private_key(&text);
    parse_private_key_pem(&private_pem).unwrap();

    let store = SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
    let user = store.find_user("alice").unwrap().unwrap();
    assert_eq!(user.first_name.as_deref(), Some("Alice"));
    assert_eq!(user.middle_name.as_deref(), Some("Marie"));
    assert_eq!(user.last_name.as_deref(), Some("Smith"));
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));

    let key = store
        .find_actor_key(&user.id, ActorType::User, "default")
        .unwrap()
        .expect("default user key");
    assert!(key.public_key.contains("BEGIN PUBLIC KEY"));
    parse_public_key_pem(&key.public_key).unwrap();
}

#[test]
fn user_create_supports_a_supplied_public_key_without_printing_a_private_key() {
    let dir = tempfile::tempdir().unwrap();
    let keys = tempfile::tempdir().unwrap();
    let (_, public_key) = microwave_crypto::generate_rsa_keypair().unwrap();
    let public_path = keys.path().join("alice.pub");
    std::fs::write(&public_path, &public_key).unwrap();

    let created = run(
        dir.path(),
        &[
            "user-create",
            "alice",
            "--email",
            "alice@example.com",
            "--public-key-path",
            public_path.to_str().unwrap(),
        ],
    );
    assert_success(&created);
    let text = stdout(&created);
    assert!(!text.contains("BEGIN PRIVATE KEY"));
    assert!(text.contains("Added key default for user alice"));

    let store = SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
    let user = store.find_user("alice").unwrap().unwrap();
    let key = store
        .find_actor_key(&user.id, ActorType::User, "default")
        .unwrap()
        .unwrap();
    assert_eq!(key.public_key.trim(), public_key.trim());
}

#[test]
fn org_create_creates_a_validator_and_can_write_the_key_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));

    let key_file = dir.path().join("acme-validator.pem");
    let created = run(
        dir.path(),
        &[
            "org-create",
            "acme",
            "Acme Corp",
            "--creator",
            "bob",
            "-a",
            "bob",
            "-f",
            key_file.to_str().unwrap(),
        ],
    );
    assert_success(&created);
    let text = stdout(&created);
    assert!(text.contains("Created organization acme (Acme Corp)"));
    assert!(text.contains("acme-validator"));
    assert!(text.contains("Associated user bob"));
    assert!(
        !text.contains("BEGIN PRIVATE KEY"),
        "the key was printed despite --filename: {text}"
    );

    let private_key = std::fs::read_to_string(&key_file).unwrap();
    parse_private_key_pem(&private_key).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let store = SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
    let org = store.find_org("acme").unwrap().unwrap();
    let validator = store
        .find_client(&org.id, "acme-validator")
        .unwrap()
        .expect("validator client");
    assert!(validator.validator);
    let key = store
        .find_actor_key(&validator.id, ActorType::Client, "default")
        .unwrap()
        .expect("validator default key");
    parse_public_key_pem(&key.public_key).unwrap();

    let bob = store.find_user("bob").unwrap().unwrap();
    assert_eq!(bob.username, "bob");
    assert!(store
        .list_org_users(&org.id)
        .unwrap()
        .iter()
        .any(|user| user.username == "bob"));
    for group_name in ["admins", "billing-admins"] {
        let group = store
            .find_group(&org.id, group_name)
            .unwrap()
            .expect("default group");
        let members = store.list_group_members(&org.id, &group.authz_id).unwrap();
        assert!(
            members.users.iter().any(|member| member == "bob"),
            "bob is missing from {group_name}: {members:?}"
        );
    }
}

#[test]
fn psql_accepts_the_upstream_service_name() {
    if std::process::Command::new("sqlite3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping psql test: sqlite3 is not installed");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "alice", "--password", "alice-password"],
    ));

    let output = run(
        dir.path(),
        &["psql", "erchef", "select count(*) from users;"],
    );
    assert_success(&output);
    assert_eq!(stdout(&output).trim(), "1");

    // `--write` is accepted and `--options` tokens are passed to the client
    // before the database argument.
    let output = run(
        dir.path(),
        &[
            "psql",
            "erchef",
            "--write",
            "--options",
            "-header",
            "select count(*) from users;",
        ],
    );
    assert_success(&output);
    assert!(stdout(&output).contains("count(*)"));
    assert!(stdout(&output).contains('1'));
}

#[test]
fn show_service_credentials_reveals_stored_values() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["set-secret", "bookshelf", "access_key_id", "access-token"],
    ));
    assert_success(&run(
        dir.path(),
        &[
            "set-secret",
            "bookshelf",
            "secret_access_key",
            "secret-token",
        ],
    ));
    assert_success(&run(
        dir.path(),
        &["set-secret", "object_store", "secret", "hmac-token"],
    ));

    let output = run(dir.path(), &["show-service-credentials"]);
    assert_success(&output);
    let text = stdout(&output);
    assert!(text.contains("bookshelf:"));
    assert!(text.contains("access_key_id: access-token"));
    assert!(text.contains("secret_access_key: secret-token"));
    assert!(text.contains("object_store:"));
    assert!(text.contains("secret: hmac-token"));
}

#[test]
fn require_credential_rotation_writes_and_rotate_shared_secrets_clears_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("credential_rotation_required");

    let refused = run(dir.path(), &["require-credential-rotation"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));
    assert!(!marker.exists());

    let required = run(dir.path(), &["require-credential-rotation", "--yes"]);
    assert_success(&required);
    let text = stdout(&required);
    assert!(text.contains("disabled until credentials have been rotated"));
    assert!(text.contains("rotate-shared-secrets"));
    assert!(marker.is_file());

    let rotated = run(dir.path(), &["rotate-shared-secrets"]);
    assert_success(&rotated);
    assert!(stdout(&rotated).contains("Cleared credential rotation requirement."));
    assert!(!marker.exists());
}

#[test]
fn cleanup_orphan_blobs_estimates_and_deletes_only_unreferenced_blobs() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["org-create", "acme", "--creator", "bob"],
    ));

    let database = dir.path().join("microwave.db");
    let org_id = {
        let store = SqliteStore::open(&database).unwrap();
        let org = store.find_org("acme").unwrap().unwrap();
        // Commit a checksum so it is referenced by the metadata store.
        let (sandbox, _) = store
            .create_sandbox(&org.id, &["referenced-checksum".to_string()])
            .unwrap();
        store.commit_sandbox(&org.id, &sandbox).unwrap();
        org.id
    };

    let bookshelf = dir.path().join("bookshelf");
    let org_dir = bookshelf.join(format!("organization-{org_id}"));
    std::fs::create_dir_all(&org_dir).unwrap();
    let referenced = org_dir.join("checksum-referenced-checksum");
    let orphan = org_dir.join("checksum-orphan-checksum");
    std::fs::write(&referenced, b"referenced").unwrap();
    std::fs::write(&orphan, b"orphaned").unwrap();

    let estimated = run(dir.path(), &["cleanup-orphan-blobs", "--estimate-only"]);
    assert_success(&estimated);
    let text = stdout(&estimated);
    assert!(text.contains("Blobs scanned: 2"), "stdout: {text}");
    assert!(text.contains("Referenced blobs: 1"), "stdout: {text}");
    assert!(text.contains("Orphaned blobs: 1"), "stdout: {text}");
    assert!(text.contains("Nothing deleted"), "stdout: {text}");
    assert!(referenced.is_file());
    assert!(orphan.is_file());

    let cleaned = run(dir.path(), &["cleanup-orphan-blobs"]);
    assert_success(&cleaned);
    let text = stdout(&cleaned);
    assert!(text.contains("Removed orphaned blobs: 1"), "stdout: {text}");
    assert!(referenced.is_file());
    assert!(!orphan.exists());
}

#[test]
fn org_delete_removes_the_local_bookshelf_prefix() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["org-create", "acme", "--creator", "bob"],
    ));

    let org_id = {
        let store = SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
        store.find_org("acme").unwrap().unwrap().id
    };
    let org_dir = dir
        .path()
        .join("bookshelf")
        .join(format!("organization-{org_id}"));
    std::fs::create_dir_all(&org_dir).unwrap();
    std::fs::write(org_dir.join("checksum-abc"), b"blob").unwrap();

    let deleted = run(dir.path(), &["org-delete", "acme"]);
    assert_success(&deleted);
    assert!(!org_dir.exists());
}

#[test]
fn backup_defaults_to_a_timestamped_directory() {
    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    assert_success(&run(
        data.path(),
        &["user-create", "alice", "--password", "alice-password"],
    ));

    let output = ctl(data.path())
        .current_dir(backups.path())
        .args(["backup", "--yes"])
        .output()
        .unwrap();
    assert_success(&output);
    let text = stdout(&output);
    let prefix = "Backup written to ";
    let written = text
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .expect("backup path line");
    assert!(written.starts_with("microwave-backup-"), "stdout: {text}");

    let backup_dir = backups.path().join(written);
    assert!(backup_dir.is_dir());
    assert!(backup_dir.join("microwave.db").is_file());

    // --config-only writes a configuration snapshot and no metadata database.
    let output = ctl(data.path())
        .current_dir(backups.path())
        .args(["backup", "config-only", "--config-only", "--yes"])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(backups
        .path()
        .join("config-only")
        .join("microwave-config.json")
        .is_file());
    assert!(!backups
        .path()
        .join("config-only")
        .join("microwave.db")
        .exists());
}

#[test]
fn help_lists_the_service_aliases_and_clarifies_status() {
    let output = Command::cargo_bin("microwave-ctl")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    assert_success(&output);
    let text = stdout(&output);
    for command in [
        "hup",
        "restart",
        "start",
        "stop",
        "tail",
        "service-list",
        "cleanup-orphan-blobs",
        "show-service-credentials",
        "require-credential-rotation",
    ] {
        assert!(text.contains(command), "help is missing {command}: {text}");
    }

    let output = Command::cargo_bin("microwave-ctl")
        .unwrap()
        .args(["help", "status"])
        .output()
        .unwrap();
    assert_success(&output);
    let text = stdout(&output);
    assert!(text.contains("storage"), "status help: {text}");
    assert!(text.contains("service status"), "status help: {text}");
    assert!(text.contains("service-list"), "status help: {text}");
}

#[test]
fn service_aliases_map_to_the_configured_systemd_unit() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("systemctl.log");
    let systemctl = dir.path().join("systemctl");
    std::fs::write(
        &systemctl,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$MW_TEST_LOG\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let output = ctl(dir.path())
        .env("MW_SYSTEMCTL", &systemctl)
        .env("MW_TEST_LOG", &log)
        .args(["hup", "--unit", "demo.service"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec!["restart", "demo.service"]
    );

    let listed = run(dir.path(), &["service-list"]);
    assert_success(&listed);
    assert!(stdout(&listed).contains("* microwave-server"));
}

#[cfg(unix)]
#[test]
fn service_start_alias_supports_foreground() {
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("microwave-server");
    let log = dir.path().join("server.log");
    std::fs::write(
        &server,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$MW_TEST_LOG\"\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = dir.path().join("microwave.toml");
    std::fs::write(&config, "[server]\nbind = \"127.0.0.1:8000\"\n").unwrap();

    let output = ctl(dir.path())
        .env("MW_SERVER_BIN", &server)
        .env("MW_TEST_LOG", &log)
        .args([
            "start",
            "--foreground",
            "--config",
            config.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec!["serve", &format!("--config={}", config.display())]
    );
}
