use assert_cmd::Command;
use microwave_crypto::{
    generate_rsa_keypair, parse_private_key_pem, sign_request, verify_password, verify_request,
    CanonicalRequest, PasswordHash, SignVersion,
};
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
        .env_remove("MW_SERVER_BIN")
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

    let refused = run(data.path(), &["restore", backup_path.to_str().unwrap()]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));
    assert!(stdout(&refused).contains("Restore target:"));

    let restored = run(
        data.path(),
        &["restore", backup_path.to_str().unwrap(), "--yes"],
    );
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
fn reindex_reports_relational_authority_for_the_embedded_backend() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["org-create", "acme", "--creator", "bob"],
    ));
    create_client(dir.path(), "acme", "web");

    let output = run(dir.path(), &["reindex"]);
    assert_success(&output);
    let text = stdout(&output);
    assert!(text.contains("search_backend: embedded"));
    assert!(text.contains("relational search is authoritative"));
    assert!(text.contains("organizations: 1"));
    assert!(text.contains("clients: 1"));
    assert!(!text.contains("not implemented"));

    let scoped = run(dir.path(), &["reindex", "--org", "acme"]);
    assert_success(&scoped);
    assert!(stdout(&scoped).contains("organization: acme"));
    assert!(stdout(&scoped).contains("documents_acknowledged: 2"));

    let missing = run(dir.path(), &["reindex", "--org", "missing"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("does not exist"));
}

#[test]
fn reindex_fails_loudly_when_a_remote_backend_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["org-create", "acme", "--creator", "bob"],
    ));

    let config = dir.path().join("opensearch.toml");
    std::fs::write(
        &config,
        "[search]\nbackend = \"opensearch\"\nurl = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "reindex"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("reindex failed"));
    assert!(!stdout(&output).contains("documents_acknowledged"));
}

#[test]
fn reconfigure_validates_initializes_and_preserves_existing_data() {
    let dir = tempfile::tempdir().unwrap();

    let first = run(dir.path(), &["reconfigure"]);
    assert_success(&first);
    let text = stdout(&first);
    assert!(text.contains("configuration is valid"));
    assert!(text.contains("database_driver: sqlite"));
    assert!(text.contains("search_backend: embedded"));
    assert!(text.contains("object_store_backend: local"));
    assert!(text.contains("users: 0"));
    assert!(text.contains("organizations: 0"));
    assert!(text.contains("not restarted"));
    assert!(dir.path().join("microwave.db").is_file());

    assert_success(&run(
        dir.path(),
        &["user-create", "alice", "--password", "alice-password"],
    ));

    let second = run(dir.path(), &["reconfigure"]);
    assert_success(&second);
    assert!(stdout(&second).contains("users: 1"));

    let listed = run(dir.path(), &["user-list"]);
    assert_success(&listed);
    assert!(stdout(&listed).contains("alice"));

    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "[server]\nbind = \"not-an-address\"\n").unwrap();
    let refused = ctl(dir.path())
        .args(["--config", bad.to_str().unwrap(), "reconfigure"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("server.bind"));
    assert!(!stdout(&refused).contains("metadata backend is ready"));
}

#[test]
fn reconfigure_with_bad_config_does_not_create_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "[server]\nbind = \"not-an-address\"\n").unwrap();

    let output = ctl(dir.path())
        .args(["--config", bad.to_str().unwrap(), "reconfigure"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("server.bind"));
    assert!(
        !dir.path().join("microwave.db").exists(),
        "a failed reconfigure must not initialize the database"
    );
}

#[test]
fn tar_backup_and_restore_round_trip() {
    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let archive = backups.path().join("snapshot.tar");

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

    let backed_up = run(
        data.path(),
        &["backup", archive.to_str().unwrap(), "--format", "tar"],
    );
    assert_success(&backed_up);
    assert!(archive.is_file());
    let contents = std::fs::read(&archive).unwrap();
    assert!(!contents.is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&archive).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // A second backup must not silently replace an existing archive.
    let again = run(
        data.path(),
        &["backup", archive.to_str().unwrap(), "--format", "tar"],
    );
    assert!(!again.status.success());
    assert!(stderr(&again).contains("already exists"));

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

    let refused = run(data.path(), &["restore", archive.to_str().unwrap()]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));
    assert!(stdout(&refused).contains("Restore target:"));

    // The refused restore must not have removed or replaced live data.
    let listed = run(data.path(), &["user-list"]);
    assert_success(&listed);
    assert!(stdout(&listed).contains("transient"));
    assert_eq!(std::fs::read(bookshelf.join("blob")).unwrap(), b"mutated");

    let restored = run(
        data.path(),
        &["restore", archive.to_str().unwrap(), "--yes"],
    );
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
fn tar_restore_rejects_unexpected_archive_entries() {
    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let archive = backups.path().join("hostile.tar");

    let file = std::fs::File::create(&archive).unwrap();
    let mut builder = tar::Builder::new(file);
    let mut header = tar::Header::new_gnu();
    header.set_size(4);
    header.set_mode(0o600);
    header.set_cksum();
    builder
        .append_data(&mut header, "pivotal.pem", &b"evil"[..])
        .unwrap();
    builder.finish().unwrap();

    let output = run(
        data.path(),
        &["restore", archive.to_str().unwrap(), "--yes"],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("unexpected entry"));
    assert!(!data.path().join("pivotal.pem").exists());

    let unknown_extension = backups.path().join("unknown.bin");
    std::fs::write(&unknown_extension, b"not a tar archive").unwrap();
    let output = run(
        data.path(),
        &[
            "restore",
            unknown_extension.to_str().unwrap(),
            "--yes",
            "--format",
            "tar",
        ],
    );
    assert!(!output.status.success());
    assert!(!stdout(&output).contains("Restored from"));
}

#[test]
fn help_lists_the_operational_surface() {
    let output = Command::cargo_bin("microwave-ctl")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    assert_success(&output);
    let text = stdout(&output);
    for command in ["reconfigure", "reindex", "pedant", "service", "backup"] {
        assert!(text.contains(command), "help is missing {command}: {text}");
    }
}

#[test]
fn service_help_documents_foreground_and_the_config_path() {
    let output = Command::cargo_bin("microwave-ctl")
        .unwrap()
        .args(["service", "start", "--help"])
        .output()
        .unwrap();
    assert_success(&output);
    let text = stdout(&output);
    for expected in ["--foreground", "--config", "--unit", "start"] {
        assert!(
            text.contains(expected),
            "service help is missing {expected}: {text}"
        );
    }
}

