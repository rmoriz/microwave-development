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
        &["migrate-from-legacy", "--input", snapshot.to_str().unwrap()],
    );
    assert!(!imported.status.success());
    assert!(stderr(&imported).contains("unsupported snapshot version 99"));
}
