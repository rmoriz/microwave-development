use assert_cmd::Command;
use base64::{engine::general_purpose::STANDARD, Engine};
use microwave_crypto::{sign_request, CanonicalRequest, SignVersion};
use microwave_store::{
    ActorType, BOOKSHELF_ACCESS_KEY_ID, BOOKSHELF_SECRET_ACCESS_KEY, BOOKSHELF_SECRET_GROUP,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PRIVATE_KEY: &str =
    include_str!("../../../crates/microwave-crypto/tests/fixtures/test_key.pem");
const PUBLIC_KEY: &str =
    include_str!("../../../crates/microwave-crypto/tests/fixtures/test_key_pub.pem");

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

/// Serializes port selection and startup between the listener tests so two
/// concurrently running tests cannot pick the same ephemeral port.
static PORT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Pick an ephemeral port by binding and immediately releasing it. The server
/// then binds the same address; the small race is acceptable for a test.
fn ephemeral_addr() -> String {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    addr.to_string()
}

async fn wait_for_status(client: &reqwest::Client, url: &str) -> serde_json::Value {
    let mut last_error = None;
    for _ in 0..600 {
        match client.get(url).send().await {
            Ok(response) if response.status().is_success() => {
                return response.json().await.unwrap();
            }
            Ok(response) => {
                last_error = Some(format!("unexpected status {}", response.status()));
            }
            Err(error) => last_error = Some(format!("{error:?}")),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("listener never became ready at {url}: {last_error:?}");
}

/// Block until the child process has bound its listener. Keeping this
/// synchronous lets the port lock be released before any async request.
fn wait_for_tcp(bind: &str) {
    for _ in 0..600 {
        if std::net::TcpStream::connect(bind).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("listener never bound {bind}");
}

/// Create the bootstrap superuser and its key before the server starts so the
/// test can sign requests without waiting for and reading generated key files.
fn seed_pivotal(data_dir: &Path) {
    let store = microwave_store::SqliteStore::open(&data_dir.join("microwave.db")).unwrap();
    let user = store
        .create_user("pivotal", "pivotal@localhost", "bcrypt", "h", "s", true)
        .unwrap();
    store
        .add_actor_key(&user.id, ActorType::User, "default", PUBLIC_KEY)
        .unwrap();
}

fn signed_headers(method: &str, path: &str, body: &[u8]) -> Vec<(String, String)> {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let canonical = CanonicalRequest {
        method,
        path,
        body,
        user_id: "pivotal",
        timestamp: &timestamp,
        api_version: Some("2"),
        version: SignVersion::V1_3,
    };
    let signed = sign_request(PRIVATE_KEY, &canonical).unwrap();
    let encoded = STANDARD.encode(signed.signature);
    let mut headers = vec![
        (
            "x-ops-sign".to_string(),
            "algorithm=sha256;version=1.3;".to_string(),
        ),
        ("x-ops-userid".to_string(), "pivotal".to_string()),
        ("x-ops-timestamp".to_string(), signed.timestamp),
        ("x-ops-content-hash".to_string(), signed.content_hash),
        ("x-ops-server-api-version".to_string(), "2".to_string()),
    ];
    for (index, chunk) in encoded.as_bytes().chunks(60).enumerate() {
        headers.push((
            format!("x-ops-authorization-{}", index + 1),
            String::from_utf8(chunk.to_vec()).unwrap(),
        ));
    }
    headers
}

#[tokio::test(flavor = "multi_thread")]
async fn http2_tls_urls_use_the_request_authority() {
    let port_guard = PORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    seed_pivotal(dir.path());
    let (cert, key) = write_tls_pair(dir.path());
    let bind = ephemeral_addr();
    let port = bind.rsplit_once(':').unwrap().1.parse::<u16>().unwrap();

    let child = std::process::Command::new(env!("CARGO_BIN_EXE_microwave-server"))
        .args(["serve"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__BIND", &bind)
        .env("MW_SERVER__TLS__CERT_PATH", &cert)
        .env("MW_SERVER__TLS__KEY_PATH", &key)
        .env_remove("MW_CONFIG")
        .env_remove("MW_SERVER__TLS__ENABLED")
        .env("RUST_LOG", "info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn microwave-server");
    let _guard = ServerProcess(Some(child));
    wait_for_tcp(&bind);
    drop(port_guard);

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .resolve("chef.example.com", ([127, 0, 0, 1], port).into())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let headers = signed_headers("GET", "/users", b"");
    let mut request = client.get(format!("https://chef.example.com:{port}/users"));
    for (name, value) in &headers {
        request = request.header(name, value);
    }
    let response = request.send().await.unwrap();
    assert_eq!(
        response.version(),
        reqwest::Version::HTTP_2,
        "the client must negotiate HTTP/2 over TLS"
    );
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["pivotal"],
        format!("https://chef.example.com:{port}/users/pivotal"),
        "generated URL must use the HTTP/2 authority, not the bind address"
    );

    // A proxy header must still override the HTTP/2 authority.
    let mut request = client.get(format!("https://chef.example.com:{port}/users"));
    for (name, value) in &headers {
        request = request.header(name, value);
    }
    let response = request
        .header("x-forwarded-host", "proxy.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(response.version(), reqwest::Version::HTTP_2);
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["pivotal"], "https://proxy.example.com/users/pivotal");
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_http_listener_serves_status_and_does_not_serve_https() {
    let port_guard = PORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let bind = ephemeral_addr();

    let child = std::process::Command::new(env!("CARGO_BIN_EXE_microwave-server"))
        .args(["serve"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__BIND", &bind)
        .env_remove("MW_CONFIG")
        .env_remove("MW_SERVER__TLS__CERT_PATH")
        .env_remove("MW_SERVER__TLS__KEY_PATH")
        .env_remove("MW_SERVER__TLS__ENABLED")
        .env("RUST_LOG", "info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn microwave-server");
    let _guard = ServerProcess(Some(child));
    wait_for_tcp(&bind);
    drop(port_guard);

    let plain = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let body = wait_for_status(&plain, &format!("http://{bind}/_status")).await;
    assert_eq!(body["status"], "pong");

    // A TLS handshake must fail on a plain listener.
    let tls = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    assert!(
        tls.get(format!("https://{bind}/_status"))
            .send()
            .await
            .is_err(),
        "HTTPS unexpectedly succeeded on a plain listener"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_listener_serves_https_and_rejects_plain_http() {
    let port_guard = PORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = write_tls_pair(dir.path());
    let bind = ephemeral_addr();

    let child = std::process::Command::new(env!("CARGO_BIN_EXE_microwave-server"))
        .args(["serve"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__BIND", &bind)
        .env("MW_SERVER__TLS__CERT_PATH", &cert)
        .env("MW_SERVER__TLS__KEY_PATH", &key)
        .env_remove("MW_CONFIG")
        .env_remove("MW_SERVER__TLS__ENABLED")
        .env("RUST_LOG", "info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn microwave-server");
    let _guard = ServerProcess(Some(child));
    wait_for_tcp(&bind);
    drop(port_guard);

    let https = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let body = wait_for_status(&https, &format!("https://{bind}/_status")).await;
    assert_eq!(body["status"], "pong");

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