#[cfg(unix)]
mod operational_process_tests {
    use super::{assert_success, ctl, run, stderr, stdout};
    use std::ffi::CString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn fake_tool(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            "#!/bin/sh\n\
             if [ -n \"$MW_TEST_LOG\" ]; then\n\
               printf '%s\\n' \"$@\" >> \"$MW_TEST_LOG\"\n\
             fi\n\
             exit \"${MW_TEST_EXIT:-0}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn pedant_tags(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn pedant_passes_tags_as_separate_arguments_and_propagates_the_status() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake_tool(dir.path(), "run-pedant.sh");
        let log = dir.path().join("pedant.log");

        let output = ctl(dir.path())
            .env("MW_PEDANT_SCRIPT", &script)
            .env("MW_TEST_LOG", &log)
            .env("MW_TEST_EXIT", "3")
            .args(["pedant", "users", "nodes", "core", "two words; rm -rf /"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(
            pedant_tags(&log),
            vec![
                "users".to_string(),
                "nodes".to_string(),
                "core".to_string(),
                "two words; rm -rf /".to_string(),
            ]
        );

        let success = ctl(dir.path())
            .env("MW_PEDANT_SCRIPT", &script)
            .env("MW_TEST_LOG", &log)
            .arg("pedant")
            .output()
            .unwrap();
        assert_success(&success);

        let missing = ctl(dir.path())
            .env("MW_PEDANT_SCRIPT", dir.path().join("missing.sh"))
            .arg("pedant")
            .output()
            .unwrap();
        assert!(!missing.status.success());
        assert!(stderr(&missing).contains("MW_PEDANT_SCRIPT"));
    }

    #[test]
    fn service_delegates_to_systemctl_with_direct_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let systemctl = fake_tool(dir.path(), "systemctl");
        let log = dir.path().join("systemctl.log");

        let output = ctl(dir.path())
            .env("MW_SYSTEMCTL", &systemctl)
            .env("MW_TEST_LOG", &log)
            .args(["service", "restart", "--unit", "demo.service"])
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(
            pedant_tags(&log),
            vec!["restart".to_string(), "demo.service".to_string()]
        );

        let failed = ctl(dir.path())
            .env("MW_SYSTEMCTL", &systemctl)
            .env("MW_TEST_LOG", &log)
            .env("MW_TEST_EXIT", "5")
            .args(["service", "status", "--unit", "demo.service"])
            .output()
            .unwrap();
        assert_eq!(failed.status.code(), Some(5));
        assert!(stdout(&failed).is_empty());

        let unavailable = ctl(dir.path())
            .env("MW_SYSTEMCTL", "/definitely/not/here/systemctl")
            .args(["service", "start"])
            .output()
            .unwrap();
        assert!(!unavailable.status.success());
        let text = stderr(&unavailable);
        assert!(text.contains("no systemd service manager"));
        assert!(text.contains("microwave-server serve"));
        assert!(text.contains("service start --foreground"));
    }

    #[test]
    fn service_rejects_option_like_units_before_running_systemctl() {
        let dir = tempfile::tempdir().unwrap();
        let output = ctl(dir.path())
            .env("MW_SYSTEMCTL", "/definitely/not/here/systemctl")
            .env("MW_SERVICE_UNIT", "-injected.service")
            .args(["service", "start"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let text = stderr(&output);
        assert!(text.contains("must not start with '-'"), "stderr: {text}");
        assert!(!text.contains("no systemd service manager"));
    }

    #[test]
    fn service_tail_uses_journalctl_without_requiring_systemctl() {
        let dir = tempfile::tempdir().unwrap();
        let journalctl = fake_tool(dir.path(), "journalctl");
        let log = dir.path().join("journalctl.log");

        let output = ctl(dir.path())
            .env("MW_SYSTEMCTL", "/definitely/not/here/systemctl")
            .env("MW_JOURNALCTL", &journalctl)
            .env("MW_TEST_LOG", &log)
            .args([
                "service",
                "tail",
                "--unit",
                "demo.service",
                "--lines",
                "5",
                "--follow",
            ])
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(
            pedant_tags(&log),
            vec![
                "-u".to_string(),
                "demo.service".to_string(),
                "-n".to_string(),
                "5".to_string(),
                "-f".to_string()
            ]
        );

        let invalid = ctl(dir.path())
            .env("MW_SYSTEMCTL", "/definitely/not/here/systemctl")
            .env("MW_JOURNALCTL", &journalctl)
            .args(["service", "tail", "--lines", "0"])
            .output()
            .unwrap();
        assert!(!invalid.status.success());
        assert!(stderr(&invalid).contains("greater than zero"));
    }

    #[test]
    fn service_start_foreground_execs_the_server_with_direct_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let server = fake_tool(dir.path(), "microwave-server");
        let log = dir.path().join("server.log");
        // A space in the path proves the config is passed as one argument
        // rather than concatenated into a shell command.
        let config = dir.path().join("config with space.toml");
        std::fs::write(&config, "[server]\nbind = \"127.0.0.1:8000\"\n").unwrap();

        let output = ctl(dir.path())
            .env("MW_SERVER_BIN", &server)
            .env("MW_TEST_LOG", &log)
            .args([
                "service",
                "start",
                "--foreground",
                "--config",
                config.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(
            pedant_tags(&log),
            vec![
                "serve".to_string(),
                "--config".to_string(),
                config.to_str().unwrap().to_string()
            ]
        );

        // The CLI execs the fake server, so its exit status is the command's.
        let failed = ctl(dir.path())
            .env("MW_SERVER_BIN", &server)
            .env("MW_TEST_LOG", &log)
            .env("MW_TEST_EXIT", "7")
            .args([
                "service",
                "start",
                "--foreground",
                "--config",
                config.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert_eq!(failed.status.code(), Some(7));
        assert!(stdout(&failed).is_empty());
    }

    #[test]
    fn service_start_honours_the_foreground_environment_alternative() {
        let dir = tempfile::tempdir().unwrap();
        let server = fake_tool(dir.path(), "microwave-server");
        let log = dir.path().join("server.log");
        let config = dir.path().join("microwave.toml");
        std::fs::write(&config, "[server]\nbind = \"127.0.0.1:8000\"\n").unwrap();

        let output = ctl(dir.path())
            .env("MW_SERVER_BIN", &server)
            .env("MW_SERVICE_FOREGROUND", "1")
            .env("MW_TEST_LOG", &log)
            .args(["--config", config.to_str().unwrap(), "service", "start"])
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(
            pedant_tags(&log),
            vec![
                "serve".to_string(),
                "--config".to_string(),
                config.to_str().unwrap().to_string()
            ]
        );
    }

    #[test]
    fn service_foreground_is_rejected_for_non_start_actions() {
        let dir = tempfile::tempdir().unwrap();
        let systemctl = fake_tool(dir.path(), "systemctl");
        let server = fake_tool(dir.path(), "microwave-server");
        let log = dir.path().join("tool.log");

        for action in ["stop", "restart", "status", "tail"] {
            let output = ctl(dir.path())
                .env("MW_SYSTEMCTL", &systemctl)
                .env("MW_SERVER_BIN", &server)
                .env("MW_SERVICE_FOREGROUND", "1")
                .env("MW_TEST_LOG", &log)
                .args(["service", action, "--foreground"])
                .output()
                .unwrap();
            assert!(
                !output.status.success(),
                "{action} --foreground should fail: {}",
                stdout(&output)
            );
            assert!(
                stderr(&output).contains("only valid for `service start`"),
                "unexpected stderr for {action}: {}",
                stderr(&output)
            );
        }
        assert!(
            !log.exists(),
            "no tool should run when --foreground is rejected"
        );

        // The environment alternative is scoped to `start`, so setting it in a
        // container image must not break systemd control commands.
        let status = ctl(dir.path())
            .env("MW_SYSTEMCTL", &systemctl)
            .env("MW_SERVICE_FOREGROUND", "1")
            .env("MW_TEST_LOG", &log)
            .args(["service", "status", "--unit", "demo.service"])
            .output()
            .unwrap();
        assert_success(&status);
        assert_eq!(
            pedant_tags(&log),
            vec!["status".to_string(), "demo.service".to_string()]
        );
    }

    #[test]
    fn service_foreground_reports_bad_server_binary_and_config_paths() {
        let dir = tempfile::tempdir().unwrap();
        let server = fake_tool(dir.path(), "microwave-server");
        let log = dir.path().join("server.log");

        let missing_binary = ctl(dir.path())
            .env("MW_SERVER_BIN", "/definitely/not/here/microwave-server")
            .env("MW_TEST_LOG", &log)
            .args(["service", "start", "--foreground"])
            .output()
            .unwrap();
        assert!(!missing_binary.status.success());
        assert!(stderr(&missing_binary).contains("MW_SERVER_BIN"));

        let missing_config = ctl(dir.path())
            .env("MW_SERVER_BIN", &server)
            .env("MW_TEST_LOG", &log)
            .args([
                "service",
                "start",
                "--foreground",
                "--config",
                dir.path().join("missing.toml").to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!missing_config.status.success());
        assert!(stderr(&missing_config).contains("cannot read --config"));
        assert!(
            !log.exists(),
            "the server must not start with an unreadable config"
        );

        let unit = ctl(dir.path())
            .env("MW_SERVER_BIN", &server)
            .env("MW_TEST_LOG", &log)
            .args(["service", "start", "--foreground", "--unit", "demo.service"])
            .output()
            .unwrap();
        assert!(!unit.status.success());
        assert!(stderr(&unit).contains("--unit is not valid"));
    }

    #[test]
    fn tar_backup_rejects_symlinked_bookshelf_content() {
        let data = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("outside-secret");
        std::fs::write(&secret, b"do-not-archive").unwrap();

        let bookshelf = data.path().join("bookshelf");
        std::fs::create_dir_all(&bookshelf).unwrap();
        std::os::unix::fs::symlink(&secret, bookshelf.join("leaked")).unwrap();
        let archive = backups.path().join("booked.tar");

        let mut command = ctl(data.path());
        command.timeout(Duration::from_secs(30)).args([
            "backup",
            archive.to_str().unwrap(),
            "--tar",
        ]);
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(stderr(&output).contains("refusing to copy symlink"));
        assert!(!archive.exists());

        // The target must still be intact and was not archived anywhere.
        assert_eq!(std::fs::read(&secret).unwrap(), b"do-not-archive");
    }

    #[test]
    fn tar_backup_rejects_a_fifo_without_blocking() {
        let data = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        let bookshelf = data.path().join("bookshelf");
        std::fs::create_dir_all(&bookshelf).unwrap();
        let fifo = bookshelf.join("pipe");
        let fifo_c = CString::new(fifo.to_str().unwrap()).unwrap();
        let result = unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) };
        assert_eq!(
            result,
            0,
            "mkfifo failed: {}",
            std::io::Error::last_os_error()
        );
        let archive = backups.path().join("booked.tar");

        let mut command = ctl(data.path());
        command.timeout(Duration::from_secs(30)).args([
            "backup",
            archive.to_str().unwrap(),
            "--tar",
        ]);
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(stderr(&output).contains("not a regular file or directory"));
        assert!(!archive.exists());
    }

    /// Create live data, take a directory backup, then mutate the live
    /// installation so a failed restore would be visible.
    fn seeded_directory_backup(data: &Path, backups: &Path) -> PathBuf {
        let bookshelf = data.join("bookshelf");
        std::fs::create_dir_all(&bookshelf).unwrap();
        std::fs::write(bookshelf.join("blob"), b"live-original").unwrap();
        assert_success(&run(
            data,
            &[
                "user-create",
                "persisted",
                "--password",
                "persisted-password",
            ],
        ));

        let backup = backups.join("snapshot");
        assert_success(&run(data, &["backup", backup.to_str().unwrap()]));

        assert_success(&run(
            data,
            &[
                "user-create",
                "transient",
                "--password",
                "transient-password",
            ],
        ));
        std::fs::write(bookshelf.join("blob"), b"live-mutated").unwrap();
        std::fs::write(bookshelf.join("extra"), b"live-extra").unwrap();
        backup
    }

    fn assert_live_data_unchanged(data: &Path) {
        let listed = run(data, &["user-list"]);
        assert_success(&listed);
        let text = stdout(&listed);
        assert!(text.contains("persisted"), "stdout: {text}");
        assert!(text.contains("transient"), "stdout: {text}");

        let bookshelf = data.join("bookshelf");
        assert_eq!(
            std::fs::read(bookshelf.join("blob")).unwrap(),
            b"live-mutated"
        );
        assert_eq!(
            std::fs::read(bookshelf.join("extra")).unwrap(),
            b"live-extra"
        );
        assert!(!bookshelf.join("leaked").exists());
    }

    #[test]
    fn directory_restore_rejects_a_symlink_before_touching_live_data() {
        let data = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("outside-secret");
        std::fs::write(&secret, b"do-not-restore").unwrap();

        let backup = seeded_directory_backup(data.path(), backups.path());
        std::os::unix::fs::symlink(&secret, backup.join("bookshelf").join("leaked")).unwrap();

        let mut command = ctl(data.path());
        command.timeout(Duration::from_secs(30)).args([
            "restore",
            backup.to_str().unwrap(),
            "--yes",
        ]);
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("refusing to copy symlink"),
            "stderr: {}",
            stderr(&output)
        );

        assert_live_data_unchanged(data.path());
        assert_eq!(std::fs::read(&secret).unwrap(), b"do-not-restore");
    }

    #[test]
    fn directory_restore_rejects_a_fifo_before_touching_live_data() {
        let data = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        let backup = seeded_directory_backup(data.path(), backups.path());
        let fifo = backup.join("bookshelf").join("pipe");
        let fifo_c = CString::new(fifo.to_str().unwrap()).unwrap();
        let result = unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) };
        assert_eq!(
            result,
            0,
            "mkfifo failed: {}",
            std::io::Error::last_os_error()
        );

        let mut command = ctl(data.path());
        command.timeout(Duration::from_secs(30)).args([
            "restore",
            backup.to_str().unwrap(),
            "--yes",
        ]);
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("not a regular file or directory"),
            "stderr: {}",
            stderr(&output)
        );

        assert_live_data_unchanged(data.path());
    }
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
    let refused = run(
        target.path(),
        &["migrate-from-legacy", "--input", snapshot.to_str().unwrap()],
    );
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));
    assert!(!target.path().join("microwave.db").exists());

    let imported = run(
        target.path(),
        &[
            "migrate-from-legacy",
            "--input",
            snapshot.to_str().unwrap(),
            "--yes",
        ],
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

fn create_client(dir: &Path, org_name: &str, client_name: &str) {
    let store = SqliteStore::open(&dir.join("microwave.db")).unwrap();
    let org = store.find_org(org_name).unwrap().unwrap();
    store
        .create_client(&org.id, client_name, false, false)
        .unwrap();
}

fn extract_private_key(output: &str) -> String {
    const BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
    const END: &str = "-----END PRIVATE KEY-----";
    let start = output.find(BEGIN).expect("private key start marker");
    let end = output[start..].find(END).expect("private key end marker") + start + END.len();
    output[start..end].to_string()
}

#[test]
fn server_admin_permissions_can_be_granted_listed_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "alice", "--password", "alice-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["user-create", "pivotal", "--password", "pivotal-password"],
    ));

    let empty = run(dir.path(), &["list-server-admins"]);
    assert_success(&empty);
    assert!(!stdout(&empty).contains("alice"));

    let granted = run(dir.path(), &["grant-server-admin-permissions", "alice"]);
    assert_success(&granted);
    assert!(stdout(&granted).contains("alice"));
    assert!(stdout(&granted).contains("server-admins"));

    let listed = run(dir.path(), &["list-server-admins"]);
    assert_success(&listed);
    assert!(stdout(&listed).lines().any(|line| line.trim() == "alice"));

    let shown = run(dir.path(), &["user-show", "alice"]);
    assert_success(&shown);
    assert!(stdout(&shown).contains("admin: true"));

    let removed = run(dir.path(), &["remove-server-admin-permissions", "alice"]);
    assert_success(&removed);
    assert!(stdout(&removed).contains("alice"));

    let listed = run(dir.path(), &["list-server-admins"]);
    assert_success(&listed);
    assert!(!stdout(&listed).contains("alice"));

    let removed_again = run(dir.path(), &["remove-server-admin-permissions", "alice"]);
    assert!(!removed_again.status.success());
    assert!(stderr(&removed_again).contains("not a member of server-admins"));

    assert_success(&run(
        dir.path(),
        &["grant-server-admin-permissions", "pivotal"],
    ));
    let refused = run(dir.path(), &["remove-server-admin-permissions", "pivotal"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("cannot remove"));

    let missing = run(dir.path(), &["grant-server-admin-permissions", "nobody"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("not found"));
}

#[test]
fn client_keys_can_be_created_listed_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["org-create", "acme", "--creator", "bob"],
    ));
    create_client(dir.path(), "acme", "web");

    let created = run(
        dir.path(),
        &["add-client-key", "acme", "web", "--key-name", "deploy"],
    );
    assert_success(&created);
    let text = stdout(&created);
    assert!(text.contains("Created key deploy for client web in organization acme"));
    assert!(text.contains("not saved in the database"));
    assert!(!text.contains("BEGIN PUBLIC KEY"));
    let private_pem = extract_private_key(&text);
    parse_private_key_pem(&private_pem).unwrap();

    let store = SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
    let org = store.find_org("acme").unwrap().unwrap();
    let client = store.find_client(&org.id, "web").unwrap().unwrap();
    let stored = store
        .find_actor_key(&client.id, ActorType::Client, "deploy")
        .unwrap()
        .unwrap();
    drop(store);

    let request = CanonicalRequest {
        method: "GET",
        path: "/organizations/acme/clients/web",
        body: b"",
        user_id: "web",
        timestamp: "2026-10-24T12:00:00Z",
        api_version: Some("1"),
        version: SignVersion::V1_3,
    };
    let headers = sign_request(&private_pem, &request).unwrap();
    verify_request(&stored.public_key, &headers, &request).unwrap();

    let listed = run(dir.path(), &["list-client-keys", "acme", "web"]);
    assert_success(&listed);
    let text = stdout(&listed);
    assert!(text.contains("name: deploy"));
    assert!(text.contains("expired: false"));
    assert!(!text.contains("BEGIN PUBLIC KEY"));

    let verbose = run(
        dir.path(),
        &["list-client-keys", "acme", "web", "--verbose"],
    );
    assert_success(&verbose);
    let text = stdout(&verbose);
    assert!(text.contains("name: deploy"));
    assert!(text.contains("expiration_date: infinity"));
    assert!(text.contains("BEGIN PUBLIC KEY"));

    let deleted = run(dir.path(), &["delete-client-key", "acme", "web", "deploy"]);
    assert_success(&deleted);
    assert!(stdout(&deleted).contains("Deleted key deploy"));

    let listed = run(dir.path(), &["list-client-keys", "acme", "web"]);
    assert_success(&listed);
    assert!(!stdout(&listed).contains("name: deploy"));

    let deleted_again = run(dir.path(), &["delete-client-key", "acme", "web", "deploy"]);
    assert!(!deleted_again.status.success());
    assert!(stderr(&deleted_again).contains("not found"));

    let missing_client = run(dir.path(), &["add-client-key", "acme", "ghost"]);
    assert!(!missing_client.status.success());
    assert!(stderr(&missing_client).contains("not found"));

    // Without --key-name the key name defaults to the public key fingerprint.
    let generated = run(dir.path(), &["add-client-key", "acme", "web"]);
    assert_success(&generated);
    let text = stdout(&generated);
    assert!(text.contains("Created key "));
    let generated_name = text
        .lines()
        .find_map(|line| line.strip_prefix("Created key "))
        .and_then(|line| line.split(" for client").next())
        .expect("generated key name")
        .to_string();
    assert!(generated_name.contains(':'));

    let listed = run(dir.path(), &["list-client-keys", "acme", "web"]);
    assert_success(&listed);
    assert_eq!(stdout(&listed).matches("name: ").count(), 1);
    assert!(stdout(&listed).contains(&format!("name: {generated_name}")));

    let duplicate = run(
        dir.path(),
        &[
            "add-client-key",
            "acme",
            "web",
            "--key-name",
            &generated_name,
        ],
    );
    assert!(!duplicate.status.success());
    assert!(stderr(&duplicate).contains("already exists"));
}

