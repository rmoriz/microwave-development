use microwave_config::{
    Config, DataCollectorStore, DatabaseDriver, MailAuth, MailConfig, MailSecurity, McpConfig,
    McpOrgConfig, ObjectStoreBackend, SearchBackend, StorageCompression,
};
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
    assert_eq!(c.storage.compression, StorageCompression::Zlib);
    assert_eq!(c.search.backend, SearchBackend::Embedded);
    assert_eq!(c.search.timeout_ms, 30_000);
    assert_eq!(c.object_store.backend, ObjectStoreBackend::Local);
    assert_eq!(c.auth.skew_seconds, 900);
    assert_eq!(c.auth.superuser_name, "pivotal");
    assert_eq!(c.keygen.size, 3);
    assert_eq!(c.keygen.workers, 1);
    assert!(!c.keygen.warm_up);
    assert!(!c.data_collector.is_configured());
    assert_eq!(c.data_collector.timeout, 30_000);
    assert!(!c.data_collector.actions_enabled);
    assert!(!c.data_collector.actions_enabled());
    assert_eq!(c.data_collector.service_hostname, None);
    assert!(!c.compliance_proxy.is_configured());
    assert_eq!(c.compliance_proxy.timeout, 30_000);
    assert_eq!(c.compliance_proxy.token_header, None);
    assert!(!c.compliance_proxy.insecure_skip_tls_verify);
    assert!(!c.required_recipe.enabled);
    assert!(!c.telemetry.enabled);
    assert_eq!(c.telemetry.interval_seconds, 86_400);
    assert!(!c.server.tls.is_enabled());
    assert_eq!(c.server.tls.cert_path, None);
    assert_eq!(c.server.tls.key_path, None);
    assert_eq!(c.server.tls.reload_interval_seconds, 0);
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
fn keygen_settings_can_be_overridden_from_toml_and_env() {
    let c = Config::from_toml_str(
        r#"
        [keygen]
        size = 5
        workers = 2
        warm_up = true
    "#,
    )
    .unwrap();
    assert_eq!(c.keygen.size, 5);
    assert_eq!(c.keygen.workers, 2);
    assert!(c.keygen.warm_up);

    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_KEYGEN__SIZE", "4");
    std::env::set_var("MW_KEYGEN__WORKERS", "2");
    std::env::set_var("MW_KEYGEN__WARM_UP", "yes");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.keygen.size, 4);
    assert_eq!(c.keygen.workers, 2);
    assert!(c.keygen.warm_up);

    std::env::set_var("MW_KEYGEN__SIZE", "not-a-number");
    let mut c = Config::default();
    assert!(c.apply_env().is_err());

    std::env::set_var("MW_KEYGEN__SIZE", "2");
    std::env::set_var("MW_KEYGEN__WORKERS", "3");
    let mut c = Config::default();
    assert!(
        c.apply_env().is_err(),
        "workers above size must be rejected"
    );

    std::env::set_var("MW_KEYGEN__SIZE", "2048");
    std::env::set_var("MW_KEYGEN__WORKERS", "1");
    let mut c = Config::default();
    assert!(c.apply_env().is_err(), "oversized cache must be rejected");

    std::env::remove_var("MW_KEYGEN__SIZE");
    std::env::remove_var("MW_KEYGEN__WORKERS");
    std::env::remove_var("MW_KEYGEN__WARM_UP");
}

#[test]
fn keygen_rejects_invalid_toml_combinations() {
    assert!(Config::from_toml_str(
        r#"
        [keygen]
        size = 2048
    "#
    )
    .is_err());

    assert!(Config::from_toml_str(
        r#"
        [keygen]
        size = 2
        workers = 3
    "#
    )
    .is_err());

    assert!(Config::from_toml_str(
        r#"
        [keygen]
        size = 0
        warm_up = true
    "#
    )
    .is_err());

    assert!(Config::from_toml_str(
        r#"
        [keygen]
        size = 4
        workers = 9
    "#
    )
    .is_err());
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
    std::env::set_var("MW_SEARCH__TIMEOUT_MS", "5000");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.search.backend, SearchBackend::Elasticsearch);
    assert_eq!(c.search.url.as_deref(), Some("http://127.0.0.1:9201"));
    assert_eq!(c.search.timeout_ms, 5000);

    std::env::set_var("MW_SEARCH__BACKEND", "solr");
    let mut c = Config::default();
    assert!(c.apply_env().is_err());

    std::env::set_var("MW_SEARCH__TIMEOUT_MS", "0");
    std::env::remove_var("MW_SEARCH__BACKEND");
    let mut c = Config::default();
    assert!(c.apply_env().is_err());

    std::env::remove_var("MW_SEARCH__BACKEND");
    std::env::remove_var("MW_SEARCH__URL");
    std::env::remove_var("MW_SEARCH__TIMEOUT_MS");
}

