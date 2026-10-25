//! Integration tests for `user-edit` and `oc-id-show-app`.
//!
//! The commands are exercised through [`microwave_ctl::dispatch`] so the tests
//! cover the same configuration loading and store wiring as the binaries.
//! Rust's test harness captures `println!` per test thread, so each command runs
//! in a child process (this test binary invoked with `--nocapture`) and the
//! parent asserts on that process's stdout.

#![cfg(unix)]

use microwave_crypto::{hash_password, verify_password, PasswordHash};
use microwave_ctl::{Cli, Command, CtlError};
use microwave_store::{SqliteStore, UserProfile};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const SUPERUSER: &str = "pivotal";
const BEGIN_MARKER: &str = "<<<MICROWAVE-TEST-BEGIN>>>";
const END_MARKER: &str = "<<<MICROWAVE-TEST-END>>>";

struct TestEnv {
    dir: TempDir,
    config_path: PathBuf,
    database_path: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let config_path = dir.path().join("microwave.toml");
        let config = format!(
            "[paths]\ndata_dir = {:?}\n\n[auth]\nsuperuser_name = {:?}\n",
            data_dir.to_string_lossy(),
            SUPERUSER
        );
        std::fs::write(&config_path, config).unwrap();
        Self {
            dir,
            config_path,
            database_path: data_dir.join("microwave.db"),
        }
    }

    fn store(&self) -> SqliteStore {
        SqliteStore::open(&self.database_path).unwrap()
    }

    fn write_input(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn seed_user(
        &self,
        username: &str,
        email: &str,
        admin: bool,
        password: &str,
        profile: &UserProfile,
    ) {
        let hash = hash_password(password).unwrap();
        self.store()
            .create_user_with_profile(
                username,
                Some(email),
                &hash.hash_type,
                &hash.hash,
                &hash.salt,
                admin,
                profile,
            )
            .unwrap();
    }
}

#[derive(Debug)]
enum TestCommand {
    UserEdit {
        user: String,
        input: Option<PathBuf>,
    },
    OcIdShowApp {
        app: String,
    },
}

