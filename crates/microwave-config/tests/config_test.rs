use microwave_config::{Config, DatabaseDriver, ObjectStoreBackend, SearchBackend};
use std::sync::Mutex;

/// Environment mutation is process-wide, so tests that set MW_* variables must
/// not overlap. In particular an invalid value in one test would otherwise
/// make an unrelated `apply_env` call fail.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn defaults_are_safe_and_local() {
    let c = Config::default();
    assert_eq!(c.server.bind, "0.0.0.0:8000");
    assert_eq!(c.paths.data_dir, "./data");
    assert_eq!(c.database.driver, DatabaseDriver::Sqlite);
    assert_eq!(c.search.backend, SearchBackend::Embedded);
    assert_eq!(c.object_store.backend, ObjectStoreBackend::Local);
    assert_eq!(c.auth.skew_seconds, 900);
    assert_eq!(c.auth.superuser_name, "pivotal");
}

#[test]
fn toml_overrides_defaults() {
    let c = Config::from_toml_str(
        r#"
        [server]
        bind = "127.0.0.1:9000"
        [auth]
        skew_seconds = 60
    "#,
    )
    .unwrap();
    assert_eq!(c.server.bind, "127.0.0.1:9000");
    assert_eq!(c.auth.skew_seconds, 60);
    assert_eq!(c.paths.data_dir, "./data");
}

#[test]
fn env_overrides_toml() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_SERVER__BIND", "127.0.0.1:9100");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.server.bind, "127.0.0.1:9100");
    std::env::remove_var("MW_SERVER__BIND");
}

#[test]
fn sqlite_url_is_derived_from_data_dir() {
    let mut c = Config::default();
    c.paths.data_dir = "/tmp/mw".into();
    assert_eq!(c.resolved_database_url(), "sqlite:///tmp/mw/microwave.db");
}

#[test]
fn object_store_defaults_are_local_and_path_style() {
    let c = Config::default();
    assert_eq!(c.object_store.region, "us-east-1");
    assert!(c.object_store.path_style);
    assert_eq!(c.object_store.access_key, None);
    assert_eq!(c.object_store.secret_key, None);
}

#[test]
fn toml_settings_configure_s3_backend() {
    let c = Config::from_toml_str(
        r#"
        [object_store]
        backend = "s3"
        endpoint = "https://s3.example.com"
        bucket = "chef"
        region = "eu-central-1"
        access_key = "access"
        secret_key = "secret"
        path_style = false
    "#,
    )
    .unwrap();
    assert_eq!(c.object_store.backend, ObjectStoreBackend::S3);
    assert_eq!(
        c.object_store.endpoint.as_deref(),
        Some("https://s3.example.com")
    );
    assert_eq!(c.object_store.bucket.as_deref(), Some("chef"));
    assert_eq!(c.object_store.region, "eu-central-1");
    assert_eq!(c.object_store.access_key.as_deref(), Some("access"));
    assert_eq!(c.object_store.secret_key.as_deref(), Some("secret"));
    assert!(!c.object_store.path_style);
}

#[test]
fn env_overrides_s3_settings() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_OBJECT_STORE__BACKEND", "garage");
    std::env::set_var("MW_OBJECT_STORE__ENDPOINT", "http://garage:3900");
    std::env::set_var("MW_OBJECT_STORE__BUCKET", "chef");
    std::env::set_var("MW_OBJECT_STORE__REGION", "garage");
    std::env::set_var("MW_OBJECT_STORE__ACCESS_KEY", "access");
    std::env::set_var("MW_OBJECT_STORE__SECRET_KEY", "secret");
    std::env::set_var("MW_OBJECT_STORE__PATH_STYLE", "false");

    let mut c = Config::default();
    c.apply_env().unwrap();

    std::env::remove_var("MW_OBJECT_STORE__BACKEND");
    std::env::remove_var("MW_OBJECT_STORE__ENDPOINT");
    std::env::remove_var("MW_OBJECT_STORE__BUCKET");
    std::env::remove_var("MW_OBJECT_STORE__REGION");
    std::env::remove_var("MW_OBJECT_STORE__ACCESS_KEY");
    std::env::remove_var("MW_OBJECT_STORE__SECRET_KEY");
    std::env::remove_var("MW_OBJECT_STORE__PATH_STYLE");

    assert_eq!(c.object_store.backend, ObjectStoreBackend::Garage);
    assert_eq!(
        c.object_store.endpoint.as_deref(),
        Some("http://garage:3900")
    );
    assert_eq!(c.object_store.bucket.as_deref(), Some("chef"));
    assert_eq!(c.object_store.region, "garage");
    assert_eq!(c.object_store.access_key.as_deref(), Some("access"));
    assert_eq!(c.object_store.secret_key.as_deref(), Some("secret"));
    assert!(!c.object_store.path_style);

    std::env::set_var("MW_OBJECT_STORE__PATH_STYLE", "sometimes");
    let result = c.apply_env();
    std::env::remove_var("MW_OBJECT_STORE__PATH_STYLE");
    assert!(result.is_err());
}