#[test]
fn storage_compression_can_be_disabled_from_toml_and_env() {
    let c = Config::from_toml_str(
        r#"
        [storage]
        compression = "none"
    "#,
    )
    .unwrap();
    assert_eq!(c.storage.compression, StorageCompression::None);

    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_STORAGE__COMPRESSION", "none");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.storage.compression, StorageCompression::None);

    std::env::set_var("MW_STORAGE__COMPRESSION", "gzip");
    assert!(c.apply_env().is_err());
    std::env::remove_var("MW_STORAGE__COMPRESSION");
}

#[test]
fn toml_configures_tls_and_enables_it_when_paths_are_set() {
    let c = Config::from_toml_str(
        r#"
        [server]
        bind = "0.0.0.0:8443"

        [server.tls]
        cert_path = "/etc/microwave/tls.crt"
        key_path = "/etc/microwave/tls.key"
    "#,
    )
    .unwrap();
    assert_eq!(
        c.server.tls.cert_path.as_deref(),
        Some("/etc/microwave/tls.crt")
    );
    assert_eq!(
        c.server.tls.key_path.as_deref(),
        Some("/etc/microwave/tls.key")
    );
    assert_eq!(c.server.tls.enabled, None);
    assert!(c.server.tls.is_enabled());
}

#[test]
fn tls_can_be_explicitly_disabled_even_with_paths() {
    let c = Config::from_toml_str(
        r#"
        [server.tls]
        enabled = false
        cert_path = "/etc/microwave/tls.crt"
        key_path = "/etc/microwave/tls.key"
    "#,
    )
    .unwrap();
    assert!(!c.server.tls.is_enabled());
}

#[test]
fn tls_env_overrides_toml_paths_and_enable_flag() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_SERVER__TLS__ENABLED", "true");
    std::env::set_var("MW_SERVER__TLS__CERT_PATH", "/env/tls.crt");
    std::env::set_var("MW_SERVER__TLS__KEY_PATH", "/env/tls.key");

    let mut c = Config::from_toml_str(
        r#"
        [server.tls]
        cert_path = "/toml/tls.crt"
        key_path = "/toml/tls.key"
    "#,
    )
    .unwrap();
    c.apply_env().unwrap();

    std::env::remove_var("MW_SERVER__TLS__ENABLED");
    std::env::remove_var("MW_SERVER__TLS__CERT_PATH");
    std::env::remove_var("MW_SERVER__TLS__KEY_PATH");

    assert_eq!(c.server.tls.enabled, Some(true));
    assert_eq!(c.server.tls.cert_path.as_deref(), Some("/env/tls.crt"));
    assert_eq!(c.server.tls.key_path.as_deref(), Some("/env/tls.key"));
    assert!(c.server.tls.is_enabled());
}

#[test]
fn tls_env_rejects_an_invalid_enable_flag() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_SERVER__TLS__ENABLED", "maybe");
    let mut c = Config::default();
    let result = c.apply_env();
    std::env::remove_var("MW_SERVER__TLS__ENABLED");
    assert!(result.is_err());
}

#[test]
fn tls_reload_interval_is_validated_in_toml_and_env() {
    let c = Config::from_toml_str(
        r#"
        [server.tls]
        cert_path = "/etc/microwave/tls.crt"
        key_path = "/etc/microwave/tls.key"
        reload_interval_seconds = 60
    "#,
    )
    .unwrap();
    assert_eq!(c.server.tls.reload_interval_seconds, 60);

    let error = Config::from_toml_str(
        r#"
        [server.tls]
        cert_path = "/etc/microwave/tls.crt"
        key_path = "/etc/microwave/tls.key"
        reload_interval_seconds = 604801
    "#,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("reload_interval_seconds"),
        "unexpected error: {error}"
    );

    let error = Config::from_toml_str(
        r#"
        [server.tls]
        reload_interval_seconds = 30
    "#,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("requires TLS to be enabled"),
        "unexpected error: {error}"
    );

    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_SERVER__TLS__CERT_PATH", "/env/tls.crt");
    std::env::set_var("MW_SERVER__TLS__KEY_PATH", "/env/tls.key");
    std::env::set_var("MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS", "30");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.server.tls.reload_interval_seconds, 30);

    std::env::set_var("MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS", "not-a-number");
    let mut c = Config::default();
    let result = c.apply_env();

    std::env::remove_var("MW_SERVER__TLS__CERT_PATH");
    std::env::remove_var("MW_SERVER__TLS__KEY_PATH");
    std::env::remove_var("MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS");
    assert!(result.is_err(), "a non-numeric interval must be rejected");
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

