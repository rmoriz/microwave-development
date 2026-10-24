use microwave_config::{Config, DatabaseDriver, ObjectStoreBackend, SearchBackend};

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
