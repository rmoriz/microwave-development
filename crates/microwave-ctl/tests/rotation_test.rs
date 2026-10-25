//! Integration coverage for the credential rotation commands.
//!
//! Output assertions need the real process stdout. Rust's test harness
//! captures `println!` inside a test thread, so each command runs in a child
//! invocation of this same test binary with `--nocapture`. The child helper
//! below is a normal test that only does work when the parent sets its
//! environment variables.

use microwave_crypto::{
    parse_public_key_pem, sign_request, verify_request, CanonicalRequest, SignVersion,
};
use microwave_ctl::{dispatch, Cli, Command as CtlCommand};
use microwave_store::SqliteStore;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Runs the requested command when spawned by [`run_ctl`], otherwise no-ops.
#[test]
fn rotation_child_dispatch() {
    let Ok(config) = std::env::var("MW_ROTATION_TEST_CONFIG") else {
        return;
    };
    let kind = std::env::var("MW_ROTATION_TEST_COMMAND").expect("child command");
    let service = std::env::var("MW_ROTATION_TEST_SERVICE").ok();

    let cli = Cli {
        config: Some(PathBuf::from(config)),
        command: ctl_command(&kind, service.as_deref()),
    };

    if let Err(error) = dispatch(cli) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn ctl_command(kind: &str, service: Option<&str>) -> CtlCommand {
    match kind {
        "rotate-credentials" => CtlCommand::RotateCredentials {
            service: service.expect("service for rotate-credentials").to_string(),
        },
        "rotate-all-credentials" => CtlCommand::RotateAllCredentials,
        "rotate-shared-secrets" => CtlCommand::RotateSharedSecrets,
        other => panic!("unknown test command {other}"),
    }
}

/// Run one CLI command in a child test process and capture its output.
fn run_ctl(config: &Path, kind: &str, service: Option<&str>) -> Output {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "rotation_child_dispatch", "--nocapture"])
        .env_remove("MW_CONFIG")
        .env_remove("MW_DATABASE__DRIVER")
        .env_remove("MW_DATABASE__URL")
        .env_remove("MW_PATHS__DATA_DIR")
        .env("MW_ROTATION_TEST_CONFIG", config)
        .env("MW_ROTATION_TEST_COMMAND", kind);
    match service {
        Some(service) => {
            command.env("MW_ROTATION_TEST_SERVICE", service);
        }
        None => {
            command.env_remove("MW_ROTATION_TEST_SERVICE");
        }
    }
    command.output().unwrap()
}

/// A temporary installation with a resolved SQLite path and config file.
struct Installation {
    _dir: tempfile::TempDir,
    config: PathBuf,
    database: PathBuf,
}

impl Installation {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let config = dir.path().join("microwave.toml");
        std::fs::write(
            &config,
            format!("[paths]\ndata_dir = \"{}\"\n", data_dir.display()),
        )
        .unwrap();
        Self {
            _dir: dir,
            config,
            database: data_dir.join("microwave.db"),
        }
    }

    fn seed(&self, values: &[(&str, &str, &str)]) {
        let store = SqliteStore::open(&self.database).unwrap();
        for &(group, name, value) in values {
            store.set_secret(group, name, value).unwrap();
        }
    }

    fn secret(&self, group: &str, name: &str) -> Option<String> {
        SqliteStore::open(&self.database)
            .unwrap()
            .get_secret(group, name)
            .unwrap()
    }
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

fn assert_hex64(value: &str) {
    assert_eq!(value.len(), 64, "expected 64 hex characters, got {value:?}");
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "expected lowercase hex, got {value:?}"
    );
}

#[test]
fn rotate_credentials_changes_one_group_and_leaves_others_alone() {
    let installation = Installation::new();
    let old = [
        ("bookshelf", "access_key_id", "old-access-key"),
        ("bookshelf", "secret_access_key", "old-secret-key"),
        ("bookshelf", "sql_password", "old-sql-password"),
        ("bookshelf", "sql_ro_password", "old-sql-ro-password"),
        ("object_store", "secret", "old-object-store-secret"),
    ];
    installation.seed(&old);

    let output = run_ctl(
        &installation.config,
        "rotate-credentials",
        Some("bookshelf"),
    );
    assert_success(&output);
    let text = stdout(&output);

    for (group, name, old_value) in old.iter().take(4) {
        let new_value = installation.secret(group, name).expect("rotated secret");
        assert_ne!(&new_value, old_value, "{group}.{name} was not rotated");
        assert_hex64(&new_value);
        assert!(
            text.contains(&format!("{group}.{name}")),
            "stdout is missing {group}.{name}: {text}"
        );
        assert!(!text.contains(&new_value), "stdout leaked {group}.{name}");
        assert!(
            !text.contains(old_value),
            "stdout leaked the old {group}.{name}"
        );
    }

    assert_eq!(
        installation.secret("object_store", "secret").as_deref(),
        Some("old-object-store-secret"),
        "rotation must not touch another group"
    );
}