#[test]
fn toml_configures_compliance_proxy_and_action_notifications() {
    let c = Config::from_toml_str(
        r#"
        [data_collector]
        root_url = "https://automate.example.com/data-collector/v0/"
        actions_enabled = false
        service_hostname = "chef.example.com"

        [compliance_proxy]
        url = "https://automate.example.com"
        token = "service-token"
        token_header = "x-automate-token"
        timeout_ms = 2500
        insecure_skip_tls_verify = true
    "#,
    )
    .unwrap();

    assert!(c.data_collector.is_configured());
    assert!(!c.data_collector.actions_enabled());
    assert_eq!(
        c.data_collector.service_hostname.as_deref(),
        Some("chef.example.com")
    );
    assert!(c.compliance_proxy.is_configured());
    assert_eq!(
        c.compliance_proxy.url.as_deref(),
        Some("https://automate.example.com")
    );
    assert_eq!(c.compliance_proxy.token.as_deref(), Some("service-token"));
    assert_eq!(
        c.compliance_proxy.token_header.as_deref(),
        Some("x-automate-token")
    );
    assert_eq!(c.compliance_proxy.timeout, 2500);
    assert!(c.compliance_proxy.insecure_skip_tls_verify);
}

#[test]
fn env_overrides_compliance_proxy_and_action_notifications() {
    let _guard = ENV_LOCK.lock().unwrap();
    for (key, value) in [
        (
            "MW_DATA_COLLECTOR__ROOT_URL",
            "https://automate.example.com/data-collector/v0/",
        ),
        ("MW_DATA_COLLECTOR__ACTIONS_ENABLED", "false"),
        ("MW_DATA_COLLECTOR__SERVICE_HOSTNAME", "chef.example.com"),
        ("MW_COMPLIANCE_PROXY__URL", "https://automate.example.com"),
        ("MW_COMPLIANCE_PROXY__TOKEN", "env-service-token"),
        ("MW_COMPLIANCE_PROXY__TOKEN_HEADER", "x-automate-token"),
        ("MW_COMPLIANCE_PROXY__TIMEOUT", "1500"),
        ("MW_COMPLIANCE_PROXY__INSECURE_SKIP_TLS_VERIFY", "true"),
    ] {
        std::env::set_var(key, value);
    }

    let mut c = Config::default();
    c.apply_env().unwrap();

    for key in [
        "MW_DATA_COLLECTOR__ROOT_URL",
        "MW_DATA_COLLECTOR__ACTIONS_ENABLED",
        "MW_DATA_COLLECTOR__SERVICE_HOSTNAME",
        "MW_COMPLIANCE_PROXY__URL",
        "MW_COMPLIANCE_PROXY__TOKEN",
        "MW_COMPLIANCE_PROXY__TOKEN_HEADER",
        "MW_COMPLIANCE_PROXY__TIMEOUT",
        "MW_COMPLIANCE_PROXY__INSECURE_SKIP_TLS_VERIFY",
    ] {
        std::env::remove_var(key);
    }

    assert!(!c.data_collector.actions_enabled());
    assert_eq!(
        c.data_collector.service_hostname.as_deref(),
        Some("chef.example.com")
    );
    assert_eq!(
        c.compliance_proxy.url.as_deref(),
        Some("https://automate.example.com")
    );
    assert_eq!(
        c.compliance_proxy.token.as_deref(),
        Some("env-service-token")
    );
    assert_eq!(
        c.compliance_proxy.token_header.as_deref(),
        Some("x-automate-token")
    );
    assert_eq!(c.compliance_proxy.timeout, 1500);
    assert!(c.compliance_proxy.insecure_skip_tls_verify);
}

#[test]
fn rejects_zero_forwarding_timeouts() {
    assert!(Config::from_toml_str(
        r#"
        [data_collector]
        timeout = 0
    "#
    )
    .is_err());
    assert!(Config::from_toml_str(
        r#"
        [compliance_proxy]
        url = "https://automate.example.com"
        timeout = 0
    "#
    )
    .is_err());
}

#[test]
fn root_url_alone_does_not_enable_action_notifications() {
    let c = Config::from_toml_str(
        r#"
        [data_collector]
        root_url = "https://automate.example.com/data-collector/v0/"
    "#,
    )
    .unwrap();
    assert!(c.data_collector.is_configured());
    assert!(!c.data_collector.actions_enabled());

    let enabled = Config::from_toml_str(
        r#"
        [data_collector]
        root_url = "https://automate.example.com/data-collector/v0/"
        actions_enabled = true
    "#,
    )
    .unwrap();
    assert!(enabled.data_collector.actions_enabled());
}

