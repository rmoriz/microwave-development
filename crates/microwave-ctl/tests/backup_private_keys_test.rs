//! Integration coverage for opt-in private key/certificate backup and restore.
//!
//! Commands run in a child invocation of this same test binary so stdout and
//! stderr can be inspected. The child helper is a normal test that only does
//! work when the parent sets its environment variables.

use microwave_ctl::{dispatch, Cli, Command as CtlCommand};
use microwave_store::SqliteStore;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DB_SECRET: &str = "database-secret-value-9f3a";
const PIVOTAL_CONTENT: &str = "PIVOTAL-PRIVATE-CONTENT-8c2b";
const WEBUI_PUBLIC_CONTENT: &str = "WEBUI-PUBLIC-CONTENT-1a4d";
const TLS_CERT_CONTENT: &str = "TLS-CERT-PUBLIC-CONTENT-77e1";
const TLS_KEY_CONTENT: &str = "TLS-KEY-PRIVATE-CONTENT-c93f";

/// Runs the requested command when spawned by [`run_ctl`], otherwise no-ops.
#[test]
fn backup_child_dispatch() {
    let Ok(config) = std::env::var("MW_BACKUP_TEST_CONFIG") else {
        return;
    };
    let action = std::env::var("MW_BACKUP_TEST_ACTION").expect("child action");
    let path = PathBuf::from(std::env::var("MW_BACKUP_TEST_PATH").expect("child path"));
    let tar = std::env::var("MW_BACKUP_TEST_TAR").as_deref() == Ok("1");
    let private = std::env::var("MW_BACKUP_TEST_PRIVATE").as_deref() == Ok("1");
    let private_only = std::env::var("MW_BACKUP_TEST_PRIVATE_ONLY").as_deref() == Ok("1");

    let command = match action.as_str() {
        "backup" => CtlCommand::Backup {
            path,
            format: None,
            tar,
            include_private_keys: private,
        },
        "restore" => CtlCommand::Restore {
            path,
            yes: true,
            format: None,
            tar,
            restore_private_keys: private,
            private_only,
        },
        other => panic!("unknown test action {other}"),
    };

    if let Err(error) = dispatch(Cli {
        config: Some(PathBuf::from(config)),
        command,
    }) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run_ctl(config: &Path, action: &str, path: &Path, tar: bool, private: bool) -> Output {
    run_ctl_with(config, action, path, tar, private, false)
}

fn run_private_only(config: &Path, path: &Path, tar: bool) -> Output {
    run_ctl_with(config, "restore", path, tar, true, true)
}

fn run_ctl_with(
    config: &Path,
    action: &str,
    path: &Path,
    tar: bool,
    private: bool,
    private_only: bool,
) -> Output {
    run_ctl_with_failpoint(config, action, path, tar, private, private_only, None)
}

fn run_ctl_with_failpoint(
    config: &Path,
    action: &str,
    path: &Path,
    tar: bool,
    private: bool,
    private_only: bool,
    fail_after: Option<usize>,
) -> Output {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "backup_child_dispatch", "--nocapture"])
        .env_remove("MW_CONFIG")
        .env_remove("MW_DATABASE__DRIVER")
        .env_remove("MW_DATABASE__URL")
        .env_remove("MW_PATHS__DATA_DIR")
        .env_remove("MW_CTL_TEST_PRIVATE_APPLY_FAIL_AFTER")
        .env("MW_BACKUP_TEST_CONFIG", config)
        .env("MW_BACKUP_TEST_ACTION", action)
        .env("MW_BACKUP_TEST_PATH", path)
        .env("MW_BACKUP_TEST_TAR", if tar { "1" } else { "0" })
        .env("MW_BACKUP_TEST_PRIVATE", if private { "1" } else { "0" })
        .env(
            "MW_BACKUP_TEST_PRIVATE_ONLY",
            if private_only { "1" } else { "0" },
        );
    if let Some(fail_after) = fail_after {
        command.env(
            "MW_CTL_TEST_PRIVATE_APPLY_FAIL_AFTER",
            fail_after.to_string(),
        );
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn combined(output: &Output) -> String {
    format!("{}\n{}", stdout(output), stderr(output))
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
}

fn assert_failure(output: &Output) {
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
}

fn assert_no_secret_contents(output: &Output) {
    let text = combined(output);
    for secret in [
        DB_SECRET,
        PIVOTAL_CONTENT,
        WEBUI_PUBLIC_CONTENT,
        TLS_CERT_CONTENT,
        TLS_KEY_CONTENT,
    ] {
        assert!(
            !text.contains(secret),
            "command output leaked private material {secret:?}: {text}"
        );
    }
}

/// A temporary installation with a resolved SQLite path and optional TLS
/// certificate/key paths.
struct Installation {
    _dir: tempfile::TempDir,
    root: PathBuf,
    data_dir: PathBuf,
    config: PathBuf,
    database: PathBuf,
    tls_cert: PathBuf,
    tls_key: PathBuf,
}

impl Installation {
    fn new(tls: bool) -> Self {
        Self::new_with_data_dir(tls, false)
    }

    #[cfg(unix)]
    fn with_symlinked_data_dir(tls: bool) -> Self {
        Self::new_with_data_dir(tls, true)
    }

    fn new_with_data_dir(tls: bool, symlink_data_dir: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let real_data_dir = root.join("data-real");
        std::fs::create_dir_all(&real_data_dir).unwrap();
        let data_dir = root.join("data");
        if symlink_data_dir {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&real_data_dir, &data_dir).unwrap();
            #[cfg(not(unix))]
            std::fs::create_dir_all(&data_dir).unwrap();
        } else {
            std::fs::create_dir_all(&data_dir).unwrap();
        }
        let database = data_dir.join("microwave.db");
        let config = root.join("microwave.toml");
        let tls_cert = root.join("tls/cert.pem");
        let tls_key = root.join("tls/key.pem");

        let mut text = format!("[paths]\ndata_dir = \"{}\"\n", data_dir.display());
        if tls {
            text.push_str(&format!(
                "[server.tls]\ncert_path = \"{}\"\nkey_path = \"{}\"\n",
                tls_cert.display(),
                tls_key.display()
            ));
        }
        std::fs::write(&config, text).unwrap();

        Self {
            _dir: dir,
            root,
            data_dir,
            config,
            database,
            tls_cert,
            tls_key,
        }
    }

    #[cfg(unix)]
    fn use_letsencrypt_tls_layout(&mut self) {
        let archive = self.root.join("tls/archive");
        let live = self.root.join("tls/live");
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        let cert_archive = archive.join("cert-1.pem");
        let key_archive = archive.join("key-1.pem");
        write_file(&cert_archive, TLS_CERT_CONTENT, 0o600);
        write_file(&key_archive, TLS_KEY_CONTENT, 0o644);
        std::os::unix::fs::symlink(Path::new("../archive/cert-1.pem"), live.join("cert.pem"))
            .unwrap();
        std::os::unix::fs::symlink(Path::new("../archive/key-1.pem"), live.join("key.pem"))
            .unwrap();
        self.tls_cert = live.join("cert.pem");
        self.tls_key = live.join("key.pem");
        let text = format!(
            "[paths]\ndata_dir = \"{}\"\n[server.tls]\ncert_path = \"{}\"\nkey_path = \"{}\"\n",
            self.data_dir.display(),
            self.tls_cert.display(),
            self.tls_key.display()
        );
        std::fs::write(&self.config, text).unwrap();
    }

    fn write_bookshelf_sentinel(&self) {
        write_file(
            &self.data_dir.join("bookshelf/org/sentinel"),
            "BOOKSHELF-SENTINEL",
            0o600,
        );
    }

    fn seed_live_marker(&self) -> Vec<u8> {
        self.seed_database(DB_SECRET);
        self.write_bookshelf_sentinel();
        std::fs::read(&self.database).unwrap()
    }

    fn assert_live_marker_untouched(&self, before: &[u8]) {
        assert_eq!(
            self.database_secret().as_deref(),
            Some(DB_SECRET),
            "live database marker changed"
        );
        assert_eq!(
            std::fs::read_to_string(self.data_dir.join("bookshelf/org/sentinel")).unwrap(),
            "BOOKSHELF-SENTINEL",
            "live bookshelf sentinel changed"
        );
        assert_eq!(
            std::fs::read(&self.database).unwrap(),
            before,
            "live database bytes changed"
        );
    }

    fn seed_database(&self, value: &str) {
        let store = SqliteStore::open(&self.database).unwrap();
        store.set_secret("shared", "secret", value).unwrap();
    }

    fn database_secret(&self) -> Option<String> {
        SqliteStore::open(&self.database)
            .unwrap()
            .get_secret("shared", "secret")
            .unwrap()
    }

    fn write_data_file(&self, name: &str, contents: &str, mode: u32) {
        write_file(&self.data_dir.join(name), contents, mode);
    }

    fn write_secrets(&self) {
        self.write_data_file("pivotal.pem", PIVOTAL_CONTENT, 0o644);
        self.write_data_file("webui_pub.pem", WEBUI_PUBLIC_CONTENT, 0o600);
        write_file(&self.tls_cert, TLS_CERT_CONTENT, 0o600);
        write_file(&self.tls_key, TLS_KEY_CONTENT, 0o644);
    }

    fn replace_live_secrets(&self) {
        self.write_data_file("pivotal.pem", "LIVE-PIVOTAL", 0o644);
        self.write_data_file("webui_pub.pem", "LIVE-PUBLIC", 0o600);
        write_file(&self.tls_cert, "LIVE-CERT", 0o600);
        write_file(&self.tls_key, "LIVE-KEY", 0o644);
    }
}