#[test]
fn client_keys_can_be_imported_from_a_public_key_file() {
    let dir = tempfile::tempdir().unwrap();
    let keys = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["org-create", "acme", "--creator", "bob"],
    ));
    create_client(dir.path(), "acme", "web");

    let (_, public_key) = generate_rsa_keypair().unwrap();
    let public_path = keys.path().join("web.pub");
    std::fs::write(&public_path, &public_key).unwrap();

    let created = run(
        dir.path(),
        &[
            "add-client-key",
            "acme",
            "web",
            "--key-name",
            "imported",
            "--public-key-path",
            public_path.to_str().unwrap(),
        ],
    );
    assert_success(&created);
    assert!(!stdout(&created).contains("PRIVATE KEY"));

    let verbose = run(
        dir.path(),
        &["list-client-keys", "acme", "web", "--verbose"],
    );
    assert_success(&verbose);
    assert!(stdout(&verbose).contains("name: imported"));
    assert!(stdout(&verbose).contains(public_key.trim()));
}

#[test]
fn client_key_expiration_is_listed() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "bob", "--password", "bob-password"],
    ));
    assert_success(&run(
        dir.path(),
        &["org-create", "acme", "--creator", "bob"],
    ));
    create_client(dir.path(), "acme", "web");

    assert_success(&run(
        dir.path(),
        &[
            "add-client-key",
            "acme",
            "web",
            "--key-name",
            "future",
            "--expiration-date",
            "2050-12-24T21:00:00Z",
        ],
    ));
    assert_success(&run(
        dir.path(),
        &[
            "add-client-key",
            "acme",
            "web",
            "--key-name",
            "past",
            "--expiration-date",
            "2012-12-24T21:00:00Z",
        ],
    ));

    let verbose = run(
        dir.path(),
        &["list-client-keys", "acme", "web", "--verbose"],
    );
    assert_success(&verbose);
    assert!(stdout(&verbose).contains("name: future"));
    assert!(stdout(&verbose).contains("expiration_date: 2050-12-24T21:00:00Z"));
    assert!(stdout(&verbose).contains("name: past"));

    let simple = run(dir.path(), &["list-client-keys", "acme", "web"]);
    assert_success(&simple);
    let text = stdout(&simple);
    assert!(text.contains("name: past\nexpired: true"));

    let invalid = run(
        dir.path(),
        &[
            "add-client-key",
            "acme",
            "web",
            "--key-name",
            "bad",
            "--expiration-date",
            "tomorrow",
        ],
    );
    assert!(!invalid.status.success());
    assert!(stderr(&invalid).contains("expiration-date"));
}