#[test]
fn rejects_invalid_compliance_proxy_urls_and_headers() {
    for url in ["not a url", "ftp://automate.example.com", "https://"] {
        let input = format!(
            r#"
            [compliance_proxy]
            url = "{url}"
            "#
        );
        assert!(
            Config::from_toml_str(&input).is_err(),
            "accepted invalid URL {url}"
        );
    }

    for header in [
        "Host",
        "Content-Type",
        "Content-Length",
        "Connection",
        "Transfer-Encoding",
        "X-Forwarded-For",
        "X-Forwarded-Host",
        "X-Real-IP",
        "Forwarded",
        "X-Data-Collector-Auth",
        "not a header",
    ] {
        let input = format!(
            r#"
            [compliance_proxy]
            url = "https://automate.example.com"
            token_header = "{header}"
            "#
        );
        assert!(
            Config::from_toml_str(&input).is_err(),
            "accepted dangerous token header {header}"
        );
    }
}

#[test]
fn mail_defaults_are_disabled_and_safe() {
    let c = Config::default();
    assert!(!c.mail.enabled);
    assert!(!c.mail.is_enabled());
    assert_eq!(c.mail.security, MailSecurity::Starttls);
    assert_eq!(c.mail.auth, MailAuth::Auto);
    assert_eq!(c.mail.timeout_ms, 10_000);
    assert_eq!(c.mail.port, None);
    assert_eq!(c.server.public_url, None);
    assert_eq!(c.auth.password_reset_ttl_seconds, 3600);
}

#[test]
fn mail_toml_round_trips_remote_auth_fields() {
    let c = Config::from_toml_str(
        r#"
        [server]
        public_url = "https://chef.example.com/"
        [mail]
        enabled = true
        host = "smtp.example.com"
        port = 587
        security = "starttls"
        username = "mailer"
        password = "secret"
        auth = "login"
        from = "Chef <chef@example.com>"
        reply_to = "ops@example.com"
        timeout_ms = 5000
        hello_name = "chef.example.com"
        tls_ca_path = "/etc/ssl/private-ca.pem"
        [auth]
        password_reset_ttl_seconds = 900
    "#,
    )
    .unwrap();
    assert!(c.mail.is_enabled());
    assert_eq!(c.mail.security, MailSecurity::Starttls);
    assert_eq!(c.mail.auth, MailAuth::Login);
    assert_eq!(
        c.server.public_url.as_deref(),
        Some("https://chef.example.com/")
    );
    assert_eq!(c.auth.password_reset_ttl_seconds, 900);
}

#[test]
fn mail_is_enabled_requires_a_non_empty_host() {
    let mut mail = MailConfig::default();
    assert!(!mail.is_enabled());

    mail.enabled = true;
    assert!(!mail.is_enabled(), "enabled without a host is not active");

    mail.host = Some("   ".into());
    assert!(!mail.is_enabled(), "whitespace-only host is not active");

    mail.host = Some("smtp.example.com".into());
    assert!(mail.is_enabled());
}

/// Build an enabled mail configuration with the mandatory host/from fields and
/// an optional public URL so each test only varies the rule under test.
fn mail_toml_with_auth(public_url: Option<&str>, auth: Option<&str>, extra: &str) -> String {
    let server = match public_url {
        Some(url) => format!("[server]\npublic_url = \"{url}\"\n"),
        None => String::new(),
    };
    let auth = auth
        .map(|auth| format!("auth = \"{auth}\"\n"))
        .unwrap_or_default();
    format!(
        r#"
        {server}
        [mail]
        enabled = true
        host = "smtp.example.com"
        from = "Chef <chef@example.com>"
        {auth}{extra}
        "#
    )
}

/// Enabled mail without authentication. Unauthenticated relays must opt in
/// with `auth = "none"`; the default `auto` requires credentials.
fn mail_toml(public_url: Option<&str>, extra: &str) -> String {
    mail_toml_with_auth(public_url, Some("none"), extra)
}

#[test]
fn mail_requires_host_from_timeout_and_public_url_when_enabled() {
    for (label, input) in [
        (
            "missing host",
            mail_toml(Some("https://chef.example.com"), "")
                .replace("host = \"smtp.example.com\"", ""),
        ),
        (
            "missing from",
            mail_toml(Some("https://chef.example.com"), "")
                .replace("from = \"Chef <chef@example.com>\"", ""),
        ),
        ("missing public_url", mail_toml(None, "")),
        (
            "zero timeout",
            mail_toml(Some("https://chef.example.com"), "timeout_ms = 0"),
        ),
    ] {
        assert!(
            Config::from_toml_str(&input).is_err(),
            "accepted enabled mail with {label}"
        );
    }
}