fn write_file(path: &Path, contents: &str, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    set_mode(path, mode);
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

fn mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0o600
    }
}

fn read_manifest(backup: &Path) -> serde_json::Value {
    let bytes = std::fs::read(backup.join("private/manifest.json")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn manifest_entries(manifest: &serde_json::Value) -> &[serde_json::Value] {
    manifest["entries"].as_array().unwrap()
}

#[test]
fn directory_backup_includes_private_files_only_on_request() {
    let installation = Installation::new(true);
    installation.seed_database(DB_SECRET);
    installation.write_secrets();

    let default_backup = installation.root.join("default-backup");
    let output = run_ctl(
        &installation.config,
        "backup",
        &default_backup,
        false,
        false,
    );
    assert_success(&output);
    assert_no_secret_contents(&output);
    assert!(!default_backup.join("private").exists());

    let private_backup = installation.root.join("private-backup");
    let output = run_ctl(&installation.config, "backup", &private_backup, false, true);
    assert_success(&output);
    assert_no_secret_contents(&output);
    let text = stdout(&output);
    assert!(
        text.contains("included 4 private payload file(s)") && text.contains("2 private, 2 public"),
        "stdout: {text}"
    );

    let manifest = read_manifest(&private_backup);
    assert_eq!(manifest["format"], "microwave-ctl-private-files");
    assert_eq!(manifest["version"], 1);
    let entries = manifest_entries(&manifest);
    assert_eq!(entries.len(), 4);
    for entry in entries {
        let target = entry["target"].as_str().unwrap();
        let expected_mode = match target {
            "webui_pub.pem" | "tls-cert" => 0o644,
            _ => 0o600,
        };
        assert_eq!(entry["mode"], expected_mode, "target {target}");
        assert_eq!(
            mode(&private_backup.join(entry["path"].as_str().unwrap())),
            expected_mode
        );
    }
}

#[test]
fn directory_restore_skips_private_files_without_the_flag() {
    let installation = Installation::new(true);
    installation.seed_database(DB_SECRET);
    installation.write_secrets();

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);

    installation.seed_database("LIVE-DATABASE");
    installation.replace_live_secrets();

    let output = run_ctl(&installation.config, "restore", &backup, false, false);
    assert_success(&output);
    assert_no_secret_contents(&output);
    let warning = stderr(&output);
    assert!(
        warning.contains("--restore-private-keys was not given")
            && warning.contains("Existing target key files are untouched"),
        "stderr: {warning}"
    );

    assert_eq!(installation.database_secret().as_deref(), Some(DB_SECRET));
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("pivotal.pem")).unwrap(),
        "LIVE-PIVOTAL"
    );
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("webui_pub.pem")).unwrap(),
        "LIVE-PUBLIC"
    );
    assert_eq!(
        std::fs::read_to_string(&installation.tls_cert).unwrap(),
        "LIVE-CERT"
    );
    assert_eq!(
        std::fs::read_to_string(&installation.tls_key).unwrap(),
        "LIVE-KEY"
    );
}