#[test]
fn user_keys_can_be_created_listed_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    assert_success(&run(
        dir.path(),
        &["user-create", "alice", "--password", "alice-password"],
    ));

    let created = run(
        dir.path(),
        &["add-user-key", "alice", "--key-name", "laptop"],
    );
    assert_success(&created);
    let text = stdout(&created);
    assert!(text.contains("Created key laptop for user alice"));
    assert!(text.contains("BEGIN PRIVATE KEY"));

    let listed = run(dir.path(), &["list-user-keys", "alice"]);
    assert_success(&listed);
    assert!(stdout(&listed).contains("name: laptop"));
    assert!(stdout(&listed).contains("expired: false"));

    let verbose = run(dir.path(), &["list-user-keys", "alice", "--verbose"]);
    assert_success(&verbose);
    assert!(stdout(&verbose).contains("expiration_date: infinity"));
    assert!(stdout(&verbose).contains("BEGIN PUBLIC KEY"));

    let deleted = run(dir.path(), &["delete-user-key", "alice", "laptop"]);
    assert_success(&deleted);
    assert!(stdout(&deleted).contains("Deleted key laptop"));

    let listed = run(dir.path(), &["list-user-keys", "alice"]);
    assert_success(&listed);
    assert!(!stdout(&listed).contains("name: laptop"));

    let missing = run(dir.path(), &["add-user-key", "nobody"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("not found"));
}

