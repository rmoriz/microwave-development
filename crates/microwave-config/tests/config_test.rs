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
    assert!(!c.data_collector.is_configured());
    assert_eq!(c.data_collector.timeout, 30_000);
    assert!(!c.required_recipe.enabled);
    assert!(!c.telemetry.enabled);
    assert_eq!(c.telemetry.interval_seconds, 86_400);
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
fn selects_the_opensearch_backend_from_toml() {
    let c = Config::from_toml_str(
        r#"
        [search]
        backend = "opensearch"
        url = "http://127.0.0.1:9200"
    "#,
    )
    .unwrap();
    assert_eq!(c.search.backend, SearchBackend::OpenSearch);
    assert_eq!(c.search.url.as_deref(), Some("http://127.0.0.1:9200"));
}

#[test]
fn search_backend_and_url_can_be_overridden_from_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_SEARCH__BACKEND", "elasticsearch");
    std::env::set_var("MW_SEARCH__URL", "http://127.0.0.1:9201");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.search.backend, SearchBackend::Elasticsearch);
    assert_eq!(c.search.url.as_deref(), Some("http://127.0.0.1:9201"));

    std::env::set_var("MW_SEARCH__BACKEND", "solr");
    let mut c = Config::default();
    assert!(c.apply_env().is_err());

    std::env::remove_var("MW_SEARCH__BACKEND");
    std::env::remove_var("MW_SEARCH__URL");
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

#[test]
fn toml_configures_data_collector_required_recipe_and_telemetry() {
    let c = Config::from_toml_str(
        r#"
        [data_collector]
        root_url = "https://automate.example.com/data-collector/v0/"
        token = "shared-secret"
        timeout = 1500

        [required_recipe]
        enable = true
        path = "/etc/microwave/required_recipe.rb"

        [telemetry]
        enabled = false
        reporting_url = "https://telemetry.example.com/ingest"
        interval_seconds = 3600
    "#,
    )
    .unwrap();

    assert!(c.data_collector.is_configured());
    assert_eq!(
        c.data_collector.root_url.as_deref(),
        Some("https://automate.example.com/data-collector/v0/")
    );
    assert_eq!(c.data_collector.token.as_deref(), Some("shared-secret"));
    assert_eq!(c.data_collector.timeout, 1500);
    assert!(c.required_recipe.enabled);
    assert_eq!(
        c.required_recipe.path.as_deref(),
        Some("/etc/microwave/required_recipe.rb")
    );
    assert!(!c.telemetry.enabled);
    assert_eq!(
        c.telemetry.reporting_url.as_deref(),
        Some("https://telemetry.example.com/ingest")
    );
    assert_eq!(c.telemetry.interval_seconds, 3600);
}

#[test]
fn env_overrides_data_collector_required_recipe_and_telemetry() {
    let _guard = ENV_LOCK.lock().unwrap();
    for (key, value) in [
        (
            "MW_DATA_COLLECTOR__ROOT_URL",
            "http://127.0.0.1:9999/ingest",
        ),
        ("MW_DATA_COLLECTOR__TOKEN", "env-token"),
        ("MW_DATA_COLLECTOR__TIMEOUT", "2500"),
        ("MW_REQUIRED_RECIPE__ENABLE", "true"),
        ("MW_REQUIRED_RECIPE__PATH", "/tmp/recipe.rb"),
        ("MW_TELEMETRY__ENABLED", "true"),
        (
            "MW_TELEMETRY__REPORTING_URL",
            "https://telemetry.example.com",
        ),
        ("MW_TELEMETRY__INTERVAL_SECONDS", "120"),
    ] {
        std::env::set_var(key, value);
    }

    let mut c = Config::default();
    c.apply_env().unwrap();

    for key in [
        "MW_DATA_COLLECTOR__ROOT_URL",
        "MW_DATA_COLLECTOR__TOKEN",
        "MW_DATA_COLLECTOR__TIMEOUT",
        "MW_REQUIRED_RECIPE__ENABLE",
        "MW_REQUIRED_RECIPE__PATH",
        "MW_TELEMETRY__ENABLED",
        "MW_TELEMETRY__REPORTING_URL",
        "MW_TELEMETRY__INTERVAL_SECONDS",
    ] {
        std::env::remove_var(key);
    }

    assert_eq!(
        c.data_collector.root_url.as_deref(),
        Some("http://127.0.0.1:9999/ingest")
    );
    assert_eq!(c.data_collector.token.as_deref(), Some("env-token"));
    assert_eq!(c.data_collector.timeout, 2500);
    assert!(c.required_recipe.enabled);
    assert_eq!(c.required_recipe.path.as_deref(), Some("/tmp/recipe.rb"));
    assert!(c.telemetry.enabled);
    assert_eq!(
        c.telemetry.reporting_url.as_deref(),
        Some("https://telemetry.example.com")
    );
    assert_eq!(c.telemetry.interval_seconds, 120);
}
