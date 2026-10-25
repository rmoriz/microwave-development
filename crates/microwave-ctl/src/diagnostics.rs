use crate::{context::Context, CtlError};
use microwave_config::{DatabaseDriver, ObjectStoreBackend, SearchBackend};
use microwave_store::SqliteStore;
use std::net::SocketAddr;

/// Load the resolved configuration and validate the values required to serve
/// requests. External backends are checked for the credentials they need.
pub fn check_config(context: &Context) -> Result<(), CtlError> {
    let config = &context.config;
    let mut problems = Vec::new();
    let mut checks = Vec::new();

    if config.paths.data_dir.trim().is_empty() {
        problems.push("paths.data_dir must not be empty".to_string());
    } else if context.data_dir.exists() && !context.data_dir.is_dir() {
        problems.push(format!(
            "paths.data_dir {} is not a directory",
            context.data_dir.display()
        ));
    } else {
        checks.push(format!("data_dir {}", context.data_dir.display()));
    }

    match config.database.driver {
        DatabaseDriver::Sqlite => match context.database_path.as_deref() {
            Some(path) if !path.as_os_str().is_empty() => {
                if path.exists() && !path.is_file() {
                    problems.push(format!("database path {} is not a file", path.display()));
                } else {
                    checks.push(format!("database sqlite {}", path.display()));
                }
            }
            _ => problems.push("database.url must be sqlite://<path>".to_string()),
        },
        DatabaseDriver::Postgres => {
            if context.database_url.starts_with("postgres://")
                || context.database_url.starts_with("postgresql://")
            {
                checks.push("database postgresql".to_string());
            } else {
                problems
                    .push("database.url must be a postgres:// or postgresql:// URL".to_string());
            }
        }
    }

    match config.server.bind.parse::<SocketAddr>() {
        Ok(bind) => checks.push(format!("server.bind {bind}")),
        Err(_) => problems.push(format!(
            "server.bind {:?} is not a valid socket address",
            config.server.bind
        )),
    }

    match config.search.backend {
        SearchBackend::Embedded => checks.push("search embedded".to_string()),
        backend => match non_empty(config.search.url.as_deref()) {
            Some(url) => checks.push(format!("search {} {url}", search_backend_name(backend))),
            None => problems.push(format!(
                "search.url is required for the {} backend",
                search_backend_name(backend)
            )),
        },
    }

    match config.object_store.backend {
        ObjectStoreBackend::Local => checks.push("object_store local".to_string()),
        backend => {
            let name = object_store_backend_name(backend);
            if non_empty(config.object_store.endpoint.as_deref()).is_none() {
                problems.push(format!(
                    "object_store.endpoint is required for the {name} backend"
                ));
            }
            if non_empty(config.object_store.bucket.as_deref()).is_none() {
                problems.push(format!(
                    "object_store.bucket is required for the {name} backend"
                ));
            }
            let access_key = non_empty(config.object_store.access_key.as_deref())
                .map(str::to_string)
                .or_else(|| stored_secret(context, "bookshelf", "access_key_id"));
            if access_key.is_none() {
                problems.push(format!(
                    "object_store.access_key (or the bookshelf.access_key_id secret) is required for the {name} backend"
                ));
            }
            let secret_key = non_empty(config.object_store.secret_key.as_deref())
                .map(str::to_string)
                .or_else(|| stored_secret(context, "bookshelf", "secret_access_key"));
            if secret_key.is_none() {
                problems.push(format!(
                    "object_store.secret_key (or the bookshelf.secret_access_key secret) is required for the {name} backend"
                ));
            }
            if non_empty(config.object_store.endpoint.as_deref()).is_some()
                && non_empty(config.object_store.bucket.as_deref()).is_some()
                && access_key.is_some()
                && secret_key.is_some()
            {
                checks.push(format!("object_store {name}"));
            }
        }
    }

    if config.auth.superuser_name.trim().is_empty() {
        problems.push("auth.superuser_name must not be empty".to_string());
    } else {
        checks.push(format!(
            "auth.superuser_name {}",
            config.auth.superuser_name
        ));
    }

    if let Some(root_url) = non_empty(config.data_collector.root_url.as_deref()) {
        if !is_http_url(root_url) {
            problems.push(format!(
                "data_collector.root_url must be an http:// or https:// URL with a host, got {root_url}"
            ));
        } else {
            checks.push(format!("data_collector.root_url {root_url}"));
        }
    }
    if config.data_collector.timeout == 0 {
        problems.push("data_collector.timeout must be greater than 0".to_string());
    } else {
        checks.push(format!(
            "data_collector.timeout {}ms",
            config.data_collector.timeout
        ));
    }
    if config.required_recipe.enabled {
        match non_empty(config.required_recipe.path.as_deref()) {
            Some(path) => checks.push(format!("required_recipe.path {path}")),
            None => problems.push(
                "required_recipe.path must be set when required_recipe.enabled is true".to_string(),
            ),
        }
    }

    if problems.is_empty() {
        for check in checks {
            println!("ok: {check}");
        }
        Ok(())
    } else {
        Err(CtlError::message(format!(
            "configuration check failed:\n  - {}",
            problems.join("\n  - ")
        )))
    }
}