#[test]
fn mail_rejects_incomplete_or_conflicting_credentials() {
    let public_url = Some("https://chef.example.com");
    for (label, auth, extra, expected) in [
        (
            "username without password",
            "plain",
            "username = \"mailer\"",
            "mail.username requires mail.password",
        ),
        (
            "password without username",
            "plain",
            "password = \"secret\"",
            "mail.password requires mail.username",
        ),
        (
            "credentials with auth = none",
            "none",
            "username = \"mailer\"\npassword = \"secret\"",
            "must not be combined with credentials",
        ),
    ] {
        let error =
            Config::from_toml_str(&mail_toml_with_auth(public_url, Some(auth), extra)).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "{label} failed through the wrong validation rule: {error}"
        );
    }

    // Every AUTH mechanism other than `none` requires complete credentials.
    for auth in ["auto", "plain", "login"] {
        let error =
            Config::from_toml_str(&mail_toml_with_auth(public_url, Some(auth), "")).unwrap_err();
        assert!(
            error.to_string().contains("requires mail.username"),
            "auth = {auth} without credentials must fail with a credential error: {error}"
        );
    }

    // Complete credentials are accepted with the default (auto) and explicit
    // login mechanisms; an unauthenticated relay needs an explicit `none`.
    for auth in [None, Some("login")] {
        assert!(
            Config::from_toml_str(&mail_toml_with_auth(
                public_url,
                auth,
                "username = \"mailer\"\npassword = \"secret\""
            ))
            .is_ok(),
            "complete credentials with auth {auth:?} must be accepted"
        );
    }
    assert!(
        Config::from_toml_str(&mail_toml(public_url, "")).is_ok(),
        "auth = \"none\" without credentials must be accepted"
    );
}

#[test]
fn mail_rejects_plaintext_auth_to_a_remote_host_but_allows_loopback() {
    let public_url = Some("https://chef.example.com");
    let credentials = "username = \"mailer\"\npassword = \"secret\"";

    assert!(
        Config::from_toml_str(&mail_toml_with_auth(
            public_url,
            None,
            &format!("security = \"none\"\n{credentials}")
        ))
        .is_err(),
        "plaintext AUTH to a remote relay must be rejected"
    );

    for host in ["127.0.0.1", "localhost", "[::1]", "::1"] {
        let input = mail_toml_with_auth(
            public_url,
            None,
            &format!("security = \"none\"\n{credentials}"),
        )
        .replace("host = \"smtp.example.com\"", &format!("host = \"{host}\""));
        assert!(
            Config::from_toml_str(&input).is_ok(),
            "plaintext AUTH to loopback host {host} must be accepted"
        );
    }
}

#[test]
fn mail_rejects_port_zero() {
    assert!(
        Config::from_toml_str(&mail_toml(Some("https://chef.example.com"), "port = 0")).is_err()
    );
}

#[test]
fn public_url_must_be_a_secure_origin_without_path_query_or_fragment() {
    for url in [
        "http://example.com",
        "ftp://chef.example.com",
        "not a url",
        "https://chef.example.com/chef",
        "https://chef.example.com/?a=1",
        "https://chef.example.com/#frag",
        "https://user:pass@chef.example.com",
        "https://",
    ] {
        assert!(
            Config::from_toml_str(&mail_toml(Some(url), "")).is_err(),
            "accepted invalid public URL {url}"
        );
    }

    for url in [
        "https://chef.example.com",
        "https://chef.example.com/",
        "http://localhost:8000",
        "http://127.0.0.1:8000",
        "http://[::1]:8000",
    ] {
        assert!(
            Config::from_toml_str(&mail_toml(Some(url), "")).is_ok(),
            "rejected valid public URL {url}"
        );
    }
}

#[test]
fn public_url_accepts_surrounding_whitespace_without_echoing_it_as_a_different_value() {
    let input = mail_toml(Some("  https://chef.example.com/  "), "");
    let config = Config::from_toml_str(&input).unwrap();
    assert_eq!(
        config.server.public_url.as_deref().map(str::trim),
        Some("https://chef.example.com/")
    );
}

#[test]
fn public_url_validation_does_not_echo_embedded_credentials() {
    let error = Config::from_toml_str(
        r#"
        [server]
        public_url = "https://user:super-secret-password@chef.example.com"
        "#,
    )
    .unwrap_err();

    let display = error.to_string();
    let debug = format!("{error:?}");
    assert!(display.contains("server.public_url"));
    assert!(display.contains("must not include credentials"));
    assert!(!display.contains("super-secret-password"));
    assert!(!debug.contains("super-secret-password"));
}

#[test]
fn public_url_is_validated_even_when_mail_is_disabled() {
    assert!(Config::from_toml_str(
        r#"
        [server]
        public_url = "http://example.com"
    "#
    )
    .is_err());
}