#[test]
fn directory_restore_private_files_round_trip_with_modes() {
    let installation = Installation::new(true);
    installation.seed_database(DB_SECRET);
    installation.write_secrets();

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);

    installation.seed_database("LIVE-DATABASE");
    installation.replace_live_secrets();

    let output = run_ctl(&installation.config, "restore", &backup, false, true);
    assert_success(&output);
    assert_no_secret_contents(&output);
    let text = combined(&output);
    assert!(
        text.contains("can invalidate existing clients"),
        "output: {text}"
    );
    assert!(
        text.contains("restored 4 private payload file(s)"),
        "output: {text}"
    );

    assert_eq!(installation.database_secret().as_deref(), Some(DB_SECRET));
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("pivotal.pem")).unwrap(),
        PIVOTAL_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("webui_pub.pem")).unwrap(),
        WEBUI_PUBLIC_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(&installation.tls_cert).unwrap(),
        TLS_CERT_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(&installation.tls_key).unwrap(),
        TLS_KEY_CONTENT
    );

    assert_eq!(mode(&installation.data_dir.join("pivotal.pem")), 0o600);
    assert_eq!(mode(&installation.data_dir.join("webui_pub.pem")), 0o644);
    assert_eq!(mode(&installation.tls_cert), 0o644);
    assert_eq!(mode(&installation.tls_key), 0o600);
}