#[test]
fn secrets_can_be_set_shown_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let secret = "s3cr3t-bookshelf-hmac";

    let set = run(
        dir.path(),
        &["set-secret", "object_store", "secret", secret],
    );
    assert_success(&set);
    assert!(!stdout(&set).contains(secret));
    assert!(stdout(&set).contains("object_store.secret"));

    let shown = run(dir.path(), &["show-secret", "object_store", "secret"]);
    assert_success(&shown);
    assert_eq!(stdout(&shown).trim(), secret);

    assert_success(&run(
        dir.path(),
        &["set-secret", "bookshelf", "access_key_id", "access-key"],
    ));
    assert_success(&run(
        dir.path(),
        &["set-secret", "bookshelf", "secret_access_key", "secret-key"],
    ));
    let shown = run(dir.path(), &["show-secret", "bookshelf", "access_key_id"]);
    assert_success(&shown);
    assert_eq!(stdout(&shown).trim(), "access-key");

    let overwritten = run(
        dir.path(),
        &["set-secret", "object_store", "secret", "rotated-value"],
    );
    assert_success(&overwritten);
    let shown = run(dir.path(), &["show-secret", "object_store", "secret"]);
    assert_success(&shown);
    assert_eq!(stdout(&shown).trim(), "rotated-value");

    let removed = run(dir.path(), &["remove-secret", "object_store", "secret"]);
    assert_success(&removed);
    assert!(stdout(&removed).contains("object_store.secret"));

    let shown = run(dir.path(), &["show-secret", "object_store", "secret"]);
    assert!(!shown.status.success());
    assert!(stderr(&shown).contains("not found"));

    let unknown = run(dir.path(), &["set-secret", "bogus", "value", "x"]);
    assert!(!unknown.status.success());
    assert!(stderr(&unknown).contains("unknown credential"));

    let unknown_name = run(dir.path(), &["set-secret", "object_store", "nope", "x"]);
    assert!(!unknown_name.status.success());
    assert!(stderr(&unknown_name).contains("unknown credential"));
}

#[test]
fn maintenance_mode_can_be_turned_on_and_off() {
    let dir = tempfile::tempdir().unwrap();

    let status = run(dir.path(), &["maintenance", "status"]);
    assert_success(&status);
    assert!(stdout(&status).contains("maintenance: off"));
    assert!(!stdout(&status).contains("not implemented"));

    let on = run(dir.path(), &["maintenance", "on"]);
    assert_success(&on);
    assert!(stdout(&on).contains("enabled"));
    assert!(!stdout(&on).contains("not implemented"));

    let status = run(dir.path(), &["maintenance", "status"]);
    assert_success(&status);
    assert!(stdout(&status).contains("maintenance: on"));

    let off = run(dir.path(), &["maintenance", "off"]);
    assert_success(&off);
    assert!(stdout(&off).contains("disabled"));

    let status = run(dir.path(), &["maintenance", "status"]);
    assert_success(&status);
    assert!(stdout(&status).contains("maintenance: off"));

    let invalid = run(dir.path(), &["maintenance", "sideways"]);
    assert!(!invalid.status.success());
    assert!(stderr(&invalid).contains("invalid value"));
}

#[test]
fn check_config_reports_valid_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &["check-config"]);
    assert_success(&output);
    let text = stdout(&output);
    assert!(text.contains("ok: data_dir"));
    assert!(text.contains("ok: database"));
    assert!(text.contains("ok: server.bind"));
    assert!(text.contains("ok: tls disabled"));
    assert!(text.contains("ok: auth.superuser_name"));
}

#[test]
fn check_config_rejects_incomplete_external_backends() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        "[server]\nbind = \"not-an-address\"\n\n[object_store]\nbackend = \"s3\"\n",
    )
    .unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("server.bind"));
    assert!(text.contains("object_store.endpoint"));
}

#[test]
fn check_config_rejects_invalid_data_collector_settings() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        "[data_collector]\nroot_url = \"ftp://automate.example.com/ingest\"\ntimeout = 0\n",
    )
    .unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("data_collector.root_url"));
    assert!(text.contains("data_collector.timeout"));
}

#[test]
fn check_config_rejects_required_recipe_without_a_path() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(&config, "[required_recipe]\nenabled = true\n").unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("required_recipe.path"));
}

