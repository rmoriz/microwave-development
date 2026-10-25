use assert_cmd::Command;
use microwave_store::{
    BOOKSHELF_ACCESS_KEY_ID, BOOKSHELF_SECRET_ACCESS_KEY, BOOKSHELF_SECRET_GROUP,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Generate a throwaway self-signed certificate. Nothing is committed: tests
/// write the PEM pair into their own temporary directory and the child process
/// reads it from there.
fn write_tls_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    let cert_path = dir.join("tls.crt");
    let key_path = dir.join("tls.key");
    std::fs::write(&cert_path, certified.cert.pem()).unwrap();
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).unwrap();
    (cert_path, key_path)
}

fn key_material(key_path: &Path) -> String {
    std::fs::read_to_string(key_path)
        .unwrap()
        .lines()
        .find(|line| !line.starts_with("-----"))
        .unwrap()
        .to_string()
}

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
fn config_redacts_database_data_collector_and_object_store_credentials() {
    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .arg("config")
        .env(
            "MW_DATABASE__URL",
            "postgres://microwave:s3cr3t@db.example.com/microwave",
        )
        .env("MW_DATA_COLLECTOR__TOKEN", "collector-token")
        .env("MW_OBJECT_STORE__SECRET", "bookshelf-secret")
        .env("MW_OBJECT_STORE__ACCESS_KEY", "access-key")
        .env("MW_OBJECT_STORE__SECRET_KEY", "secret-key")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains("s3cr3t"));
    assert!(!text.contains("collector-token"));
    assert!(!text.contains("bookshelf-secret"));
    assert!(!text.contains("access-key"));
    assert!(!text.contains("secret-key"));
    assert!(text.contains("postgres://microwave:***@db.example.com/microwave"));
    assert!(text.contains("\"token\": \"***\""));
    assert!(text.contains("\"secret\": \"***\""));
    assert!(text.contains("\"access_key\": \"***\""));
    assert!(text.contains("\"secret_key\": \"***\""));
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

#[test]
fn serve_check_accepts_a_tls_pair_and_never_prints_key_material() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = write_tls_pair(dir.path());
    let secret = key_material(&key);

    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__TLS__CERT_PATH", &cert)
        .env("MW_SERVER__TLS__KEY_PATH", &key)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("configuration ok"), "stderr: {stderr}");
    assert!(stderr.contains("scheme https"), "stderr: {stderr}");
    assert!(!stderr.contains(&secret));
    assert!(!stderr.contains("PRIVATE KEY"));
}

#[test]
fn serve_check_rejects_a_missing_tls_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let (_cert, key) = write_tls_pair(dir.path());
    let missing = dir.path().join("does-not-exist.crt");

    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__TLS__CERT_PATH", &missing)
        .env("MW_SERVER__TLS__KEY_PATH", &key)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to read TLS certificate file"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("does-not-exist.crt"), "stderr: {stderr}");
}

#[test]
fn serve_check_rejects_a_certificate_and_key_that_do_not_match() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (cert, _first_key) = write_tls_pair(first.path());
    let (_second_cert, key) = write_tls_pair(second.path());

    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", first.path())
        .env("MW_SERVER__TLS__CERT_PATH", &cert)
        .env("MW_SERVER__TLS__KEY_PATH", &key)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("matching pair"));
}

#[test]
fn serve_check_requires_both_tls_paths_when_tls_is_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, _key) = write_tls_pair(dir.path());

    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__TLS__ENABLED", "true")
        .env("MW_SERVER__TLS__CERT_PATH", &cert)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("server.tls.key_path"));
}

struct ServerProcess(Option<std::process::Child>);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Pick an ephemeral port by binding and immediately releasing it. The server
/// then binds the same address; the small race is acceptable for a test.
fn ephemeral_addr() -> String {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    addr.to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_listener_serves_https_and_rejects_plain_http() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = write_tls_pair(dir.path());
    let bind = ephemeral_addr();

    let child = std::process::Command::new(env!("CARGO_BIN_EXE_microwave-server"))
        .args(["serve"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__BIND", &bind)
        .env("MW_SERVER__TLS__CERT_PATH", &cert)
        .env("MW_SERVER__TLS__KEY_PATH", &key)
        .env("RUST_LOG", "info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn microwave-server");
    let _guard = ServerProcess(Some(child));

    let https = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let url = format!("https://{bind}/_status");

    let mut last_error = None;
    let mut ready = false;
    for _ in 0..600 {
        match https.get(&url).send().await {
            Ok(response) if response.status().is_success() => {
                let body: serde_json::Value = response.json().await.unwrap();
                assert_eq!(body["status"], "pong");
                ready = true;
                break;
            }
            Ok(response) => {
                last_error = Some(format!("unexpected status {}", response.status()));
            }
            Err(error) => last_error = Some(format!("{error:?}")),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "HTTPS listener never became ready: {last_error:?}");

    // The same port must not accept plain HTTP: a plaintext request cannot
    // complete a TLS handshake, so the client reports a transport error.
    let plain = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let plain_result = plain.get(format!("http://{bind}/_status")).send().await;
    assert!(
        plain_result.is_err(),
        "plain HTTP unexpectedly succeeded: {:?}",
        plain_result.map(|response| response.status())
    );
}
