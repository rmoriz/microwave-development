use assert_cmd::Command;
use microwave_store::{
    BOOKSHELF_ACCESS_KEY_ID, BOOKSHELF_SECRET_ACCESS_KEY, BOOKSHELF_SECRET_GROUP,
};

#[test]
fn version_prints_a_semver() {
    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .arg("version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn config_prints_json_defaults() {
    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .arg("config")
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["server"]["bind"], "0.0.0.0:8000");
    assert_eq!(value["paths"]["data_dir"], "./data");
}

#[test]
fn serve_check_reports_configuration_and_exits_successfully() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("configuration ok"));
}

#[test]
fn serve_check_accepts_an_s3_object_store() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_OBJECT_STORE__BACKEND", "garage")
        .env("MW_OBJECT_STORE__ENDPOINT", "http://127.0.0.1:3900")
        .env("MW_OBJECT_STORE__BUCKET", "chef")
        .env("MW_OBJECT_STORE__REGION", "garage")
        .env("MW_OBJECT_STORE__ACCESS_KEY", "access")
        .env("MW_OBJECT_STORE__SECRET_KEY", "secret")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("configuration ok"));
}

#[test]
fn serve_check_reports_missing_s3_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_OBJECT_STORE__BACKEND", "s3")
        .env("MW_OBJECT_STORE__ENDPOINT", "http://127.0.0.1:3900")
        .env("MW_OBJECT_STORE__BUCKET", "chef")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("access_key"));
}

#[test]
fn serve_check_uses_bookshelf_credentials_from_the_secret_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = microwave_store::SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
    store
        .set_secret(BOOKSHELF_SECRET_GROUP, BOOKSHELF_ACCESS_KEY_ID, "access")
        .unwrap();
    store
        .set_secret(
            BOOKSHELF_SECRET_GROUP,
            BOOKSHELF_SECRET_ACCESS_KEY,
            "secret",
        )
        .unwrap();
    drop(store);

    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_OBJECT_STORE__BACKEND", "s3")
        .env("MW_OBJECT_STORE__ENDPOINT", "http://127.0.0.1:3900")
        .env("MW_OBJECT_STORE__BUCKET", "chef")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn serve_check_persists_the_local_object_store_secret() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());

    let file_secret = std::fs::read_to_string(dir.path().join("objectstore.secret")).unwrap();
    let store = microwave_store::SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
    let stored = store.get_secret("object_store", "secret").unwrap().unwrap();
    assert_eq!(stored, file_secret.trim());
}