#[test]
fn tar_backup_and_private_restore_round_trip() {
    let installation = Installation::new(true);
    installation.seed_database(DB_SECRET);
    installation.write_secrets();

    let default_archive = installation.root.join("default.tar");
    let output = run_ctl(
        &installation.config,
        "backup",
        &default_archive,
        true,
        false,
    );
    assert_success(&output);
    let entries = tar_entries(&default_archive);
    assert!(
        entries.iter().all(|(path, _)| !path.starts_with("private")),
        "default tar unexpectedly contains private entries: {entries:?}"
    );

    let archive = installation.root.join("backup.tar");
    let output = run_ctl(&installation.config, "backup", &archive, true, true);
    assert_success(&output);
    assert_no_secret_contents(&output);
    let entries = tar_entries(&archive);
    assert!(entries
        .iter()
        .any(|(path, _)| path == "private/manifest.json"));
    assert!(entries
        .iter()
        .any(|(path, _)| path == "private/files/pivotal.pem"));
    assert!(entries
        .iter()
        .any(|(path, _)| path == "private/files/tls-key.pem"));

    installation.seed_database("LIVE-DATABASE");
    installation.replace_live_secrets();

    let output = run_ctl(&installation.config, "restore", &archive, true, true);
    assert_success(&output);
    assert_no_secret_contents(&output);

    assert_eq!(installation.database_secret().as_deref(), Some(DB_SECRET));
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("pivotal.pem")).unwrap(),
        PIVOTAL_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(&installation.tls_cert).unwrap(),
        TLS_CERT_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(&installation.tls_key).unwrap(),
        TLS_KEY_CONTENT
    );
    assert_eq!(mode(&installation.data_dir.join("pivotal.pem")), 0o600);
    assert_eq!(mode(&installation.tls_cert), 0o644);
    assert_eq!(mode(&installation.tls_key), 0o600);
}

#[cfg(unix)]
#[test]
fn symlinked_data_dir_candidates_are_rejected() {
    let installation = Installation::new(false);
    installation.seed_database(DB_SECRET);
    let outside = installation.root.join("outside.pem");
    write_file(&outside, PIVOTAL_CONTENT, 0o600);
    std::os::unix::fs::symlink(&outside, installation.data_dir.join("pivotal.pem")).unwrap();

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("symlinked private file"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(!backup.join("private").exists());
    assert_no_secret_contents(&output);
}

#[cfg(unix)]
#[test]
fn symlinked_private_payload_is_rejected_before_metadata_restore() {
    let installation = Installation::new(false);
    let before = installation.seed_live_marker();
    installation.write_data_file("pivotal.pem", PIVOTAL_CONTENT, 0o600);

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);

    let outside = installation.root.join("outside.pem");
    write_file(&outside, "OUTSIDE-CONTENT", 0o600);
    std::fs::remove_file(backup.join("private/files/pivotal.pem")).unwrap();
    std::os::unix::fs::symlink(&outside, backup.join("private/files/pivotal.pem")).unwrap();

    let output = run_ctl(&installation.config, "restore", &backup, false, true);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("symlinked private file"),
        "stderr: {}",
        stderr(&output)
    );
    installation.assert_live_marker_untouched(&before);
}

#[test]
fn restore_requires_a_private_manifest_when_the_flag_is_given() {
    let installation = Installation::new(false);
    let before = installation.seed_live_marker();

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, false);
    assert_success(&output);

    let output = run_ctl(&installation.config, "restore", &backup, false, true);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("does not contain private/manifest.json"),
        "stderr: {}",
        stderr(&output)
    );
    installation.assert_live_marker_untouched(&before);
}