#[test]
fn rotate_credentials_rejects_an_unknown_service_without_changing_anything() {
    let installation = Installation::new();
    installation.seed(&[("object_store", "secret", "keep-this-value")]);

    let output = run_ctl(&installation.config, "rotate-credentials", Some("nope"));
    assert!(!output.status.success());
    // The child test harness writes its own progress to stdout, but the
    // command itself must not have claimed any rotation.
    assert!(!stdout(&output).contains("Rotated"));
    let errors = stderr(&output);
    assert!(
        errors.contains("unknown credential group 'nope'"),
        "stderr: {errors}"
    );
    assert!(errors.contains("bookshelf"), "stderr: {errors}");

    assert_eq!(
        installation.secret("object_store", "secret").as_deref(),
        Some("keep-this-value")
    );
}

#[test]
fn rotate_all_credentials_changes_multiple_groups() {
    let installation = Installation::new();
    let old = [
        ("object_store", "secret", "old-object-store"),
        ("bookshelf", "access_key_id", "old-bookshelf-key"),
        ("redis_lb", "password", "old-redis-password"),
    ];
    installation.seed(&old);

    let output = run_ctl(&installation.config, "rotate-all-credentials", None);
    assert_success(&output);
    let text = stdout(&output);

    for (group, name, old_value) in old {
        let new_value = installation.secret(group, name).expect("rotated secret");
        assert_ne!(new_value, old_value);
        assert_hex64(&new_value);
        assert!(text.contains(&format!("{group}.{name}")));
        assert!(!text.contains(&new_value));
    }
}

#[test]
fn rotate_shared_secrets_resets_the_version_and_rotates_services() {
    let installation = Installation::new();
    installation.seed(&[
        ("shared", "secret", "old-shared-secret"),
        ("shared", "salt", "old-shared-salt"),
        ("shared", "credential_version", "7"),
        ("bookshelf", "access_key_id", "old-bookshelf-key"),
        ("oc_bifrost", "superuser_id", "old-superuser-id"),
    ]);

    let output = run_ctl(&installation.config, "rotate-shared-secrets", None);
    assert_success(&output);
    let text = stdout(&output);

    for name in ["secret", "salt"] {
        let value = installation.secret("shared", name).expect("shared secret");
        assert_ne!(value, format!("old-shared-{name}"));
        assert_hex64(&value);
        assert!(!text.contains(&value), "stdout leaked shared.{name}");
    }

    assert_eq!(
        installation
            .secret("shared", "credential_version")
            .as_deref(),
        Some("0")
    );

    let bookshelf = installation
        .secret("bookshelf", "access_key_id")
        .expect("rotated bookshelf key");
    assert_ne!(bookshelf, "old-bookshelf-key");
    assert_hex64(&bookshelf);

    let superuser_id = installation
        .secret("oc_bifrost", "superuser_id")
        .expect("rotated superuser id");
    assert_ne!(superuser_id, "old-superuser-id");
    assert!(
        uuid_from_dashes(&superuser_id),
        "superuser_id should be a hyphenated UUID, got {superuser_id:?}"
    );

    assert!(text.contains("shared.secret"));
    assert!(text.contains("shared.salt"));
    assert!(text.contains("shared.credential_version"));
    assert!(text.contains("secrets file"), "stdout: {text}");
    assert!(text.contains("restart"), "stdout: {text}");
    assert!(!text.contains("old-shared-secret"));
    assert!(!text.contains("old-shared-salt"));
}

fn uuid_from_dashes(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && value
            .bytes()
            .filter(|byte| *byte != b'-')
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[test]
fn push_jobs_rotation_generates_a_matching_rsa_keypair() {
    let installation = Installation::new();
    installation.seed(&[("push-jobs-server", "sql_password", "old-pushy-sql")]);

    let output = run_ctl(
        &installation.config,
        "rotate-credentials",
        Some("push-jobs-server"),
    );
    assert_success(&output);
    let text = stdout(&output);

    let private_pem = installation
        .secret("push-jobs-server", "pushy_priv_key")
        .expect("private key");
    let public_pem = installation
        .secret("push-jobs-server", "pushy_pub_key")
        .expect("public key");
    assert!(private_pem.contains("PRIVATE KEY"));
    assert!(public_pem.contains("PUBLIC KEY"));

    let _public_key = parse_public_key_pem(&public_pem).expect("parse public key");
    let request = CanonicalRequest {
        method: "GET",
        path: "/organizations/acme/nodes",
        body: b"",
        user_id: "pushy",
        timestamp: "2026-10-25T12:00:00Z",
        api_version: Some("1"),
        version: SignVersion::V1_3,
    };
    let headers = sign_request(&private_pem, &request).expect("sign with private key");
    verify_request(&public_pem, &headers, &request).expect("verify with public key");

    let sql_password = installation
        .secret("push-jobs-server", "sql_password")
        .expect("rotated sql password");
    assert_ne!(sql_password, "old-pushy-sql");
    assert_hex64(&sql_password);
    assert!(text.contains("push-jobs-server.sql_password"));

    // Key material and the names carrying it are never printed.
    assert!(!text.contains(&private_pem));
    assert!(!text.contains(&public_pem));
    assert!(!text.contains("BEGIN PRIVATE KEY"));
    assert!(!text.contains("BEGIN PUBLIC KEY"));
    assert!(!text.contains("pushy_priv_key"));
    assert!(!text.contains("pushy_pub_key"));
}