#[test]
fn password_reset_ttl_is_bounded() {
    for ttl in ["0", "604801"] {
        let input = format!(
            r#"
            [auth]
            password_reset_ttl_seconds = {ttl}
        "#
        );
        assert!(
            Config::from_toml_str(&input).is_err(),
            "accepted out-of-range TTL {ttl}"
        );
    }

    for ttl in ["1", "604800"] {
        let input = format!(
            r#"
            [auth]
            password_reset_ttl_seconds = {ttl}
        "#
        );
        assert!(
            Config::from_toml_str(&input).is_ok(),
            "rejected valid TTL {ttl}"
        );
    }
}

#[test]
fn mail_and_public_url_can_be_overridden_from_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    let keys = [
        "MW_SERVER__PUBLIC_URL",
        "MW_MAIL__ENABLED",
        "MW_MAIL__HOST",
        "MW_MAIL__PORT",
        "MW_MAIL__SECURITY",
        "MW_MAIL__USERNAME",
        "MW_MAIL__PASSWORD",
        "MW_MAIL__AUTH",
        "MW_MAIL__FROM",
        "MW_MAIL__REPLY_TO",
        "MW_MAIL__TIMEOUT_MS",
        "MW_MAIL__HELLO_NAME",
        "MW_MAIL__TLS_CA_PATH",
        "MW_AUTH__PASSWORD_RESET_TTL_SECONDS",
    ];
    for (key, value) in [
        ("MW_SERVER__PUBLIC_URL", "https://chef.example.com/"),
        ("MW_MAIL__ENABLED", "true"),
        ("MW_MAIL__HOST", "smtp.example.com"),
        ("MW_MAIL__PORT", "2525"),
        ("MW_MAIL__SECURITY", "tls"),
        ("MW_MAIL__USERNAME", "mailer"),
        ("MW_MAIL__PASSWORD", "secret"),
        ("MW_MAIL__AUTH", "plain"),
        ("MW_MAIL__FROM", "Chef <chef@example.com>"),
        ("MW_MAIL__REPLY_TO", "ops@example.com"),
        ("MW_MAIL__TIMEOUT_MS", "2500"),
        ("MW_MAIL__HELLO_NAME", "chef.example.com"),
        ("MW_MAIL__TLS_CA_PATH", "/etc/ssl/private-ca.pem"),
        ("MW_AUTH__PASSWORD_RESET_TTL_SECONDS", "1200"),
    ] {
        std::env::set_var(key, value);
    }

    let mut c = Config::default();
    let result = c.apply_env();

    for key in keys {
        std::env::remove_var(key);
    }
    result.unwrap();

    assert!(c.mail.is_enabled());
    assert_eq!(c.mail.host.as_deref(), Some("smtp.example.com"));
    assert_eq!(c.mail.port, Some(2525));
    assert_eq!(c.mail.security, MailSecurity::Tls);
    assert_eq!(c.mail.username.as_deref(), Some("mailer"));
    assert_eq!(c.mail.password.as_deref(), Some("secret"));
    assert_eq!(c.mail.auth, MailAuth::Plain);
    assert_eq!(c.mail.from.as_deref(), Some("Chef <chef@example.com>"));
    assert_eq!(c.mail.reply_to.as_deref(), Some("ops@example.com"));
    assert_eq!(c.mail.timeout_ms, 2500);
    assert_eq!(c.mail.hello_name.as_deref(), Some("chef.example.com"));
    assert_eq!(
        c.mail.tls_ca_path.as_deref(),
        Some("/etc/ssl/private-ca.pem")
    );
    assert_eq!(
        c.server.public_url.as_deref(),
        Some("https://chef.example.com/")
    );
    assert_eq!(c.auth.password_reset_ttl_seconds, 1200);
}

#[test]
fn mail_env_rejects_invalid_enum_port_and_ttl_values() {
    let _guard = ENV_LOCK.lock().unwrap();

    std::env::set_var("MW_MAIL__SECURITY", "ssl");
    let mut c = Config::default();
    assert!(c.apply_env().is_err(), "unknown security must be rejected");
    std::env::remove_var("MW_MAIL__SECURITY");

    std::env::set_var("MW_MAIL__AUTH", "oauth");
    let mut c = Config::default();
    assert!(c.apply_env().is_err(), "unknown auth must be rejected");
    std::env::remove_var("MW_MAIL__AUTH");

    std::env::set_var("MW_MAIL__PORT", "not-a-number");
    let mut c = Config::default();
    assert!(c.apply_env().is_err(), "non-numeric port must be rejected");
    std::env::remove_var("MW_MAIL__PORT");

    std::env::set_var("MW_AUTH__PASSWORD_RESET_TTL_SECONDS", "0");
    let mut c = Config::default();
    assert!(c.apply_env().is_err(), "zero TTL must be rejected");
    std::env::remove_var("MW_AUTH__PASSWORD_RESET_TTL_SECONDS");
}