#[test]
fn malformed_private_manifests_fail_before_touching_metadata() {
    let installation = Installation::new(true);
    let before = installation.seed_live_marker();
    installation.write_secrets();

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);
    let manifest_path = backup.join("private/manifest.json");
    let valid = std::fs::read(&manifest_path).unwrap();

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("invalid JSON", b"{".to_vec()),
        ("missing mode", {
            let mut value: serde_json::Value = serde_json::from_slice(&valid).unwrap();
            value["entries"][0].as_object_mut().unwrap().remove("mode");
            serde_json::to_vec(&value).unwrap()
        }),
        ("traversal target", {
            let mut value: serde_json::Value = serde_json::from_slice(&valid).unwrap();
            value["entries"][0]["target"] = "../escape".into();
            serde_json::to_vec(&value).unwrap()
        }),
        ("wrong mode", {
            let mut value: serde_json::Value = serde_json::from_slice(&valid).unwrap();
            value["entries"][0]["mode"] = 0o644.into();
            serde_json::to_vec(&value).unwrap()
        }),
        ("unsafe path", {
            let mut value: serde_json::Value = serde_json::from_slice(&valid).unwrap();
            value["entries"][0]["path"] = "../escape".into();
            serde_json::to_vec(&value).unwrap()
        }),
    ];

    for (name, contents) in cases {
        std::fs::write(&manifest_path, contents).unwrap();
        let output = run_ctl(&installation.config, "restore", &backup, false, true);
        assert_failure(&output);
        assert!(
            stderr(&output).contains("private") || stderr(&output).contains("manifest"),
            "case {name}: stderr: {}",
            stderr(&output)
        );
        installation.assert_live_marker_untouched(&before);
    }

    // A missing referenced payload is also a preflight failure, and the error
    // names the missing payload path.
    std::fs::write(&manifest_path, &valid).unwrap();
    std::fs::remove_file(backup.join("private/files/pivotal.pem")).unwrap();
    let output = run_ctl(&installation.config, "restore", &backup, false, true);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("private/files/pivotal.pem"),
        "stderr: {}",
        stderr(&output)
    );
    installation.assert_live_marker_untouched(&before);

    // And an unreferenced payload is rejected rather than silently ignored.
    let mut value: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    value["entries"] = serde_json::json!([]);
    std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
    write_file(&backup.join("private/files/unreferenced.pem"), "x", 0o600);
    let output = run_ctl(&installation.config, "restore", &backup, false, true);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("unreferenced private file"),
        "stderr: {}",
        stderr(&output)
    );
    installation.assert_live_marker_untouched(&before);
}

#[test]
fn tar_restore_rejects_hostile_entries_before_touching_metadata() {
    use tar::{Builder, EntryType, Header};

    let installation = Installation::new(false);
    let before = installation.seed_live_marker();

    let database_bytes = before.clone();
    let cases: Vec<(&str, Vec<u8>, EntryType, Option<&str>)> = vec![
        ("unknown", b"evil.txt".to_vec(), EntryType::Regular, None),
        ("traversal", b"../escape".to_vec(), EntryType::Regular, None),
        (
            "absolute",
            b"/tmp/microwave-backup-escape".to_vec(),
            EntryType::Regular,
            None,
        ),
        (
            "symlink",
            b"private/files/evil.pem".to_vec(),
            EntryType::Symlink,
            Some("/etc/passwd"),
        ),
        (
            "hardlink",
            b"bookshelf/evil".to_vec(),
            EntryType::Link,
            Some("microwave.db"),
        ),
        ("fifo", b"bookshelf/fifo".to_vec(), EntryType::Fifo, None),
    ];

    for (name, raw_name, entry_type, link) in cases {
        let archive_path = installation.root.join(format!("hostile-{name}.tar"));
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut builder = Builder::new(file);

        let mut database_header = Header::new_gnu();
        database_header.set_size(database_bytes.len() as u64);
        database_header.set_mode(0o600);
        database_header.set_mtime(0);
        database_header.set_uid(0);
        database_header.set_gid(0);
        database_header.set_entry_type(EntryType::Regular);
        database_header.set_cksum();
        builder
            .append_data(
                &mut database_header,
                "microwave.db",
                database_bytes.as_slice(),
            )
            .unwrap();

        let mut header = Header::new_gnu();
        {
            let old = header.as_old_mut();
            old.name[..raw_name.len()].copy_from_slice(&raw_name);
        }
        header.set_size(0);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_entry_type(entry_type);
        if let Some(link) = link {
            header.set_link_name(link).unwrap();
        }
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();
        builder.finish().unwrap();

        let output = run_ctl(&installation.config, "restore", &archive_path, true, false);
        assert_failure(&output);
        let message = stderr(&output);
        assert!(
            message.contains("unexpected entry")
                || message.contains("not a regular file or directory"),
            "case {name}: stderr: {message}"
        );
        installation.assert_live_marker_untouched(&before);
    }
}