#[test]
fn check_config_rejects_missing_required_recipe_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        format!(
            "[required_recipe]\nenabled = true\npath = {:?}\n",
            dir.path().join("missing.rb")
        ),
    )
    .unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("required_recipe.path"));
    assert!(stderr(&output).contains("cannot read file"));
}

#[cfg(unix)]
#[test]
fn check_config_rejects_group_or_world_writable_required_recipe_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let recipe = dir.path().join("required_recipe.rb");
    std::fs::write(&recipe, "# recipe\n").unwrap();
    std::fs::set_permissions(&recipe, std::fs::Permissions::from_mode(0o666)).unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        format!("[required_recipe]\nenabled = true\npath = {:?}\n", recipe),
    )
    .unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("group- or world-writable"));
}

#[test]
fn check_config_accepts_a_safe_required_recipe_file() {
    let dir = tempfile::tempdir().unwrap();
    let recipe = dir.path().join("required_recipe.rb");
    std::fs::write(&recipe, "# recipe\n").unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        format!("[required_recipe]\nenabled = true\npath = {:?}\n", recipe),
    )
    .unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(stdout(&output).contains("required_recipe.path"));
}

#[test]
fn check_config_redacts_data_collector_userinfo() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        "[data_collector]\nroot_url = \"https://user:supersecret@automate.example.com/ingest\"\n",
    )
    .unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert_success(&output);
    let text = stdout(&output);
    assert!(text.contains("https://automate.example.com"));
    assert!(!text.contains("supersecret"));
    assert!(!text.contains("user:"));
}

#[test]
fn check_config_rejects_hostless_data_collector_urls() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(&config, "[data_collector]\nroot_url = \"http://?foo\"\n").unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("data_collector.root_url"));
}

#[test]
fn check_config_accepts_a_postgres_url_without_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let output = ctl(dir.path())
        .env("MW_DATABASE__DRIVER", "postgres")
        .env(
            "MW_DATABASE__URL",
            "postgres://microwave:secret@localhost/microwave",
        )
        .arg("check-config")
        .output()
        .unwrap();
    assert_success(&output);
    assert!(stdout(&output).contains("ok: database"));
}

#[test]
fn ha_status_reports_a_single_node() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &["ha-status"]);
    assert_success(&output);
    let text = stdout(&output);
    assert!(text.contains("mode: standalone"));
    assert!(text.contains("nodes: 1"));
    assert!(text.contains("not implemented"));
}

#[test]
fn psql_executes_against_the_configured_sqlite_database() {
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

    let output = ctl(dir.path())
        .args(["psql", "select count(*) from users;"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(stdout(&output).trim(), "1");
}

#[test]
fn chef_server_ctl_alias_exposes_the_new_commands() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("chef-server-ctl")
        .unwrap()
        .env_remove("MW_CONFIG")
        .env_remove("MW_DATABASE__DRIVER")
        .env_remove("MW_DATABASE__URL")
        .env_remove("MW_AUTH__SUPERUSER_NAME")
        .env("MW_PATHS__DATA_DIR", dir.path())
        .args(["maintenance", "status"])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(stdout(&output).contains("maintenance: off"));
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
        &[
            "migrate-from-legacy",
            "--input",
            snapshot.to_str().unwrap(),
            "--yes",
        ],
    );
    assert!(!imported.status.success());
    assert!(stderr(&imported).contains("unsupported snapshot version 99"));
}

// ---------------------------------------------------------------------------
// PostgreSQL-gated CLI coverage
// ---------------------------------------------------------------------------

fn postgres_url() -> Option<String> {
    let url = std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())?;
    Some(url)
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{}-{nanos}", std::process::id())
}

fn pg_ctl(dir: &Path, url: &str) -> Command {
    let mut command = ctl(dir);
    command
        .env("MW_DATABASE__DRIVER", "postgres")
        .env("MW_DATABASE__URL", url);
    command
}

fn run_pg(dir: &Path, url: &str, args: &[&str]) -> Output {
    pg_ctl(dir, url).args(args).output().unwrap()
}

fn pg_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Require a host PostgreSQL client tool when the test database is configured.
///
/// Missing tools are a hard failure so the PostgreSQL suite cannot silently
/// report success while skipping backup/restore/psql coverage. Set
/// `MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS=1` to opt out explicitly on a host
/// that only has Docker.
fn pg_client_tool_available(tool: &str) -> bool {
    if std::process::Command::new(tool)
        .arg("--version")
        .output()
        .is_ok()
    {
        return true;
    }
    if std::env::var("MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS").as_deref() == Ok("1") {
        eprintln!(
            "SKIP: {tool} is not installed and MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS=1 was set explicitly"
        );
        return false;
    }
    panic!(
        "{tool} is required for the PostgreSQL CLI tests; install the PostgreSQL client tools \
         or set MICROWAVE_TEST_SKIP_PG_CLIENT_TOOLS=1 to skip backup/restore/psql coverage explicitly"
    );
}