/// Run a command in a child test process and return its stdout.
fn run(
    env: &TestEnv,
    command: TestCommand,
    visual: Option<&Path>,
) -> (Result<(), CtlError>, String) {
    let stdout_path = env.dir.path().join("command.stdout");
    let result_path = env.dir.path().join("command.result");
    let stderr_path = env.dir.path().join("command.stderr");
    let stdout = std::fs::File::create(&stdout_path).unwrap();
    let stderr = std::fs::File::create(&stderr_path).unwrap();

    let mut process = std::process::Command::new(std::env::current_exe().unwrap());
    process
        .args([
            "--exact",
            "child_dispatch_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr));

    // Keep the child independent of ambient deployment and helper settings.
    for name in [
        "MW_CONFIG",
        "MW_PATHS__DATA_DIR",
        "MW_DATABASE__DRIVER",
        "MW_DATABASE__URL",
        "MW_AUTH__SUPERUSER_NAME",
        "MW_STORAGE__COMPRESSION",
        "MW_CTL_TEST_COMMAND",
        "MW_CTL_TEST_CONFIG",
        "MW_CTL_TEST_INPUT",
        "MW_CTL_TEST_USER",
        "MW_CTL_TEST_APP",
        "MW_CTL_TEST_RESULT",
    ] {
        process.env_remove(name);
    }
    process
        .env("MW_CTL_TEST_CONFIG", &env.config_path)
        .env("MW_CTL_TEST_RESULT", &result_path);

    match &command {
        TestCommand::UserEdit { user, input } => {
            process
                .env("MW_CTL_TEST_COMMAND", "user-edit")
                .env("MW_CTL_TEST_USER", user);
            if let Some(input) = input {
                process.env("MW_CTL_TEST_INPUT", input);
            }
        }
        TestCommand::OcIdShowApp { app } => {
            process
                .env("MW_CTL_TEST_COMMAND", "oc-id-show-app")
                .env("MW_CTL_TEST_APP", app);
        }
    }
    if let Some(visual) = visual {
        process.env("VISUAL", visual);
    }

    let status = process.status().unwrap();
    let captured = std::fs::read_to_string(&stdout_path).unwrap();
    let stderr = std::fs::read_to_string(&stderr_path).unwrap();
    assert!(
        status.success(),
        "child command process failed with {status}\nstdout:\n{captured}\nstderr:\n{stderr}"
    );

    let begin = captured.find(BEGIN_MARKER).unwrap_or_else(|| {
        panic!("command output is missing the begin marker\nstdout:\n{captured}\nstderr:\n{stderr}")
    });
    let end = captured
        .find(END_MARKER)
        .unwrap_or_else(|| panic!("command output is missing the end marker\nstdout:\n{captured}"));
    let body = &captured[begin + BEGIN_MARKER.len()..end];
    let body = body.strip_prefix('\n').unwrap_or(body);

    let encoded = std::fs::read_to_string(&result_path)
        .unwrap_or_else(|error| panic!("command result is missing: {error}\nstdout:\n{captured}"));
    let result = if encoded == "ok" {
        Ok(())
    } else if let Some(message) = encoded.strip_prefix("err:") {
        Err(CtlError::message(message))
    } else {
        panic!("unexpected child result {encoded:?}");
    };
    (result, body.to_string())
}

/// Child-process entry point used by [`run`]. Normal test runs return
/// immediately because the helper environment is absent.
#[test]
fn child_dispatch_helper() {
    let Ok(scenario) = std::env::var("MW_CTL_TEST_COMMAND") else {
        return;
    };
    let config = PathBuf::from(std::env::var("MW_CTL_TEST_CONFIG").unwrap());
    let result_path = PathBuf::from(std::env::var("MW_CTL_TEST_RESULT").unwrap());

    let command = match scenario.as_str() {
        "user-edit" => Command::UserEdit {
            user: std::env::var("MW_CTL_TEST_USER").unwrap(),
            input: std::env::var_os("MW_CTL_TEST_INPUT").map(PathBuf::from),
        },
        "oc-id-show-app" => Command::OcIdShowApp {
            app: std::env::var("MW_CTL_TEST_APP").unwrap(),
        },
        other => panic!("unknown child test command {other}"),
    };

    println!("{BEGIN_MARKER}");
    let result = microwave_ctl::dispatch(Cli {
        config: Some(config),
        command,
    });
    println!("{END_MARKER}");

    let encoded = match &result {
        Ok(()) => "ok".to_string(),
        Err(error) => format!("err:{error}"),
    };
    std::fs::write(result_path, encoded).unwrap();
}

fn profile() -> UserProfile {
    UserProfile {
        display_name: Some("Alice Example".into()),
        first_name: Some("Alice".into()),
        middle_name: None,
        last_name: Some("Example".into()),
        ..Default::default()
    }
}

fn error_message(error: CtlError) -> String {
    error.to_string()
}

#[test]
fn user_edit_input_updates_profile_admin_clear_and_rename() {
    let env = TestEnv::new();
    env.seed_user(
        "alice",
        "alice@example.com",
        false,
        "old-password",
        &profile(),
    );

    let input = env.write_input(
        "edit.json",
        r#"{
            "username": "alice-renamed",
            "email": "new@example.com",
            "admin": true,
            "recovery_authentication_enabled": true,
            "display_name": "Alice Renamed",
            "first_name": null,
            "middle_name": "Q",
            "last_name": "Example"
        }"#,
    );

    let (result, output) = run(
        &env,
        TestCommand::UserEdit {
            user: "alice".into(),
            input: Some(input),
        },
        None,
    );
    result.unwrap();

    assert!(output.contains("Updated user alice-renamed"), "{output}");
    let changed = output
        .lines()
        .find(|line| line.starts_with("changed_fields:"))
        .expect("changed fields line");
    for field in [
        "username",
        "email",
        "admin",
        "recovery_authentication_enabled",
        "display_name",
        "first_name",
        "middle_name",
    ] {
        assert!(changed.contains(field), "{field} missing from {changed}");
    }
    // Unchanged fields are not reported and no values are printed.
    assert!(!changed.contains("last_name"), "{changed}");
    assert!(!output.contains("new@example.com"), "{output}");
    assert!(!output.contains("Alice Renamed"), "{output}");

    let store = env.store();
    assert!(store.find_user("alice").unwrap().is_none());
    let user = store.find_user("alice-renamed").unwrap().unwrap();
    assert_eq!(user.email.as_deref(), Some("new@example.com"));
    assert!(user.admin);
    assert!(user.recovery_authentication_enabled);
    assert_eq!(user.display_name.as_deref(), Some("Alice Renamed"));
    assert_eq!(user.first_name, None);
    assert_eq!(user.middle_name.as_deref(), Some("Q"));
    assert_eq!(user.last_name.as_deref(), Some("Example"));
}

#[test]
fn user_edit_password_produces_a_verifiable_hash_and_is_not_printed() {
    let env = TestEnv::new();
    let old_hash = hash_password("old-password").unwrap();
    env.seed_user(
        "alice",
        "alice@example.com",
        false,
        "old-password",
        &profile(),
    );

    const NEW_PASSWORD: &str = "correct horse battery staple";
    let input = env.write_input(
        "password.json",
        &format!(r#"{{"password": {NEW_PASSWORD:?}}}"#),
    );

    let (result, output) = run(
        &env,
        TestCommand::UserEdit {
            user: "alice".into(),
            input: Some(input),
        },
        None,
    );
    result.unwrap();

    assert!(output.contains("changed_fields: password"), "{output}");
    assert!(!output.contains(NEW_PASSWORD), "password leaked: {output}");

    let user = env.store().find_user("alice").unwrap().unwrap();
    assert_ne!(user.password_hash, old_hash.hash);
    let stored = PasswordHash {
        hash: user.password_hash,
        salt: user.password_salt,
        hash_type: user.hash_type,
    };
    assert!(verify_password(NEW_PASSWORD, &stored));
    assert!(!verify_password("old-password", &stored));
}

#[test]
fn user_edit_refuses_to_demote_the_configured_superuser() {
    let env = TestEnv::new();
    env.seed_user(SUPERUSER, "pivotal@example.com", true, "secret", &profile());

    let input = env.write_input("demote.json", r#"{"admin": false}"#);
    let (result, _) = run(
        &env,
        TestCommand::UserEdit {
            user: SUPERUSER.into(),
            input: Some(input),
        },
        None,
    );
    let message = error_message(result.unwrap_err());

    assert!(
        message.contains("cannot remove the base superuser pivotal"),
        "{message}"
    );
    assert!(env.store().find_user(SUPERUSER).unwrap().unwrap().admin);
}

#[test]
fn user_edit_rejects_bad_documents_and_missing_users() {
    let env = TestEnv::new();
    env.seed_user(
        "alice",
        "alice@example.com",
        false,
        "old-password",
        &profile(),
    );

    let malformed = env.write_input("malformed.json", r#"{"email": }"#);
    let (result, _) = run(
        &env,
        TestCommand::UserEdit {
            user: "alice".into(),
            input: Some(malformed),
        },
        None,
    );
    let message = error_message(result.unwrap_err());
    assert!(message.contains("invalid JSON"), "{message}");

    let unknown = env.write_input("unknown.json", r#"{"nickname": "ali"}"#);
    let (result, _) = run(
        &env,
        TestCommand::UserEdit {
            user: "alice".into(),
            input: Some(unknown),
        },
        None,
    );
    let message = error_message(result.unwrap_err());
    assert!(message.contains("unknown field \"nickname\""), "{message}");

    let wrong_type = env.write_input("wrong_type.json", r#"{"admin": "yes"}"#);
    let (result, _) = run(
        &env,
        TestCommand::UserEdit {
            user: "alice".into(),
            input: Some(wrong_type),
        },
        None,
    );
    let message = error_message(result.unwrap_err());
    assert!(
        message.contains("admin") && message.contains("must be a boolean"),
        "{message}"
    );

    let empty = env.write_input("empty.json", "{}");
    let (result, _) = run(
        &env,
        TestCommand::UserEdit {
            user: "ghost".into(),
            input: Some(empty),
        },
        None,
    );
    let message = error_message(result.unwrap_err());
    assert!(
        message.contains("not found") && message.contains("ghost"),
        "{message}"
    );
}

#[test]
fn user_edit_opens_the_document_in_the_editor() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::new();
    env.seed_user(
        "alice",
        "alice@example.com",
        false,
        "old-password",
        &profile(),
    );

    let editor = env.dir.path().join("fake-editor.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\nprintf '%s\\n' '{\"display_name\": \"Edited Display\"}' > \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o700)).unwrap();

    let (result, output) = run(
        &env,
        TestCommand::UserEdit {
            user: "alice".into(),
            input: None,
        },
        Some(&editor),
    );
    result.unwrap();

    assert!(output.contains("Updated user alice"), "{output}");
    assert!(output.contains("changed_fields: display_name"), "{output}");
    let user = env.store().find_user("alice").unwrap().unwrap();
    assert_eq!(user.display_name.as_deref(), Some("Edited Display"));
}

#[test]
fn oc_id_show_app_prints_the_application_credentials() {
    let env = TestEnv::new();
    let created = env
        .store()
        .create_oauth_application(
            "supermarket",
            "https://supermarket.example.com/auth/chef_oauth2/callback",
            "read write",
            true,
        )
        .unwrap();

    let (result, output) = run(
        &env,
        TestCommand::OcIdShowApp {
            app: "supermarket".into(),
        },
        None,
    );
    result.unwrap();

    let value: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    let object = value.as_object().expect("application object");
    let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "confidential",
            "name",
            "redirect_uri",
            "scopes",
            "secret",
            "uid"
        ]
    );
    assert_eq!(object["name"], "supermarket");
    assert_eq!(object["uid"], created.uid);
    assert_eq!(object["secret"], created.secret);
    assert_eq!(
        object["redirect_uri"],
        "https://supermarket.example.com/auth/chef_oauth2/callback"
    );
    assert_eq!(object["scopes"], "read write");
    assert_eq!(object["confidential"], true);
}

#[test]
fn oc_id_show_app_errors_for_a_missing_application() {
    let env = TestEnv::new();
    env.store()
        .create_oauth_application("supermarket", "https://example.com/callback", "read", true)
        .unwrap();

    let (result, output) = run(
        &env,
        TestCommand::OcIdShowApp {
            app: "missing".into(),
        },
        None,
    );
    let message = error_message(result.unwrap_err());

    assert!(message.contains("not found"), "{message}");
    assert!(message.contains("missing"), "{message}");
    assert!(output.is_empty(), "{output}");
}