#[cfg(unix)]
#[test]
fn symlinked_data_dir_private_round_trip() {
    let installation = Installation::with_symlinked_data_dir(true);
    let before = installation.seed_live_marker();
    installation.write_secrets();
    assert!(std::fs::symlink_metadata(&installation.data_dir)
        .unwrap()
        .file_type()
        .is_symlink());

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);
    assert_no_secret_contents(&output);

    installation.seed_database("LIVE-DATABASE");
    installation.replace_live_secrets();

    let output = run_ctl(&installation.config, "restore", &backup, false, true);
    assert_success(&output);
    assert_no_secret_contents(&output);
    assert_eq!(installation.database_secret().as_deref(), Some(DB_SECRET));
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("pivotal.pem")).unwrap(),
        PIVOTAL_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(&installation.tls_key).unwrap(),
        TLS_KEY_CONTENT
    );
    assert!(
        std::fs::symlink_metadata(&installation.data_dir)
            .unwrap()
            .file_type()
            .is_symlink(),
        "restore must preserve the configured data_dir symlink"
    );
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("bookshelf/org/sentinel")).unwrap(),
        "BOOKSHELF-SENTINEL"
    );
    let _ = before;
}

#[cfg(unix)]
#[test]
fn letsencrypt_tls_symlink_targets_are_replaced_at_their_canonical_files() {
    let mut installation = Installation::new(true);
    installation.use_letsencrypt_tls_layout();
    installation.seed_live_marker();

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);
    let manifest = read_manifest(&backup);
    let targets = manifest_entries(&manifest)
        .iter()
        .filter_map(|entry| entry["target"].as_str())
        .collect::<Vec<_>>();
    assert!(targets.contains(&"tls-cert"));
    assert!(targets.contains(&"tls-key"));

    let archive_cert = installation.root.join("tls/archive/cert-1.pem");
    let archive_key = installation.root.join("tls/archive/key-1.pem");
    write_file(&archive_cert, "LIVE-CERT", 0o600);
    write_file(&archive_key, "LIVE-KEY", 0o644);
    installation.seed_database("LIVE-DATABASE");

    let output = run_ctl(&installation.config, "restore", &backup, false, true);
    assert_success(&output);
    assert_no_secret_contents(&output);

    assert_eq!(
        std::fs::read_to_string(&archive_cert).unwrap(),
        TLS_CERT_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(&archive_key).unwrap(),
        TLS_KEY_CONTENT
    );
    assert_eq!(mode(&archive_cert), 0o644);
    assert_eq!(mode(&archive_key), 0o600);
    for link in [&installation.tls_cert, &installation.tls_key] {
        assert!(
            std::fs::symlink_metadata(link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "restore must preserve the configured TLS symlink {link:?}"
        );
    }
    assert_eq!(installation.database_secret().as_deref(), Some(DB_SECRET));
}

#[test]
fn private_only_restores_keys_without_replaying_metadata() {
    let installation = Installation::new(true);
    installation.seed_database(DB_SECRET);
    installation.write_secrets();
    installation.write_bookshelf_sentinel();

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);

    installation.seed_database("LIVE-DATABASE");
    write_file(
        &installation.data_dir.join("bookshelf/org/sentinel"),
        "LIVE-SENTINEL",
        0o600,
    );
    installation.replace_live_secrets();

    let output = run_private_only(&installation.config, &backup, false);
    assert_success(&output);
    assert_no_secret_contents(&output);
    let text = combined(&output);
    assert!(
        text.contains("The database and bookshelf will NOT be restored"),
        "output: {text}"
    );

    assert_eq!(
        installation.database_secret().as_deref(),
        Some("LIVE-DATABASE")
    );
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("bookshelf/org/sentinel")).unwrap(),
        "LIVE-SENTINEL"
    );
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("pivotal.pem")).unwrap(),
        PIVOTAL_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(&installation.tls_key).unwrap(),
        TLS_KEY_CONTENT
    );
}

#[test]
fn private_only_tar_restore_does_not_replay_metadata() {
    let installation = Installation::new(true);
    installation.seed_database(DB_SECRET);
    installation.write_secrets();

    let archive = installation.root.join("backup.tar");
    let output = run_ctl(&installation.config, "backup", &archive, true, true);
    assert_success(&output);

    installation.seed_database("LIVE-DATABASE");
    installation.replace_live_secrets();

    let output = run_private_only(&installation.config, &archive, true);
    assert_success(&output);
    assert_eq!(
        installation.database_secret().as_deref(),
        Some("LIVE-DATABASE")
    );
    assert_eq!(
        std::fs::read_to_string(installation.data_dir.join("pivotal.pem")).unwrap(),
        PIVOTAL_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(&installation.tls_key).unwrap(),
        TLS_KEY_CONTENT
    );
}