/// Create a uniquely named database from the configured test server and return
/// its URL. The configured database is used only as the administrative
/// connection; tests that import snapshots or restore dumps need an empty
/// target so global groups and ids cannot collide with other tests.
fn scratch_database(base_url: &str, prefix: &str) -> (String, String) {
    let name = unique(prefix).replace('-', "_");
    let runtime = pg_runtime();
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
    let runtime = pg_runtime();
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

#[test]
fn postgres_status_and_user_lifecycle() {
    let Some(url) = postgres_url() else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let superuser = unique("pg-super");
    let username = unique("pg-user");
    // Ensure the schema exists so `status` reports a readable database; status
    // itself is read-only and does not create tables.
    pg_runtime()
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect to configured PostgreSQL");
    let status = || {
        pg_ctl(dir.path(), &url)
            .env("MW_AUTH__SUPERUSER_NAME", &superuser)
            .arg("status")
            .output()
            .unwrap()
    };

    let fresh = status();
    assert_success(&fresh);
    let text = stdout(&fresh);
    assert!(text.contains("database_driver: postgresql"));
    assert!(text.contains("database_readable: true"));
    assert!(text.contains(&format!("superuser: {superuser} (absent)")));

    let created = run_pg(
        dir.path(),
        &url,
        &[
            "user-create",
            &username,
            "--email",
            &format!("{username}@example.com"),
            "--password",
            "postgres-password",
            "--admin",
        ],
    );
    assert_success(&created);

    let listed = run_pg(dir.path(), &url, &["user-list"]);
    assert_success(&listed);
    assert!(stdout(&listed).contains(&username));

    let shown = run_pg(dir.path(), &url, &["user-show", &username]);
    assert_success(&shown);
    assert!(stdout(&shown).contains("admin: true"));

    let superuser_create = run_pg(
        dir.path(),
        &url,
        &["user-create", &superuser, "--password", "super-password"],
    );
    assert_success(&superuser_create);
    let ready = status();
    assert_success(&ready);
    assert!(stdout(&ready).contains(&format!("superuser: {superuser} (present)")));

    let deleted = run_pg(dir.path(), &url, &["user-delete", &username]);
    assert_success(&deleted);
    let missing = run_pg(dir.path(), &url, &["user-show", &username]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("not found"));
}

#[test]
fn postgres_organization_membership_and_server_admins() {
    let Some(url) = postgres_url() else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let creator = unique("pg-org-creator");
    let member = unique("pg-org-member");
    let org = unique("pg-org");

    assert_success(&run_pg(
        dir.path(),
        &url,
        &["user-create", &creator, "--password", "creator-password"],
    ));
    assert_success(&run_pg(
        dir.path(),
        &url,
        &["user-create", &member, "--password", "member-password"],
    ));
    assert_success(&run_pg(
        dir.path(),
        &url,
        &[
            "org-create",
            &org,
            "--full-name",
            "PostgreSQL Org",
            "--creator",
            &creator,
        ],
    ));

    let shown = run_pg(dir.path(), &url, &["org-show", &org]);
    assert_success(&shown);
    assert!(stdout(&shown).contains("full_name: PostgreSQL Org"));
    assert!(stdout(&shown).contains(&creator));

    let added = run_pg(dir.path(), &url, &["org-user-add", &org, &member]);
    assert_success(&added);
    let shown = run_pg(dir.path(), &url, &["org-show", &org]);
    assert_success(&shown);
    assert!(stdout(&shown).contains(&member));

    let removed = run_pg(dir.path(), &url, &["org-user-remove", &org, &member]);
    assert_success(&removed);
    let shown = run_pg(dir.path(), &url, &["org-show", &org]);
    assert_success(&shown);
    assert!(!stdout(&shown).contains(&member));

    let granted = run_pg(
        dir.path(),
        &url,
        &["grant-server-admin-permissions", &member],
    );
    assert_success(&granted);
    let admins = run_pg(dir.path(), &url, &["list-server-admins"]);
    assert_success(&admins);
    assert!(stdout(&admins).lines().any(|line| line.trim() == member));
    let revoked = run_pg(
        dir.path(),
        &url,
        &["remove-server-admin-permissions", &member],
    );
    assert_success(&revoked);

    let deleted = run_pg(dir.path(), &url, &["org-delete", &org]);
    assert_success(&deleted);
    let missing = run_pg(dir.path(), &url, &["org-show", &org]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("not found"));
}

#[test]
fn postgres_keys_secrets_maintenance_and_check_config() {
    use microwave_store::IdentityRepository;

    let Some(url) = postgres_url() else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let owner = unique("pg-key-owner");
    let org = unique("pg-key-org");
    let client = unique("pg-key-client");
    let secret_value = unique("pg-secret-value");

    assert_success(&run_pg(
        dir.path(),
        &url,
        &["user-create", &owner, "--password", "owner-password"],
    ));
    assert_success(&run_pg(
        dir.path(),
        &url,
        &["org-create", &org, "--creator", &owner],
    ));

    let runtime = pg_runtime();
    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect to configured PostgreSQL");
    let org_row = runtime
        .block_on(store.find_org(&org))
        .expect("find org")
        .expect("org exists");
    runtime
        .block_on(store.create_client(&org_row.id, &client, false, false))
        .expect("create client");

    let created = run_pg(
        dir.path(),
        &url,
        &["add-client-key", &org, &client, "--key-name", "deploy"],
    );
    assert_success(&created);
    assert!(stdout(&created).contains("BEGIN PRIVATE KEY"));
    let listed = run_pg(dir.path(), &url, &["list-client-keys", &org, &client]);
    assert_success(&listed);
    assert!(stdout(&listed).contains("name: deploy"));
    let deleted = run_pg(
        dir.path(),
        &url,
        &["delete-client-key", &org, &client, "deploy"],
    );
    assert_success(&deleted);

    let set = run_pg(
        dir.path(),
        &url,
        &["set-secret", "data_collector", "token", &secret_value],
    );
    assert_success(&set);
    let shown = run_pg(
        dir.path(),
        &url,
        &["show-secret", "data_collector", "token"],
    );
    assert_success(&shown);
    assert_eq!(stdout(&shown).trim(), secret_value);
    let removed = run_pg(
        dir.path(),
        &url,
        &["remove-secret", "data_collector", "token"],
    );
    assert_success(&removed);

    assert_success(&run_pg(dir.path(), &url, &["maintenance", "on"]));
    let status = run_pg(dir.path(), &url, &["maintenance", "status"]);
    assert_success(&status);
    assert!(stdout(&status).contains("maintenance: on"));
    assert_success(&run_pg(dir.path(), &url, &["maintenance", "off"]));
    let status = run_pg(dir.path(), &url, &["maintenance", "status"]);
    assert_success(&status);
    assert!(stdout(&status).contains("maintenance: off"));

    // check-config resolves object-store credentials from PostgreSQL when
    // they are absent from the config file.
    let access = unique("pg-access");
    let secret = unique("pg-secret");
    assert_success(&run_pg(
        dir.path(),
        &url,
        &["set-secret", "bookshelf", "access_key_id", &access],
    ));
    assert_success(&run_pg(
        dir.path(),
        &url,
        &["set-secret", "bookshelf", "secret_access_key", &secret],
    ));
    let config = dir.path().join("pg.toml");
    std::fs::write(
        &config,
        format!(
            "[database]\ndriver = \"postgres\"\nurl = \"{url}\"\n\n\
             [object_store]\nbackend = \"s3\"\nendpoint = \"http://minio:9000\"\n\
             bucket = \"chef-cookbooks\"\n"
        ),
    )
    .unwrap();
    let checked = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert_success(&checked);
    assert!(stdout(&checked).contains("ok: object_store s3"));
    assert_success(&run_pg(
        dir.path(),
        &url,
        &["remove-secret", "bookshelf", "access_key_id"],
    ));
    assert_success(&run_pg(
        dir.path(),
        &url,
        &["remove-secret", "bookshelf", "secret_access_key"],
    ));
}

#[test]
fn postgres_migrate_from_legacy_imports_a_sqlite_snapshot() {
    use microwave_store::IdentityRepository;

    let Some(base_url) = postgres_url() else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    let (scratch, url) = scratch_database(&base_url, "ctl-import-db");
    let source = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&source.path().join("microwave.db")).unwrap();
    let username = unique("pg-legacy");
    let user = store
        .create_user(
            &username,
            &format!("{username}@example.com"),
            "bcrypt",
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
    let imported = run_pg(
        target.path(),
        &url,
        &[
            "migrate-from-legacy",
            "--input",
            snapshot.to_str().unwrap(),
            "--yes",
        ],
    );
    assert_success(&imported);
    assert!(stdout(&imported).contains("configured PostgreSQL database"));

    let runtime = pg_runtime();
    let restored = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect to configured PostgreSQL");
    let restored_user = runtime
        .block_on(restored.find_user(&username))
        .expect("find imported user")
        .expect("imported user exists");
    assert_eq!(restored_user.id, user.id);
    assert_eq!(restored_user.password_hash, "legacy-hash");
    assert!(restored_user.admin);
    drop_scratch_database(&base_url, &scratch);
}

#[test]
fn postgres_psql_passes_queries_through() {
    let Some(url) = postgres_url() else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    if !pg_client_tool_available("psql") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let output = run_pg(
        dir.path(),
        &url,
        &["psql", "-tA", "-c", "select count(*) >= 0 from users;"],
    );
    assert_success(&output);
    assert_eq!(stdout(&output).trim(), "t");
}

#[test]
fn postgres_backup_and_restore_round_trip() {
    use microwave_store::IdentityRepository;

    let Some(base_url) = postgres_url() else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    if !pg_client_tool_available("pg_dump") || !pg_client_tool_available("pg_restore") {
        return;
    }

    let (scratch, url) = scratch_database(&base_url, "ctl-backup-db");
    let runtime = pg_runtime();
    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect to scratch PostgreSQL");
    let persisted = unique("pg-backup-persisted");
    runtime
        .block_on(store.create_user(
            &persisted,
            &format!("{persisted}@example.com"),
            "bcrypt",
            "hash",
            "salt",
            false,
        ))
        .expect("create persisted user");
    drop(store);

    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let path = backups.path().join("pg-backup");
    let output = run_pg(data.path(), &url, &["backup", path.to_str().unwrap()]);
    assert_success(&output);
    assert!(path.join("microwave.dump").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path.join("microwave.dump"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("reconnect to scratch PostgreSQL");
    let transient = unique("pg-backup-transient");
    runtime
        .block_on(store.create_user(
            &transient,
            &format!("{transient}@example.com"),
            "bcrypt",
            "hash",
            "salt",
            false,
        ))
        .expect("create transient user");
    drop(store);

    let refused = run_pg(data.path(), &url, &["restore", path.to_str().unwrap()]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));
    let text = stdout(&refused);
    assert!(text.contains("Restore target:"));
    assert!(!text.contains("microwave:microwave"));

    // A refused confirmation must not have dropped the live objects.
    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect after refused restore");
    assert!(runtime
        .block_on(store.find_user(&transient))
        .expect("find transient user after refused restore")
        .is_some());
    drop(store);

    let restored = run_pg(
        data.path(),
        &url,
        &["restore", path.to_str().unwrap(), "--yes"],
    );
    assert_success(&restored);
    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect after restore");
    assert!(runtime
        .block_on(store.find_user(&persisted))
        .expect("find restored user")
        .is_some());
    assert!(runtime
        .block_on(store.find_user(&transient))
        .expect("find transient user")
        .is_none());
    drop(store);

    drop_scratch_database(&base_url, &scratch);
}

#[test]
fn postgres_reconfigure_initializes_and_reports_the_running_backend() {
    let Some(base_url) = postgres_url() else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    let (scratch, url) = scratch_database(&base_url, "ctl-reconfigure-db");
    let dir = tempfile::tempdir().unwrap();

    let output = run_pg(dir.path(), &url, &["reconfigure"]);
    assert_success(&output);
    let text = stdout(&output);
    assert!(text.contains("database_driver: postgresql"));
    assert!(text.contains("database: PostgreSQL"));
    assert!(text.contains("not restarted"));
    assert!(!text.contains("microwave:microwave"));

    // A second run is idempotent and still reports the same backend.
    let output = run_pg(dir.path(), &url, &["reconfigure"]);
    assert_success(&output);
    assert!(stdout(&output).contains("database_driver: postgresql"));

    drop_scratch_database(&base_url, &scratch);
}

#[test]
fn postgres_tar_backup_and_restore_round_trip() {
    use microwave_store::IdentityRepository;

    let Some(base_url) = postgres_url() else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    if !pg_client_tool_available("pg_dump") || !pg_client_tool_available("pg_restore") {
        return;
    }

    let (scratch, url) = scratch_database(&base_url, "ctl-tar-db");
    let runtime = pg_runtime();
    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect to scratch PostgreSQL");
    let persisted = unique("pg-tar-persisted");
    runtime
        .block_on(store.create_user(
            &persisted,
            &format!("{persisted}@example.com"),
            "bcrypt",
            "hash",
            "salt",
            false,
        ))
        .expect("create persisted user");
    drop(store);

    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let archive = backups.path().join("pg-backup.tar");
    let output = run_pg(
        data.path(),
        &url,
        &["backup", archive.to_str().unwrap(), "--tar"],
    );
    assert_success(&output);
    assert!(archive.is_file());

    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("reconnect to scratch PostgreSQL");
    let transient = unique("pg-tar-transient");
    runtime
        .block_on(store.create_user(
            &transient,
            &format!("{transient}@example.com"),
            "bcrypt",
            "hash",
            "salt",
            false,
        ))
        .expect("create transient user");
    drop(store);

    let refused = run_pg(data.path(), &url, &["restore", archive.to_str().unwrap()]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"));

    let restored = run_pg(
        data.path(),
        &url,
        &["restore", archive.to_str().unwrap(), "--yes"],
    );
    assert_success(&restored);

    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect after tar restore");
    assert!(runtime
        .block_on(store.find_user(&persisted))
        .expect("find restored user")
        .is_some());
    assert!(runtime
        .block_on(store.find_user(&transient))
        .expect("find transient user")
        .is_none());
    drop(store);

    drop_scratch_database(&base_url, &scratch);
}

fn write_tls_pair(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    let cert_path = dir.join("tls.crt");
    let key_path = dir.join("tls.key");
    std::fs::write(&cert_path, certified.cert.pem()).unwrap();
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).unwrap();
    (cert_path, key_path)
}

#[test]
fn check_config_accepts_a_tls_pair_without_printing_key_material() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = write_tls_pair(dir.path());
    let secret = std::fs::read_to_string(&key)
        .unwrap()
        .lines()
        .find(|line| !line.starts_with("-----"))
        .unwrap()
        .to_string();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        format!(
            "[server.tls]\ncert_path = {:?}\nkey_path = {:?}\n",
            cert, key
        ),
    )
    .unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();

    assert_success(&output);
    let text = stdout(&output);
    assert!(text.contains("ok: tls "), "stdout: {text}");
    assert!(!text.contains(&secret));
    assert!(!text.contains("PRIVATE KEY"));
}

#[test]
fn check_config_rejects_a_missing_tls_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("tls.key");
    std::fs::write(&key, "not used").unwrap();
    let config = dir.path().join("microwave.toml");
    std::fs::write(
        &config,
        format!(
            "[server.tls]\ncert_path = {:?}\nkey_path = {:?}\n",
            dir.path().join("missing.crt"),
            key
        ),
    )
    .unwrap();

    let output = ctl(dir.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(
        text.contains("failed to read TLS certificate file"),
        "stderr: {text}"
    );
}

#[test]
fn check_config_rejects_a_mismatched_tls_pair() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (cert, _first_key) = write_tls_pair(first.path());
    let (_second_cert, key) = write_tls_pair(second.path());
    let config = first.path().join("microwave.toml");
    std::fs::write(
        &config,
        format!(
            "[server.tls]\ncert_path = {:?}\nkey_path = {:?}\n",
            cert, key
        ),
    )
    .unwrap();

    let output = ctl(first.path())
        .args(["--config", config.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("matching pair"));
}
