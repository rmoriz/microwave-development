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
fn serve_check_validates_the_tls_reload_interval() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = write_tls_pair(dir.path());

    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__TLS__CERT_PATH", &cert)
        .env("MW_SERVER__TLS__KEY_PATH", &key)
        .env("MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS", "60")
        .env_remove("MW_SERVER__TLS__ENABLED")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__TLS__CERT_PATH", &cert)
        .env("MW_SERVER__TLS__KEY_PATH", &key)
        .env(
            "MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS",
            (microwave_config::TlsConfig::MAX_RELOAD_INTERVAL_SECONDS + 1).to_string(),
        )
        .env_remove("MW_SERVER__TLS__ENABLED")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reload_interval_seconds"));

    let output = Command::cargo_bin("microwave-server")
        .unwrap()
        .args(["serve", "--check"])
        .env("MW_PATHS__DATA_DIR", dir.path())
        .env("MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS", "60")
        .env_remove("MW_SERVER__TLS__ENABLED")
        .env_remove("MW_SERVER__TLS__CERT_PATH")
        .env_remove("MW_SERVER__TLS__KEY_PATH")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires TLS to be enabled"));
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

struct GeneratedPair {
    cert_pem: String,
    key_pem: String,
    fingerprint: String,
}

/// Generate a self-signed pair and return its PEM contents plus the SHA-256
/// fingerprint of the DER leaf certificate.
fn generated_pair() -> GeneratedPair {
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    let fingerprint = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(certified.cert.der().as_ref()))
    };
    GeneratedPair {
        cert_pem: certified.cert.pem(),
        key_pem: certified.signing_key.serialize_pem(),
        fingerprint,
    }
}

fn write_pair_files(dir: &Path, pair: &GeneratedPair) -> (PathBuf, PathBuf) {
    let cert_path = dir.join("tls.crt");
    let key_path = dir.join("tls.key");
    std::fs::write(&cert_path, &pair.cert_pem).unwrap();
    std::fs::write(&key_path, &pair.key_pem).unwrap();
    (cert_path, key_path)
}

/// Replace a file via a temporary sibling and rename, matching the rotation
/// procedure documented for operators.
fn replace_file_atomically(path: &Path, contents: &str) {
    let file_name = path.file_name().unwrap().to_string_lossy();
    let temporary = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&temporary, contents).unwrap();
    std::fs::rename(&temporary, path).unwrap();
}