/// Report the high-availability state. Chef Server's DRBD/keepalived HA was
/// removed upstream; Microwave is single-node, so this is an honest report
/// rather than an attempt to query services that do not exist.
pub fn ha_status(context: &Context) -> Result<(), CtlError> {
    let config = &context.config;
    println!("mode: standalone");
    println!("nodes: 1");
    println!("high_availability: not configured");
    println!("database: {}", database_driver_name(config.database.driver));
    println!("search: {}", search_backend_name(config.search.backend));
    println!(
        "object_store: {}",
        object_store_backend_name(config.object_store.backend)
    );
    println!(
        "note: multi-node and DRBD/keepalived HA are not implemented; this report describes a single node only."
    );
    Ok(())
}

/// Open an interactive `sqlite3` shell on the configured database, or `psql`
/// when a PostgreSQL URL is configured. Extra arguments are passed through.
pub fn psql(context: &Context, args: &[String]) -> Result<(), CtlError> {
    let (program, mut command_args) = match context.config.database.driver {
        DatabaseDriver::Sqlite => {
            let path = context.sqlite_path()?.to_path_buf();
            // Ensure the file exists and has the current schema before opening it.
            drop(context.open_store()?);
            ("sqlite3", vec![path.display().to_string()])
        }
        DatabaseDriver::Postgres => ("psql", vec![context.database_url.clone()]),
    };

    command_args.extend(args.iter().cloned());
    let status = std::process::Command::new(program)
        .args(&command_args)
        .status()
        .map_err(|error| CtlError::message(format!("failed to run {program}: {error}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(CtlError::message(format!("{program} exited with {status}")))
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn is_http_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    else {
        return false;
    };
    !rest.is_empty()
        && !rest.starts_with('/')
        && !rest.chars().any(char::is_whitespace)
        && !rest.starts_with(':')
}

fn stored_secret(context: &Context, group: &str, name: &str) -> Option<String> {
    let path = context.database_path.as_deref()?;
    if !path.is_file() {
        return None;
    }
    let store = SqliteStore::open_read_only(path).ok()?;
    store
        .get_secret(group, name)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

fn database_driver_name(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Sqlite => "sqlite",
        DatabaseDriver::Postgres => "postgresql",
    }
}

fn search_backend_name(backend: SearchBackend) -> &'static str {
    match backend {
        SearchBackend::Embedded => "embedded",
        SearchBackend::OpenSearch => "opensearch",
        SearchBackend::Elasticsearch => "elasticsearch",
    }
}

fn object_store_backend_name(backend: ObjectStoreBackend) -> &'static str {
    match backend {
        ObjectStoreBackend::Local => "local",
        ObjectStoreBackend::S3 => "s3",
        ObjectStoreBackend::Garage => "garage",
    }
}