#[test]
fn private_only_requires_restore_private_keys() {
    let installation = Installation::new(false);
    installation.seed_database(DB_SECRET);
    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);

    let output = run_ctl_with(&installation.config, "restore", &backup, false, false, true);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("--private-only requires --restore-private-keys"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn default_backup_only_replaces_a_recognized_private_tree() {
    let installation = Installation::new(false);
    installation.seed_database(DB_SECRET);

    let recognized = installation.root.join("recognized");
    write_file(
        &recognized.join("private/manifest.json"),
        r#"{"format":"microwave-ctl-private-files","version":1,"entries":[]}"#,
        0o600,
    );
    std::fs::create_dir_all(recognized.join("private/files")).unwrap();
    let output = run_ctl(&installation.config, "backup", &recognized, false, false);
    assert_success(&output);
    assert!(!recognized.join("private").exists());
    assert!(recognized.join("microwave.db").is_file());

    let unrelated = installation.root.join("unrelated");
    write_file(&unrelated.join("private/keep-me.txt"), "USER-DATA", 0o600);
    let output = run_ctl(&installation.config, "backup", &unrelated, false, false);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("not a recognizable microwave-ctl private backup tree"),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(
        std::fs::read_to_string(unrelated.join("private/keep-me.txt")).unwrap(),
        "USER-DATA"
    );
    assert!(!unrelated.join("microwave.db").exists());

    let extra = installation.root.join("recognized-with-extra");
    write_file(
        &extra.join("private/manifest.json"),
        r#"{"format":"microwave-ctl-private-files","version":1,"entries":[]}"#,
        0o600,
    );
    std::fs::create_dir_all(extra.join("private/files")).unwrap();
    write_file(&extra.join("private/extra.txt"), "EXTRA", 0o600);
    let output = run_ctl(&installation.config, "backup", &extra, false, false);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("not a recognizable microwave-ctl private backup tree"),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(
        std::fs::read_to_string(extra.join("private/extra.txt")).unwrap(),
        "EXTRA"
    );
    assert!(!extra.join("microwave.db").exists());
}

#[test]
fn every_allowlisted_private_filename_is_backed_up_with_its_mode() {
    const ALLOWLIST: [(&str, bool); 13] = [
        ("pivotal.pem", true),
        ("pivotal.password", true),
        ("objectstore.secret", true),
        ("private-chef-secrets.json", true),
        ("private_chef_secrets.json", true),
        ("webui_priv.pem", true),
        ("webui_pub.pem", false),
        ("oc_id_priv.pem", true),
        ("oc_id_pub.pem", false),
        ("ocid_priv.pem", true),
        ("ocid_pub.pem", false),
        ("oc-id.pem", true),
        ("oc_id.pem", true),
    ];

    let installation = Installation::new(false);
    installation.seed_database(DB_SECRET);
    for (name, _) in ALLOWLIST {
        installation.write_data_file(name, &format!("CONTENT-{name}"), 0o644);
    }

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);
    assert_no_secret_contents(&output);

    let manifest = read_manifest(&backup);
    let entries = manifest_entries(&manifest);
    assert_eq!(entries.len(), ALLOWLIST.len());
    for (name, private) in ALLOWLIST {
        let entry = entries
            .iter()
            .find(|entry| entry["target"] == name)
            .unwrap_or_else(|| panic!("manifest is missing {name}"));
        let expected_mode = if private { 0o600 } else { 0o644 };
        assert_eq!(entry["mode"], expected_mode, "{name}");
        assert_eq!(
            mode(&backup.join(entry["path"].as_str().unwrap())),
            expected_mode,
            "{name}"
        );
    }
}

#[test]
fn an_empty_private_set_does_not_claim_key_material_was_included() {
    let installation = Installation::new(false);
    installation.seed_database(DB_SECRET);

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);
    let text = stdout(&output);
    assert!(
        text.contains("no allowlisted private key/certificate payload files were present"),
        "stdout: {text}"
    );
    assert!(
        !text.contains("contains unencrypted key material"),
        "stdout: {text}"
    );
    let manifest = read_manifest(&backup);
    assert!(manifest_entries(&manifest).is_empty());

    let output = run_private_only(&installation.config, &backup, false);
    assert_success(&output);
    assert!(
        stdout(&output).contains("nothing to restore"),
        "stdout: {}",
        stdout(&output)
    );
}