#[test]
fn mail_password_is_redacted_from_debug_and_serialization() {
    let mut c = Config::default();
    c.mail.password = Some("super-secret".into());
    assert!(!format!("{:?}", c.mail).contains("super-secret"));
    assert!(!toml::to_string(&c.mail).unwrap().contains("super-secret"));
}

#[test]
fn data_collector_defaults_to_forward_with_50_runs() {
    let c = Config::default();
    assert_eq!(c.data_collector.store, DataCollectorStore::Forward);
    assert_eq!(c.data_collector.keep_runs, 50);
    assert!(!c.data_collector.is_enabled());
    assert!(c.data_collector.forwards());
    assert!(!c.data_collector.stores_locally());
}

#[test]
fn data_collector_local_enables_the_surface_without_forwarding() {
    let c = Config::from_toml_str(
        r#"
        [data_collector]
        store = "local"
        keep_runs = 5
    "#,
    )
    .unwrap();
    assert_eq!(c.data_collector.store, DataCollectorStore::Local);
    assert_eq!(c.data_collector.keep_runs, 5);
    assert!(!c.data_collector.is_configured());
    assert!(c.data_collector.is_enabled());
    assert!(c.data_collector.stores_locally());
    assert!(!c.data_collector.forwards());
}

#[test]
fn data_collector_both_requires_a_root_url() {
    assert!(Config::from_toml_str(
        r#"
        [data_collector]
        store = "both"
    "#
    )
    .is_err());

    let c = Config::from_toml_str(
        r#"
        [data_collector]
        store = "both"
        root_url = "http://127.0.0.1:1/data-collector/v0/"
    "#,
    )
    .unwrap();
    assert!(c.data_collector.is_enabled());
    assert!(c.data_collector.forwards());
    assert!(c.data_collector.stores_locally());
}

#[test]
fn data_collector_rejects_unknown_modes_and_negative_retention() {
    assert!(Config::from_toml_str(
        r#"
        [data_collector]
        store = "sometimes"
    "#
    )
    .is_err());
    assert!(Config::from_toml_str(
        r#"
        [data_collector]
        keep_runs = -1
    "#
    )
    .is_err());
}

#[test]
fn data_collector_local_suppresses_action_notifications() {
    let c = Config::from_toml_str(
        r#"
        [data_collector]
        store = "local"
        root_url = "http://127.0.0.1:1/data-collector/v0/"
        actions_enabled = true
    "#,
    )
    .unwrap();
    assert!(!c.data_collector.actions_enabled());
}

#[test]
fn data_collector_store_and_retention_can_come_from_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_DATA_COLLECTOR__STORE", "local");
    std::env::set_var("MW_DATA_COLLECTOR__KEEP_RUNS", "7");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.data_collector.store, DataCollectorStore::Local);
    assert_eq!(c.data_collector.keep_runs, 7);

    std::env::set_var("MW_DATA_COLLECTOR__STORE", "nope");
    let mut c = Config::default();
    assert!(c.apply_env().is_err());

    std::env::remove_var("MW_DATA_COLLECTOR__STORE");
    std::env::remove_var("MW_DATA_COLLECTOR__KEEP_RUNS");
}

#[test]
fn mcp_defaults_are_disabled_with_bounded_limits() {
    let c = Config::default();
    assert_eq!(McpConfig::default(), c.mcp);
    assert!(McpOrgConfig::default().writes);
    assert!(!c.mcp.enabled);
    assert_eq!(c.mcp.max_results, 100);
    assert_eq!(c.mcp.max_result_bytes, 1_048_576);
    assert_eq!(c.mcp.max_upload_bytes, 10_485_760);
    assert!(c.mcp.allowed_hosts.is_empty());
    assert!(c.mcp.allowed_origins.is_empty());
    assert!(c.mcp.organizations.is_empty());
}

#[test]
fn mcp_parses_per_org_tokens_principals_and_writes() {
    let c = Config::from_toml_str(
        r#"
        [mcp]
        enabled = true
        [mcp.organizations.acme]
        token = "a-token"
        principal = "mcp-agent"
        writes = false
        [mcp.organizations.beta]
    "#,
    )
    .unwrap();
    assert!(c.mcp.enabled);
    let acme = c.mcp.organizations.get("acme").unwrap();
    assert_eq!(acme.token.as_deref(), Some("a-token"));
    assert_eq!(acme.principal.as_deref(), Some("mcp-agent"));
    assert!(!acme.writes);
    assert!(c.mcp.organizations.get("beta").unwrap().writes);
}

#[test]
fn mcp_clamps_limits() {
    let c = Config::from_toml_str(
        r#"
        [mcp]
        max_results = 5000
        max_result_bytes = 1
        max_upload_bytes = 999999999999
    "#,
    )
    .unwrap();
    assert_eq!(c.mcp.max_results, 1000);
    assert_eq!(c.mcp.max_result_bytes, 1024);
    assert_eq!(c.mcp.max_upload_bytes, 250 * 1024 * 1024);
}

