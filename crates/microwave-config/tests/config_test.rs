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
fn sqlite_url_is_derived_from_data_dir() {
    let mut c = Config::default();
    c.paths.data_dir = "/tmp/mw".into();
    assert_eq!(c.resolved_database_url(), "sqlite:///tmp/mw/microwave.db");
}