#[test]
fn tar_restore_rejects_oversized_private_entries_before_extraction() {
    use tar::{Builder, EntryType, Header};

    let installation = Installation::new(false);
    let before = installation.seed_live_marker();
    let database_bytes = before.clone();

    let archive_path = installation.root.join("oversized-private.tar");
    let file = std::fs::File::create(&archive_path).unwrap();
    let mut builder = Builder::new(file);

    let mut database_header = Header::new_gnu();
    database_header.set_size(database_bytes.len() as u64);
    database_header.set_mode(0o600);
    database_header.set_mtime(0);
    database_header.set_uid(0);
    database_header.set_gid(0);
    database_header.set_entry_type(EntryType::Regular);
    database_header.set_cksum();
    builder
        .append_data(
            &mut database_header,
            "microwave.db",
            database_bytes.as_slice(),
        )
        .unwrap();

    let mut header = Header::new_gnu();
    header.set_path("private/files/huge.pem").unwrap();
    header.set_size(17 * 1024 * 1024);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    builder.append(&header, std::io::empty()).unwrap();
    builder.finish().unwrap();

    let output = run_ctl(&installation.config, "restore", &archive_path, true, false);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("exceeds the 16777216-byte file limit"),
        "stderr: {}",
        stderr(&output)
    );
    installation.assert_live_marker_untouched(&before);
}

#[test]
fn rerunning_a_full_backup_keeps_the_existing_private_tree_byte_identical() {
    let installation = Installation::new(true);
    installation.seed_database(DB_SECRET);
    installation.write_secrets();

    let backup = installation.root.join("full-backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);

    let manifest = read_manifest(&backup);
    let before = manifest_entries(&manifest)
        .iter()
        .map(|entry| {
            let path = backup.join(entry["path"].as_str().unwrap());
            (path.clone(), std::fs::read(&path).unwrap(), mode(&path))
        })
        .collect::<Vec<_>>();
    let manifest_before = std::fs::read(backup.join("private/manifest.json")).unwrap();

    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("destination already contains"),
        "stderr: {}",
        stderr(&output)
    );

    assert_eq!(
        std::fs::read(backup.join("private/manifest.json")).unwrap(),
        manifest_before
    );
    for (path, contents, expected_mode) in before {
        assert_eq!(std::fs::read(&path).unwrap(), contents, "{path:?}");
        assert_eq!(mode(&path), expected_mode, "{path:?}");
    }

    let archive = installation.root.join("full-backup.tar");
    let output = run_ctl(&installation.config, "backup", &archive, true, true);
    assert_success(&output);
    let archive_before = std::fs::read(&archive).unwrap();
    let output = run_ctl(&installation.config, "backup", &archive, true, true);
    assert_failure(&output);
    assert_eq!(std::fs::read(&archive).unwrap(), archive_before);
}

#[test]
fn private_apply_failure_rolls_back_earlier_overwrites_and_cleans_successful_backups() {
    let installation = Installation::new(true);
    installation.seed_database(DB_SECRET);
    installation.write_secrets();

    let backup = installation.root.join("backup");
    let output = run_ctl(&installation.config, "backup", &backup, false, true);
    assert_success(&output);

    installation.replace_live_secrets();
    let pivotal = installation.data_dir.join("pivotal.pem");
    let tls_key = installation.tls_key.clone();
    let pivotal_before = std::fs::read(&pivotal).unwrap();
    let tls_before = std::fs::read(&tls_key).unwrap();
    let pivotal_mode = mode(&pivotal);
    let tls_mode = mode(&tls_key);

    let output = run_ctl_with_failpoint(
        &installation.config,
        "restore",
        &backup,
        false,
        true,
        true,
        Some(1),
    );
    assert_failure(&output);
    let message = stderr(&output);
    assert!(message.contains("applied 1 of 4"), "stderr: {message}");
    assert!(message.contains("rolled back"), "stderr: {message}");
    assert_eq!(std::fs::read(&pivotal).unwrap(), pivotal_before);
    assert_eq!(std::fs::read(&tls_key).unwrap(), tls_before);
    assert_eq!(mode(&pivotal), pivotal_mode);
    assert_eq!(mode(&tls_key), tls_mode);
    assert_no_staging_dirs(&installation.data_dir);
    assert_no_staging_dirs(installation.tls_key.parent().unwrap());

    let output = run_private_only(&installation.config, &backup, false);
    assert_success(&output);
    assert_eq!(std::fs::read_to_string(&pivotal).unwrap(), PIVOTAL_CONTENT);
    assert_eq!(std::fs::read_to_string(&tls_key).unwrap(), TLS_KEY_CONTENT);
    assert_no_staging_dirs(&installation.data_dir);
    assert_no_staging_dirs(installation.tls_key.parent().unwrap());
}

fn assert_no_staging_dirs(parent: &Path) {
    for entry in std::fs::read_dir(parent).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(".microwave-private-restore-")
                && !name.starts_with(".microwave-private-backup-preserved-"),
            "private staging/backup artifact left behind: {name:?}"
        );
    }
}

fn tar_entries(path: &Path) -> Vec<(PathBuf, tar::EntryType)> {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = tar::Archive::new(file);
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.path().unwrap().into_owned(),
                entry.header().entry_type(),
            )
        })
        .collect()
}