#[derive(Debug)]
struct CertificateCapture {
    cert: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    provider: std::sync::Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for CertificateCapture {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        *self.cert.lock().unwrap() = Some(end_entity.as_ref().to_vec());
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Complete a TLS handshake with the server and return the SHA-256 fingerprint
/// of the leaf certificate it presented. The request proves the listener
/// accepts traffic, not just that a handshake can be started.
fn served_fingerprint(addr: &str) -> Result<String, String> {
    use std::io::{Read, Write};

    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = CertificateCapture {
        cert: captured.clone(),
        provider: provider.clone(),
    };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(verifier))
        .with_no_client_auth();
    let server_name =
        rustls::pki_types::ServerName::try_from("localhost").map_err(|error| error.to_string())?;
    let connection = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
        .map_err(|error| error.to_string())?;
    let socket = std::net::TcpStream::connect(addr).map_err(|error| error.to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut tls = rustls::StreamOwned::new(connection, socket);
    tls.write_all(b"GET /_status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|error| error.to_string())?;
    let mut response = [0u8; 64];
    let _ = tls.read(&mut response);

    let cert = captured
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "the server presented no certificate".to_string())?;
    use sha2::Digest;
    Ok(hex::encode(sha2::Sha256::digest(&cert)))
}

fn wait_for_any_fingerprint(addr: &str) -> String {
    let mut last_error = String::new();
    for _ in 0..200 {
        match served_fingerprint(addr) {
            Ok(fingerprint) => return fingerprint,
            Err(error) => last_error = error,
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the TLS listener never completed a handshake: {last_error}");
}

fn wait_for_fingerprint(addr: &str, expected: &str) -> String {
    let mut last = String::new();
    for _ in 0..200 {
        match served_fingerprint(addr) {
            Ok(fingerprint) if fingerprint == expected => return fingerprint,
            Ok(fingerprint) => last = fingerprint,
            Err(error) => last = format!("handshake error: {error}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("fingerprint never became {expected}; last observed {last}");
}

fn wait_for_log(log_path: &Path, needle: &str) -> String {
    for _ in 0..200 {
        let text = std::fs::read_to_string(log_path).unwrap_or_default();
        if text.contains(needle) {
            return text;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let text = std::fs::read_to_string(log_path).unwrap_or_default();
    panic!("log never contained {needle:?}; log contents:\n{text}");
}

/// Perform one plain-HTTP request over a raw socket. Used by the listener
/// tests that do not need an async client.
fn plain_status(addr: &str) -> Result<String, String> {
    use std::io::{Read, Write};

    let mut stream = std::net::TcpStream::connect(addr).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET /_status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|error| error.to_string())?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| error.to_string())?;
    Ok(response)
}

fn wait_for_plain_status(addr: &str) -> String {
    let mut last = String::new();
    for _ in 0..200 {
        match plain_status(addr) {
            Ok(response) if response.contains("200 OK") && response.contains("pong") => {
                return response;
            }
            Ok(response) => last = response,
            Err(error) => last = error,
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("plain HTTP listener never served /_status; last response: {last}");
}

fn spawn_server(
    data_dir: &Path,
    bind: &str,
    tls: Option<(&Path, &Path)>,
    log_path: &Path,
    reload_interval_seconds: Option<u64>,
    extra_env: &[(&str, &str)],
) -> std::process::Child {
    let stderr = std::fs::File::create(log_path).unwrap();
    let stdout = stderr.try_clone().unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_microwave-server"));
    command
        .args(["serve"])
        .env("MW_PATHS__DATA_DIR", data_dir)
        .env("MW_SERVER__BIND", bind)
        .env_remove("MW_CONFIG")
        .env_remove("MW_SERVER__TLS__ENABLED")
        .env_remove("MW_SERVER__TLS__CERT_PATH")
        .env_remove("MW_SERVER__TLS__KEY_PATH")
        .env_remove("MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS")
        .env_remove("MW_KEYGEN__SIZE")
        .env_remove("MW_KEYGEN__WORKERS")
        .env_remove("MW_KEYGEN__WARM_UP")
        .env("RUST_LOG", "info")
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr));
    if let Some((cert_path, key_path)) = tls {
        command
            .env("MW_SERVER__TLS__CERT_PATH", cert_path)
            .env("MW_SERVER__TLS__KEY_PATH", key_path);
    }
    if let Some(seconds) = reload_interval_seconds {
        command.env(
            "MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS",
            seconds.to_string(),
        );
    }
    for (name, value) in extra_env {
        command.env(name, value);
    }
    command.spawn().expect("spawn microwave-server")
}

#[cfg(unix)]
fn send_signal(child: &std::process::Child, signal: libc::c_int) {
    let result = unsafe { libc::kill(child.id() as libc::pid_t, signal) };
    assert_eq!(
        result,
        0,
        "kill({signal}) failed: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(unix)]
fn send_sighup(child: &std::process::Child) {
    send_signal(child, libc::SIGHUP);
}

#[cfg(unix)]
fn wait_for_exit(child: &mut std::process::Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            panic!("server did not exit within {timeout:?} after the shutdown signal");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
#[test]
fn sighup_during_startup_does_not_terminate() {
    let port_guard = PORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let first = generated_pair();
    let (cert, key) = write_pair_files(dir.path(), &first);
    let log = dir.path().join("server.log");
    let bind = ephemeral_addr();

    // A synchronous keygen warm-up runs between `AppState` construction and
    // the listener bind. One 2048-bit RSA keypair takes well over the 100 ms
    // poll interval, keeping startup in progress when the signal is sent.
    let child = spawn_server(
        dir.path(),
        &bind,
        Some((&cert, &key)),
        &log,
        None,
        &[
            ("MW_KEYGEN__SIZE", "1"),
            ("MW_KEYGEN__WORKERS", "0"),
            ("MW_KEYGEN__WARM_UP", "true"),
        ],
    );
    let mut guard = ServerProcess(Some(child));

    let text = wait_for_log(&log, "installed SIGHUP handler");
    assert!(
        !text.contains("microwave-server listening"),
        "startup finished before the signal could be sent; log:\n{text}"
    );
    send_sighup(guard.0.as_ref().unwrap());

    wait_for_tcp(&bind);
    drop(port_guard);
    assert_eq!(wait_for_any_fingerprint(&bind), first.fingerprint);
    assert!(
        guard.0.as_mut().unwrap().try_wait().unwrap().is_none(),
        "the server exited after a SIGHUP during startup"
    );

    let text = std::fs::read_to_string(&log).unwrap();
    let handler = text.find("installed SIGHUP handler").unwrap();
    let listening = text.find("microwave-server listening").unwrap();
    assert!(
        handler < listening,
        "the SIGHUP handler must be installed before the listener binds; log:\n{text}"
    );
    wait_for_log(&log, "reloaded TLS certificate and key");
}

#[cfg(unix)]
#[test]
fn sighup_on_a_plain_http_listener_is_safe() {
    let port_guard = PORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("server.log");
    let bind = ephemeral_addr();

    let child = spawn_server(dir.path(), &bind, None, &log, None, &[]);
    let mut guard = ServerProcess(Some(child));
    wait_for_tcp(&bind);
    drop(port_guard);
    wait_for_plain_status(&bind);

    send_sighup(guard.0.as_ref().unwrap());
    let text = wait_for_log(&log, "TLS is disabled; nothing to reload");
    assert!(text.contains("installed SIGHUP handler"));
    assert!(
        guard.0.as_mut().unwrap().try_wait().unwrap().is_none(),
        "the server exited after a SIGHUP on a plain-HTTP listener"
    );

    // The handler stays installed; a second signal is also harmless.
    send_sighup(guard.0.as_ref().unwrap());
    wait_for_plain_status(&bind);
    assert!(guard.0.as_mut().unwrap().try_wait().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn sigterm_shuts_down_plain_http_gracefully() {
    let port_guard = PORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("server.log");
    let bind = ephemeral_addr();

    // Disable background keygen workers so this test measures the shutdown
    // path, not debug-build RSA generation joined during process teardown.
    let child = spawn_server(
        dir.path(),
        &bind,
        None,
        &log,
        None,
        &[("MW_KEYGEN__WORKERS", "0"), ("MW_KEYGEN__WARM_UP", "false")],
    );
    let mut guard = ServerProcess(Some(child));
    wait_for_tcp(&bind);
    drop(port_guard);
    wait_for_plain_status(&bind);

    send_signal(guard.0.as_ref().unwrap(), libc::SIGTERM);
    let status = wait_for_exit(guard.0.as_mut().unwrap(), Duration::from_secs(15));
    let _ = guard.0.take();

    assert_eq!(
        status.code(),
        Some(0),
        "the server should exit cleanly after SIGTERM, not via the default signal action"
    );
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("installed SIGTERM/SIGINT shutdown handlers"));
    assert!(text.contains("shutdown signal received; draining in-flight requests"));
}

#[cfg(unix)]
#[test]
fn sigint_shuts_down_tls_gracefully() {
    let port_guard = PORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let pair = generated_pair();
    let (cert, key) = write_pair_files(dir.path(), &pair);
    let log = dir.path().join("server.log");
    let bind = ephemeral_addr();

    // Disable background keygen workers so this test measures the shutdown
    // path, not debug-build RSA generation joined during process teardown.
    let child = spawn_server(
        dir.path(),
        &bind,
        Some((&cert, &key)),
        &log,
        None,
        &[("MW_KEYGEN__WORKERS", "0"), ("MW_KEYGEN__WARM_UP", "false")],
    );
    let mut guard = ServerProcess(Some(child));
    wait_for_tcp(&bind);
    drop(port_guard);
    assert_eq!(wait_for_any_fingerprint(&bind), pair.fingerprint);

    send_signal(guard.0.as_ref().unwrap(), libc::SIGINT);
    let status = wait_for_exit(guard.0.as_mut().unwrap(), Duration::from_secs(15));
    let _ = guard.0.take();

    assert_eq!(
        status.code(),
        Some(0),
        "the TLS server should exit cleanly after SIGINT"
    );
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("SIGINT"));
    assert!(text.contains("shutdown signal received; draining in-flight requests"));
}

#[cfg(unix)]
#[test]
fn sighup_reloads_a_rotated_tls_pair_and_keeps_the_old_one_on_failure() {
    let port_guard = PORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let first = generated_pair();
    let second = generated_pair();
    let (cert, key) = write_pair_files(dir.path(), &first);
    let log = dir.path().join("server.log");
    let bind = ephemeral_addr();

    let child = spawn_server(dir.path(), &bind, Some((&cert, &key)), &log, None, &[]);
    let mut guard = ServerProcess(Some(child));
    wait_for_tcp(&bind);
    drop(port_guard);
    assert_eq!(wait_for_any_fingerprint(&bind), first.fingerprint);

    // Rotate the files in place, as a cert-manager or deploy hook would.
    replace_file_atomically(&cert, &second.cert_pem);
    replace_file_atomically(&key, &second.key_pem);
    send_sighup(guard.0.as_ref().unwrap());
    wait_for_log(&log, "reloaded TLS certificate and key");
    assert_eq!(
        wait_for_fingerprint(&bind, &second.fingerprint),
        second.fingerprint
    );

    // A broken replacement must be rejected: the previous (second) pair stays
    // active and the process keeps serving.
    replace_file_atomically(&cert, "-----BEGIN CERTIFICATE-----\nnot a certificate\n");
    send_sighup(guard.0.as_ref().unwrap());
    let text = wait_for_log(&log, "TLS certificate reload failed");
    assert!(
        text.contains("keeping the previous certificate"),
        "unexpected log: {text}"
    );
    assert!(
        guard.0.as_mut().unwrap().try_wait().unwrap().is_none(),
        "the server exited after a failed reload"
    );
    assert_eq!(
        wait_for_fingerprint(&bind, &second.fingerprint),
        second.fingerprint
    );
}

#[test]
fn periodic_reload_picks_up_a_rotated_tls_pair_without_a_signal() {
    let port_guard = PORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let first = generated_pair();
    let second = generated_pair();
    let (cert, key) = write_pair_files(dir.path(), &first);
    let log = dir.path().join("server.log");
    let bind = ephemeral_addr();

    let child = spawn_server(dir.path(), &bind, Some((&cert, &key)), &log, Some(1), &[]);
    let _guard = ServerProcess(Some(child));
    wait_for_tcp(&bind);
    drop(port_guard);
    assert_eq!(wait_for_any_fingerprint(&bind), first.fingerprint);

    // Ensure the replacement has a distinct mtime even on filesystems with
    // coarse timestamp granularity; the polling reload is stamp-based.
    std::thread::sleep(Duration::from_millis(1_100));
    replace_file_atomically(&cert, &second.cert_pem);
    replace_file_atomically(&key, &second.key_pem);

    assert_eq!(
        wait_for_fingerprint(&bind, &second.fingerprint),
        second.fingerprint
    );
    wait_for_log(&log, "reloaded TLS certificate and key");
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