#[test]
fn mcp_allowed_hosts_fall_back_to_the_public_url() {
    let c = Config::from_toml_str(
        r#"
        [server]
        public_url = "https://chef.example.com"
        [mcp]
        enabled = true
    "#,
    )
    .unwrap();
    assert_eq!(
        c.mcp
            .effective_allowed_hosts(c.server.public_url.as_deref()),
        vec!["chef.example.com".to_string()]
    );

    let c = Config::from_toml_str(
        r#"
        [server]
        public_url = "https://chef.example.com:8443"
    "#,
    )
    .unwrap();
    assert_eq!(
        c.mcp
            .effective_allowed_hosts(c.server.public_url.as_deref()),
        vec![
            "chef.example.com".to_string(),
            "chef.example.com:8443".to_string()
        ]
    );
}

#[test]
fn mcp_scalar_settings_can_come_from_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_MCP__ENABLED", "true");
    std::env::set_var("MW_MCP__MAX_RESULTS", "7");
    std::env::set_var("MW_MCP__MAX_RESULT_BYTES", "2048");
    std::env::set_var("MW_MCP__MAX_UPLOAD_BYTES", "4096");
    std::env::set_var("MW_MCP__ALLOWED_HOSTS", "chef.example.com, localhost");
    std::env::set_var("MW_MCP__ALLOWED_ORIGINS", "https://app.example.com");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert!(c.mcp.enabled);
    assert_eq!(c.mcp.max_results, 7);
    assert_eq!(c.mcp.max_result_bytes, 2048);
    assert_eq!(c.mcp.max_upload_bytes, 4096);
    assert_eq!(
        c.mcp.allowed_hosts,
        vec!["chef.example.com".to_string(), "localhost".to_string()]
    );
    assert_eq!(
        c.mcp.allowed_origins,
        vec!["https://app.example.com".to_string()]
    );
    for key in [
        "MW_MCP__ENABLED",
        "MW_MCP__MAX_RESULTS",
        "MW_MCP__MAX_RESULT_BYTES",
        "MW_MCP__MAX_UPLOAD_BYTES",
        "MW_MCP__ALLOWED_HOSTS",
        "MW_MCP__ALLOWED_ORIGINS",
    ] {
        std::env::remove_var(key);
    }
}

#[test]
fn mcp_environment_limits_are_clamped() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_MCP__MAX_RESULTS", "5000");
    std::env::set_var("MW_MCP__MAX_RESULT_BYTES", "1");
    std::env::set_var("MW_MCP__MAX_UPLOAD_BYTES", "999999999999");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.mcp.max_results, 1000);
    assert_eq!(c.mcp.max_result_bytes, 1024);
    assert_eq!(c.mcp.max_upload_bytes, 250 * 1024 * 1024);
    for key in [
        "MW_MCP__MAX_RESULTS",
        "MW_MCP__MAX_RESULT_BYTES",
        "MW_MCP__MAX_UPLOAD_BYTES",
    ] {
        std::env::remove_var(key);
    }
}

#[test]
fn mcp_fallback_hosts_bracket_ipv6_ports() {
    let mcp = McpConfig::default();
    assert_eq!(
        mcp.effective_allowed_hosts(Some("https://[2001:db8::1]:8443")),
        vec!["2001:db8::1".to_string(), "[2001:db8::1]:8443".to_string()]
    );
}

#[test]
fn mcp_organization_keys_must_be_valid_org_names() {
    let mut config = Config::default();
    config
        .mcp
        .organizations
        .insert("team-1".to_string(), McpOrgConfig::default());
    config.validate().expect("valid org name");

    config
        .mcp
        .organizations
        .insert("BadOrg".to_string(), McpOrgConfig::default());
    let error = config.validate().expect_err("invalid org name");
    assert!(matches!(
        error,
        microwave_config::ConfigError::InvalidValue { ref key, ref value }
            if key == "mcp.organizations.BadOrg" && value == "BadOrg"
    ));
}

#[test]
fn shipped_example_config_parses_with_safe_defaults() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/microwave.toml.example");
    let input = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let c = Config::from_toml_str(&input)
        .unwrap_or_else(|error| panic!("packaging example must parse: {error}"));

    assert!(!c.mcp.enabled);
    assert_eq!(c.mcp.max_results, 100);
    assert_eq!(c.mcp.max_result_bytes, 1_048_576);
    assert_eq!(c.mcp.max_upload_bytes, 10_485_760);
    assert!(c.mcp.organizations.is_empty());
    assert_eq!(c.data_collector.store, DataCollectorStore::Forward);
    assert_eq!(c.data_collector.keep_runs, 50);
}
