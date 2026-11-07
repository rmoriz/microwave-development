use crate::{context::Context, CtlError};
use microwave_config::{
    tenant_issues, valid_bucket_name, Config, DatabaseDriver, ObjectStoreBackend,
    ObjectStoreConfig, ObjectStoreOrgConfig, OrgCredentials, SearchBackend,
};
use microwave_mail::SmtpMailer;
use microwave_objectstore::{probe_endpoint, S3Config};
use microwave_store::{SqliteStore, StoreError, OBJECT_STORE_SECRET_GROUP};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use url::Url;

const MAX_REQUIRED_RECIPE_BYTES: u64 = 1_048_576;

/// Load the resolved configuration and validate the values required to serve
/// requests. External backends are checked for the credentials they need.
pub fn check_config(context: &Context) -> Result<(), CtlError> {
    let (problems, checks, _) = collect_config(context);
    finish(problems, checks)
}

/// Print the passing checks and turn the problems into one aggregate error.
fn finish(problems: Vec<String>, checks: Vec<String>) -> Result<(), CtlError> {
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

/// Collect the preflight problems, the checks that passed and the tenant
/// state (`None` for the local backend). Shared by `check-config` and
/// `diagnostics`; neither touches the network.
fn collect_config(
    context: &Context,
) -> (
    Vec<String>,
    Vec<String>,
    Option<Result<TenantState, String>>,
) {
    let config = &context.config;
    let mut problems = Vec::new();
    let mut checks = Vec::new();
    let mut tenant = None;

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

    // Defensive: `Context::load` already runs `Config::validate`, which rejects
    // a zero value. This only fires for configs built without validation; keep
    // it when the load path changes.
    if config.database.pool_size == 0 {
        problems.push("database.pool_size must be greater than zero".to_string());
    }
    match config.database.driver {
        DatabaseDriver::Sqlite => {
            checks.push(format!("database.pool_size {}", config.database.pool_size));
            match context.database_path.as_deref() {
                Some(path) if !path.as_os_str().is_empty() => {
                    if path.exists() && !path.is_file() {
                        problems.push(format!("database path {} is not a file", path.display()));
                    } else {
                        checks.push(format!("database sqlite {}", path.display()));
                    }
                }
                _ => problems.push("database.url must be sqlite://<path>".to_string()),
            }
        }
        DatabaseDriver::Postgres => {
            checks.push(format!("database.pool_size {}", config.database.pool_size));
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

    match resolve_bind(&config.server.bind) {
        Some(bind) => checks.push(format!("server.bind {bind}")),
        None => problems.push(format!(
            "server.bind {:?} is not a valid socket address",
            config.server.bind
        )),
    }

    if config.server.tls.is_enabled() {
        match microwave_tls::validate(&config.server.tls) {
            Ok(()) => checks.push(format!(
                "tls {} {}",
                config.server.tls.cert_path.as_deref().unwrap_or_default(),
                config.server.tls.key_path.as_deref().unwrap_or_default()
            )),
            Err(error) => problems.push(error.to_string()),
        }
    } else {
        checks.push("tls disabled".to_string());
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
            let strict = config.object_store.require_org_bucket;
            if non_empty(config.object_store.endpoint.as_deref()).is_none() {
                problems.push(format!(
                    "object_store.endpoint is required for the {name} backend"
                ));
            }
            // Strict mode has no shared bucket; `Config::validate` already
            // rejects one being configured.
            if !strict && non_empty(config.object_store.bucket.as_deref()).is_none() {
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
                && (strict || non_empty(config.object_store.bucket.as_deref()).is_some())
                && access_key.is_some()
                && secret_key.is_some()
            {
                checks.push(format!("object_store {name}"));
            }
        }
    }

    if config.object_store.backend != ObjectStoreBackend::Local {
        match tenant_state(context, config) {
            Ok(TenantState::Known { orgs, credentials }) => {
                // Defensive: `Config::validate` already rejects both, so this
                // only fires for configs built without validation.
                if !config.object_store.require_org_bucket {
                    if let Some(bucket) = non_empty(config.object_store.bucket.as_deref()) {
                        if !valid_bucket_name(bucket.trim()) {
                            problems.push(format!(
                                "object_store.bucket {bucket:?} is not a valid bucket name"
                            ));
                        }
                    }
                }
                for (organization, entry) in &config.object_store.organizations {
                    if !valid_bucket_name(entry.bucket.trim()) {
                        problems.push(format!(
                            "object_store.organizations.{organization}.bucket {:?} is not a valid bucket name",
                            entry.bucket
                        ));
                    }
                }
                let issues = tenant_issues(&config.object_store, &orgs, &credentials);
                if issues.is_empty() {
                    checks.push(format!(
                        "object_store tenant map {} organization(s)",
                        orgs.len()
                    ));
                } else {
                    for issue in issues {
                        problems.push(issue.to_string());
                    }
                }
                tenant = Some(Ok(TenantState::Known { orgs, credentials }));
            }
            Ok(TenantState::Unknown) => {
                // A fresh install has no organization table yet; the mapped
                // entries cannot be cross-checked, but the config is not wrong.
                checks.push("object_store tenant map (organization table not readable yet)".into());
                tenant = Some(Ok(TenantState::Unknown));
            }
            Err(error) => {
                problems.push(format!(
                    "cannot read organizations and object-store credentials: {error}"
                ));
                tenant = Some(Err(error));
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

    if config.auth.webui_key {
        match config.auth.webui_key_path.as_deref() {
            Some(path) => match validate_webui_key_path(path) {
                Ok(()) => checks.push(format!("auth.webui_key_path {}", path.display())),
                Err(reason) => {
                    problems.push(format!("auth.webui_key_path {}: {reason}", path.display()));
                }
            },
            None => checks.push("auth.webui_key_path default".to_string()),
        }
    }

    match config.keygen.validate() {
        Ok(()) => checks.push(format!(
            "keygen size {} workers {} warm_up {}",
            config.keygen.size, config.keygen.workers, config.keygen.warm_up
        )),
        Err(reason) => problems.push(reason),
    }

    if let Some(root_url) = non_empty(config.data_collector.root_url.as_deref()) {
        match parse_http_url(root_url) {
            Some(url) => checks.push(format!("data_collector.root_url {}", redacted_url(&url))),
            // Never echo the raw URL: it may contain userinfo/passwords.
            None => problems.push(
                "data_collector.root_url must be an http:// or https:// URL with a host"
                    .to_string(),
            ),
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
    if config.data_collector.actions_enabled() {
        checks.push("data_collector.actions enabled".to_string());
    }
    if let Some(url) = non_empty(config.compliance_proxy.url.as_deref()) {
        match parse_http_url(url) {
            Some(url) => checks.push(format!("compliance_proxy.url {}", redacted_url(&url))),
            // Never echo a URL that may contain userinfo/passwords.
            None => problems.push(
                "compliance_proxy.url must be an http:// or https:// URL with a host".to_string(),
            ),
        }
    }
    if config.compliance_proxy.timeout == 0 {
        problems.push("compliance_proxy.timeout must be greater than 0".to_string());
    } else if config.compliance_proxy.is_configured() {
        checks.push(format!(
            "compliance_proxy.timeout {}ms",
            config.compliance_proxy.timeout
        ));
    }
    if config.compliance_proxy.insecure_skip_tls_verify && config.compliance_proxy.is_configured() {
        checks.push("compliance_proxy TLS verification disabled".to_string());
    }
    if config.required_recipe.enabled {
        match non_empty(config.required_recipe.path.as_deref()) {
            Some(path) => match validate_required_recipe_path(Path::new(path)) {
                Ok(()) => checks.push(format!("required_recipe.path {path}")),
                Err(reason) => {
                    problems.push(format!("required_recipe.path {path}: {reason}"));
                }
            },
            None => problems.push(
                "required_recipe.path must be set when required_recipe.enabled is true".to_string(),
            ),
        }
    }

    if config.mail.is_enabled() {
        match SmtpMailer::validate_mail_config(&config.mail) {
            Ok(()) => checks.push("mail smtp".to_string()),
            Err(error) => problems.push(format!("mail configuration: {error}")),
        }
    }

    (problems, checks, tenant)
}

/// What the tenant checks can know about the organization table.
enum TenantState {
    /// There is no organization table to consult (no database file yet, or a
    /// database that was never migrated). Mapped entries cannot be
    /// cross-checked against existing organizations.
    Unknown,
    /// The table was read. `orgs` may legitimately be empty.
    Known {
        orgs: BTreeSet<String>,
        credentials: BTreeMap<String, OrgCredentials>,
    },
}

/// Read the organization table and the credentials the tenant map needs.
///
/// Only organizations that exist and are mapped get their credential pair
/// read. A missing or unmigrated database yields [`TenantState::Unknown`]
/// instead of an empty table, so a pre-flight check neither reports every
/// mapped organization as unknown on a fresh install nor pretends the table
/// was read.
fn tenant_state(context: &Context, config: &Config) -> Result<TenantState, String> {
    let orgs;
    let mut credentials = BTreeMap::new();

    let mut read_credentials = |orgs: &BTreeSet<String>,
                                get: &dyn Fn(&str) -> Result<Option<String>, String>|
     -> Result<(), String> {
        for organization in orgs {
            if !config.object_store.organizations.contains_key(organization) {
                continue;
            }
            let access_key = get(&format!("{organization}.access_key"))?.unwrap_or_default();
            let secret_key = get(&format!("{organization}.secret_key"))?.unwrap_or_default();
            credentials.insert(
                organization.clone(),
                OrgCredentials {
                    access_key: access_key.trim().to_string(),
                    secret_key: secret_key.trim().to_string(),
                },
            );
        }
        Ok(())
    };

    match config.database.driver {
        DatabaseDriver::Sqlite => {
            let Some(path) = context.database_path.as_deref() else {
                return Ok(TenantState::Unknown);
            };
            if !path.is_file() {
                return Ok(TenantState::Unknown);
            }
            let store = SqliteStore::open_read_only(path).map_err(|error| error.to_string())?;
            orgs = match store.list_orgs() {
                Ok(orgs) => orgs
                    .into_iter()
                    .map(|organization| organization.name)
                    .collect(),
                Err(error) if table_missing(&error) => return Ok(TenantState::Unknown),
                Err(error) => return Err(error.to_string()),
            };
            read_credentials(&orgs, &|name| {
                store
                    .get_secret(OBJECT_STORE_SECRET_GROUP, name)
                    .map_err(|error| error.to_string())
            })?;
        }
        DatabaseDriver::Postgres => {
            // An unreachable database is a real problem, but a database that
            // simply predates the first migration reports its missing relation
            // the same way a fresh SQLite file reports its missing table.
            let store = context
                .open_existing_metadata_store()
                .map_err(|error| error.to_string())?;
            orgs = match store.run(store.store().list_orgs()) {
                Ok(orgs) => orgs
                    .into_iter()
                    .map(|organization| organization.name)
                    .collect(),
                Err(error) if table_missing(&error) => return Ok(TenantState::Unknown),
                Err(error) => return Err(error.to_string()),
            };
            read_credentials(&orgs, &|name| {
                store
                    .run(store.store().get_secret(OBJECT_STORE_SECRET_GROUP, name))
                    .map_err(|error| error.to_string())
            })?;
        }
    }

    Ok(TenantState::Known { orgs, credentials })
}

/// Whether the store reports an organization table that does not exist yet.
fn table_missing(error: &StoreError) -> bool {
    let message = error.to_string();
    message.contains("no such table: orgs")
        || (message.contains("orgs") && message.contains("does not exist"))
}

/// Report the object-store tenant state and the last reload result.
///
/// Offline unless `probe` is set, in which case every configured bucket gets
/// one signed `HEAD`.
pub fn diagnostics(context: &Context, probe: bool) -> Result<(), CtlError> {
    let config = &context.config;
    let (mut problems, checks, tenant) = collect_config(context);

    println!(
        "{}",
        summary(config.object_store.require_org_bucket, &tenant)
    );
    println!("{}", reload_summary(&context.data_dir));

    if probe {
        println!("probing every configured bucket");
        let credentials = match &tenant {
            Some(Ok(TenantState::Known { credentials, .. })) => Some(credentials),
            _ => None,
        };
        probe_buckets(context, credentials, &mut problems)?;
    }
    finish(problems, checks)
}

/// The strict and organization summary lines. `None` is the local backend,
/// `Some(Err(..))` an unreadable store and `Unknown` an unreadable table.
fn summary(strict: bool, tenant: &Option<Result<TenantState, String>>) -> String {
    let organizations = match tenant {
        None => "0".to_string(),
        Some(Ok(TenantState::Known { orgs, .. })) => orgs.len().to_string(),
        Some(Ok(TenantState::Unknown)) | Some(Err(_)) => "?".to_string(),
    };
    format!(
        "strict: {}\norganizations: {organizations}",
        if strict { "enabled" } else { "disabled" }
    )
}

fn reload_summary(data_dir: &Path) -> String {
    match std::fs::read_to_string(data_dir.join(crate::reload::STATUS_FILE)) {
        Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
            Ok(status) => format!(
                "reload: outcome={} trigger={} boot id={}",
                status_field(&status, "outcome"),
                status_field(&status, "trigger"),
                status_field(&status, "boot_id")
            ),
            Err(_) => "reload: unreadable".to_string(),
        },
        Err(_) => "reload: never".to_string(),
    }
}

fn status_field<'a>(status: &'a serde_json::Value, field: &str) -> &'a str {
    status
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-")
}

/// Send one `HEAD` per configured bucket, sequentially, and record each
/// failure. The token never appears here.
fn probe_buckets(
    context: &Context,
    credentials: Option<&BTreeMap<String, OrgCredentials>>,
    problems: &mut Vec<String>,
) -> Result<(), CtlError> {
    let shared = &context.config.object_store;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| CtlError::message(format!("cannot start the probe runtime: {error}")))?;

    // Same fallback `check_config` accepts: a deployment may keep the shared
    // keys in the secrets table instead of the config file.
    let shared_access_key = non_empty(shared.access_key.as_deref())
        .map(str::to_string)
        .or_else(|| stored_secret(context, "bookshelf", "access_key_id"))
        .unwrap_or_default();
    let shared_secret_key = non_empty(shared.secret_key.as_deref())
        .map(str::to_string)
        .or_else(|| stored_secret(context, "bookshelf", "secret_access_key"))
        .unwrap_or_default();

    let mut targets: Vec<(String, String, S3Config)> = Vec::new();
    if !shared.require_org_bucket {
        if let Some(bucket) = non_empty(shared.bucket.as_deref()) {
            targets.push((
                shared
                    .endpoint
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                bucket.to_string(),
                probe_config(
                    shared,
                    None,
                    None,
                    &shared_access_key,
                    &shared_secret_key,
                    bucket,
                ),
            ));
        }
    }
    if let Some(bucket) = non_empty(shared.health_bucket.as_deref()) {
        targets.push((
            shared
                .endpoint
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_string(),
            bucket.to_string(),
            probe_config(
                shared,
                None,
                None,
                &shared_access_key,
                &shared_secret_key,
                bucket,
            ),
        ));
    }
    for (organization, entry) in &shared.organizations {
        let bucket = entry.bucket.trim();
        if bucket.is_empty() {
            continue;
        }
        let endpoint = non_empty(entry.endpoint.as_deref())
            .or_else(|| non_empty(shared.endpoint.as_deref()))
            .unwrap_or_default()
            .trim()
            .to_string();
        let credentials = credentials.and_then(|credentials| credentials.get(organization));
        targets.push((
            endpoint,
            bucket.to_string(),
            probe_config(
                shared,
                Some(entry),
                credentials,
                &shared_access_key,
                &shared_secret_key,
                bucket,
            ),
        ));
    }

    for (endpoint, bucket, config) in targets {
        match runtime.block_on(probe_endpoint(&endpoint, &bucket, &config)) {
            Ok(()) => println!("ok: bucket {bucket}"),
            Err(error) => problems.push(format!("bucket {bucket}: {error}")),
        }
    }
    Ok(())
}

/// Merge an entry's fields and credentials over the resolved shared settings,
/// exactly like the server does.
fn probe_config(
    shared: &ObjectStoreConfig,
    entry: Option<&ObjectStoreOrgConfig>,
    credentials: Option<&OrgCredentials>,
    shared_access_key: &str,
    shared_secret_key: &str,
    bucket: &str,
) -> S3Config {
    let field = |value: Option<&String>| {
        value
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let endpoint = entry
        .and_then(|entry| field(entry.endpoint.as_ref()))
        .or_else(|| field(shared.endpoint.as_ref()))
        .unwrap_or_default();
    let region = entry
        .and_then(|entry| field(entry.region.as_ref()))
        .unwrap_or_else(|| shared.region.clone());
    let access_key = credentials
        .map(|credentials| credentials.access_key.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| shared_access_key.to_string());
    let secret_key = credentials
        .map(|credentials| credentials.secret_key.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| shared_secret_key.to_string());
    let mut config = S3Config::new(endpoint, bucket, region, access_key, secret_key);
    config.path_style = entry
        .and_then(|entry| entry.path_style)
        .unwrap_or(shared.path_style);
    config
}

/// Resolve the configured bind to a socket address.
///
/// This is the `check-config` half of the pair: the recipe renders a literal
/// address, while a hand-written config may name its bind, and the listener
/// accepts both through [`tokio::net::TcpListener::bind`]. A literal address is
/// used unchanged. For a name the resolver's first result wins, in the order
/// [`ToSocketAddrs`] yields it, matching the recipe's `Resolv.getaddress`; a
/// name that does not resolve still fails the check.
fn resolve_bind(bind: &str) -> Option<SocketAddr> {
    if let Ok(address) = bind.parse::<SocketAddr>() {
        return Some(address);
    }
    bind.to_socket_addrs().ok()?.next()
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

/// An invocation of the `psql` compatibility command.
pub(crate) struct PsqlInvocation {
    pub service: Option<String>,
    pub write: bool,
    pub options: Vec<String>,
    pub args: Vec<String>,
}

/// Recognizes the upstream `psql SERVICE [--write] [--options VALUE]` form.
///
/// `args` comes from clap as a trailing var-arg so unknown client options are
/// preserved. The first remaining token is treated as a service name only when
/// it looks like one (and, for a bare single token, only when it is a known
/// Chef service); this keeps existing calls such as `psql "select 1"` and
/// `psql some-database.sqlite` working.
pub(crate) fn parse_psql_args(raw: &[String]) -> Result<PsqlInvocation, CtlError> {
    let mut write = false;
    let mut options = Vec::new();
    let mut passthrough = Vec::new();
    let mut index = 0;

    while index < raw.len() {
        let argument = &raw[index];
        if argument == "--write" {
            write = true;
        } else if argument == "--options" {
            index += 1;
            let value = raw.get(index).ok_or_else(|| {
                CtlError::message("--options requires a value for the database client")
            })?;
            options.extend(value.split_whitespace().map(str::to_string));
        } else if let Some(value) = argument.strip_prefix("--options=") {
            options.extend(value.split_whitespace().map(str::to_string));
        } else {
            passthrough.push(argument.clone());
        }
        index += 1;
    }

    let service = if passthrough
        .first()
        .map(|value| is_psql_service_name(value, passthrough.len() > 1))
        .unwrap_or(false)
    {
        Some(passthrough.remove(0))
    } else {
        None
    };

    Ok(PsqlInvocation {
        service,
        write,
        options,
        args: passthrough,
    })
}

/// Known upstream service names. These are accepted even as the only argument.
const KNOWN_PSQL_SERVICES: &[&str] = &[
    "bifrost",
    "bookshelf",
    "chef",
    "drbd",
    "erchef",
    "keepalived",
    "ldap",
    "manage",
    "nginx",
    "oc_bifrost",
    "oc_id",
    "oc-id",
    "opscode-chef",
    "opscode-erchef",
    "opscode-reporting",
    "postgres",
    "postgresql",
    "push-jobs-server",
    "rabbitmq",
    "redis",
    "redis_lb",
    "saml",
    "solr4",
    "stats",
];

fn is_psql_service_name(value: &str, has_trailing_arguments: bool) -> bool {
    let plausible = !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('-')
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-:".contains(character));
    plausible
        && (has_trailing_arguments
            || KNOWN_PSQL_SERVICES
                .iter()
                .any(|service| service.eq_ignore_ascii_case(value)))
}

/// Open an interactive `sqlite3` shell on the configured database, or `psql`
/// when a PostgreSQL URL is configured. Extra arguments are passed through.
///
/// The optional upstream service name is accepted but maps to the single
/// configured database. `--write` disables the default read-only session;
/// `--options` tokens are prepended to the trailing client arguments.
pub fn psql(context: &Context, invocation: &PsqlInvocation) -> Result<(), CtlError> {
    if let Some(service) = &invocation.service {
        eprintln!("note: psql service {service} maps to the configured database.");
    }
    let (program, mut command_args, target) = match context.config.database.driver {
        DatabaseDriver::Sqlite => {
            let path = context.sqlite_path()?.to_path_buf();
            // Ensure the file exists and has the current schema before opening it.
            drop(context.open_store()?);
            // SQLite's `-readonly` cannot open a WAL database when it cannot
            // create the shared-memory file, so the SQLite backend keeps the
            // client tool's normal behavior. `--write` is accepted for
            // upstream compatibility; write access is governed by the file
            // permissions and the configured credentials.
            ("sqlite3", vec![path.display().to_string()], None)
        }
        DatabaseDriver::Postgres => {
            let target = crate::pg::PgTarget::parse(&context.database_url)?;
            (
                "psql",
                vec!["--dbname".to_string(), target.url().to_string()],
                Some(target),
            )
        }
    };

    command_args.splice(0..0, invocation.options.iter().cloned());
    command_args.extend(invocation.args.iter().cloned());
    let mut command = std::process::Command::new(program);
    command.args(&command_args);
    if let Some(target) = &target {
        target.apply_environment(&mut command);
    }
    if target.is_some() && !invocation.write {
        // Match upstream's read-only default for the named service database.
        command.env("PGOPTIONS", "-c default_transaction_read_only=on");
    }
    let status = command
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

fn parse_http_url(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.host_str().filter(|host| !host.is_empty())?;
    Some(url)
}

fn redacted_url(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// Validate a custom WebUI key path the way startup materialization needs it:
/// the basename must be usable, the path must not already be a directory, and
/// the nearest existing ancestor directory must accept a write from the
/// service account (startup creates missing directories itself). A
/// `ProtectSystem=strict` unit makes an outside-data-dir path read-only unless
/// the operator adds a drop-in. The write probe runs as the invoking user,
/// which under `ExecStartPre` is the service account.
fn validate_webui_key_path(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return Err(
            "is an existing directory; name the key file instead, such as webui_priv.pem"
                .to_string(),
        );
    }
    let name = path
        .file_name()
        .ok_or_else(|| "must be a file path, not a directory".to_string())?;
    if name == "webui_pub.pem" {
        return Err(
            "the basename is overwritten by the derived public key; use webui_priv.pem or another name"
                .to_string(),
        );
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "must be an absolute path".to_string())?;
    // Startup creates missing directories, so only the nearest existing
    // ancestor has to exist and be writable.
    let mut probe_dir = parent;
    loop {
        match std::fs::metadata(probe_dir) {
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => return Err(format!("{} is not a directory", probe_dir.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                probe_dir = probe_dir
                    .parent()
                    .filter(|ancestor| !ancestor.as_os_str().is_empty())
                    .ok_or_else(|| {
                        format!(
                            "cannot find an existing ancestor directory of {}",
                            parent.display()
                        )
                    })?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect parent directory {}: {error}",
                    probe_dir.display()
                ))
            }
        }
    }
    // `create_new` keeps an existing file intact, so neither a stale probe nor a
    // configured key that happens to use the probe name is truncated. Each
    // collision moves on to the next candidate name.
    let mut failure = None;
    for attempt in 0..16u32 {
        let probe = probe_dir.join(format!(".webui-key-probe-{}-{attempt}", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(file) => {
                drop(file);
                let _ = std::fs::remove_file(&probe);
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => failure = Some(error),
        }
        break;
    }
    match failure {
        Some(error) => Err(format!(
            "directory {} is not writable by the service account ({error}); a path outside \
             paths.data_dir also needs a `ReadWritePaths=` systemd drop-in",
            probe_dir.display()
        )),
        None => Err(format!(
            "could not find a free probe file name in {}",
            probe_dir.display()
        )),
    }
}

fn validate_required_recipe_path(path: &Path) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| format!("cannot read file: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("must not be a symlink".to_string());
    }
    if !metadata.is_file() {
        return Err("must be a regular file".to_string());
    }
    if metadata.len() > MAX_REQUIRED_RECIPE_BYTES {
        return Err(format!(
            "must be no larger than {MAX_REQUIRED_RECIPE_BYTES} bytes"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o022 != 0 {
            return Err("must not be group- or world-writable".to_string());
        }
    }
    Ok(())
}

fn stored_secret(context: &Context, group: &str, name: &str) -> Option<String> {
    let value = match context.config.database.driver {
        DatabaseDriver::Sqlite => {
            let path = context.database_path.as_deref()?;
            if !path.is_file() {
                return None;
            }
            SqliteStore::open_read_only(path)
                .ok()?
                .get_secret(group, name)
                .ok()
                .flatten()
        }
        DatabaseDriver::Postgres => {
            // Only reached when configuration omitted the credential and the
            // object-store backend needs it. A database that cannot be opened
            // is reported as a missing credential by the caller, not as a
            // configuration-check panic.
            let store = context.open_existing_metadata_store().ok()?;
            store
                .run(store.store().get_secret(group, name))
                .ok()
                .flatten()
        }
    };
    value.filter(|value| !value.is_empty())
}

fn database_driver_name(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Sqlite => "sqlite",
        DatabaseDriver::Postgres => "postgresql",
    }
}

pub(crate) fn search_backend_name(backend: SearchBackend) -> &'static str {
    match backend {
        SearchBackend::Embedded => "embedded",
        SearchBackend::OpenSearch => "opensearch",
        SearchBackend::Elasticsearch => "elasticsearch",
    }
}

pub(crate) fn object_store_backend_name(backend: ObjectStoreBackend) -> &'static str {
    match backend {
        ObjectStoreBackend::Local => "local",
        ObjectStoreBackend::S3 => "s3",
        ObjectStoreBackend::Garage => "garage",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        check_config, diagnostics, parse_psql_args, reload_summary, resolve_bind, summary, Context,
        PsqlInvocation, TenantState,
    };
    use microwave_config::{ObjectStoreBackend, ObjectStoreOrgConfig};
    use microwave_store::{SqliteStore, OBJECT_STORE_SECRET_GROUP};
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, ToSocketAddrs};
    use std::path::{Path, PathBuf};

    fn parse(arguments: &[&str]) -> PsqlInvocation {
        let raw: Vec<String> = arguments.iter().map(|value| value.to_string()).collect();
        parse_psql_args(&raw).unwrap()
    }

    /// A `Context` built by hand, which (unlike `Context::load`) does not run
    /// `Config::validate`, so the defensive pool-size guard can be reached.
    fn context_with_pool_size(pool_size: usize) -> Context {
        let mut config = microwave_config::Config::default();
        config.database.pool_size = pool_size;
        Context {
            database_url: config.resolved_database_url(),
            database_path: Some(PathBuf::from("/nonexistent/microwave.db")),
            data_dir: PathBuf::from("/nonexistent/microwave-data"),
            config,
        }
    }

    #[test]
    fn check_config_reports_a_zero_pool_size() {
        let error = check_config(&context_with_pool_size(0)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("database.pool_size must be greater than zero"),
            "unexpected error: {error}"
        );
        assert!(check_config(&context_with_pool_size(10)).is_ok());
    }

    fn context_with_bind(bind: &str) -> Context {
        let mut context = context_with_pool_size(10);
        context.config.server.bind = bind.to_string();
        context
    }

    #[test]
    fn check_config_resolves_a_host_name_bind() {
        assert!(check_config(&context_with_bind("localhost:8000")).is_ok());
    }

    #[test]
    fn check_config_rejects_an_unresolvable_bind() {
        let error = check_config(&context_with_bind("no-such-host.invalid")).unwrap_err();
        assert!(
            error.to_string().contains("server.bind"),
            "unexpected error: {error}"
        );
    }

    fn context_with_webui_key_path(path: PathBuf) -> Context {
        let mut context = context_with_pool_size(10);
        context.config.auth.webui_key = true;
        context.config.auth.webui_key_path = Some(path);
        context
    }

    #[test]
    fn check_config_accepts_a_writable_webui_key_parent() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_with_webui_key_path(dir.path().join("webui_priv.pem"));
        assert!(check_config(&context).is_ok());
    }

    #[test]
    fn check_config_accepts_a_missing_webui_key_parent_that_startup_creates() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_with_webui_key_path(dir.path().join("nested/keys/webui_priv.pem"));
        assert!(check_config(&context).is_ok());
    }

    #[test]
    fn check_config_rejects_a_webui_key_path_that_is_an_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys");
        std::fs::create_dir(&keys).unwrap();
        let error = check_config(&context_with_webui_key_path(keys)).unwrap_err();
        assert!(
            error.to_string().contains("existing directory"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn check_config_rejects_a_webui_key_path_whose_parent_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let error =
            check_config(&context_with_webui_key_path(file.join("webui_priv.pem"))).unwrap_err();
        assert!(
            error.to_string().contains("not a directory"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn check_config_rejects_webui_pub_pem_as_the_private_key_path() {
        let dir = tempfile::tempdir().unwrap();
        let error = check_config(&context_with_webui_key_path(
            dir.path().join("webui_pub.pem"),
        ))
        .unwrap_err();
        assert!(
            error.to_string().contains("public key"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn check_config_rejects_a_bind_without_a_port() {
        let error = check_config(&context_with_bind("127.0.0.1")).unwrap_err();
        assert!(
            error.to_string().contains("server.bind"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_bind_keeps_a_literal_address_unchanged() {
        assert_eq!(
            resolve_bind("127.0.0.1:8000"),
            Some("127.0.0.1:8000".parse().unwrap())
        );
    }

    #[test]
    fn resolve_bind_takes_the_first_address_a_name_resolves_to() {
        let first = "localhost:8000"
            .to_socket_addrs()
            .ok()
            .and_then(|mut addresses| addresses.next());
        assert!(first.is_some());
        assert_eq!(resolve_bind("localhost:8000"), first);
    }

    #[test]
    fn accepts_a_service_name_write_and_options() {
        let invocation = parse(&["erchef", "--write", "--options", "-X -q", "-c", "select 1"]);
        assert_eq!(invocation.service.as_deref(), Some("erchef"));
        assert!(invocation.write);
        assert_eq!(invocation.options, vec!["-X", "-q"]);
        assert_eq!(invocation.args, vec!["-c", "select 1"]);
    }

    #[test]
    fn accepts_any_plausible_service_when_arguments_follow() {
        let invocation = parse(&["my-service", "-c", "select 1"]);
        assert_eq!(invocation.service.as_deref(), Some("my-service"));

        let invocation = parse(&["my_service", "--options=--no-psqlrc", "select 1"]);
        assert_eq!(invocation.service.as_deref(), Some("my_service"));
        assert_eq!(invocation.options, vec!["--no-psqlrc"]);
        assert_eq!(invocation.args, vec!["select 1"]);
    }

    #[test]
    fn keeps_sql_and_database_paths_as_arguments() {
        let invocation = parse(&["select count(*) from users;"]);
        assert!(invocation.service.is_none());
        assert_eq!(invocation.args, vec!["select count(*) from users;"]);

        let invocation = parse(&["some-database.sqlite", "select 1"]);
        assert!(invocation.service.is_none());
        assert_eq!(invocation.args, vec!["some-database.sqlite", "select 1"]);

        let invocation = parse(&["-tA", "-c", "select 1"]);
        assert!(invocation.service.is_none());
        assert_eq!(invocation.args, vec!["-tA", "-c", "select 1"]);
    }

    #[test]
    fn rejects_options_without_a_value() {
        assert!(parse_psql_args(&["--options".to_string()]).is_err());
    }

    /// A `Context` on a temporary directory with a real SQLite database that
    /// already holds the `acme` organization, and a strict S3 config.
    fn tenant_context(dir: &Path) -> Context {
        let database_path = dir.join("microwave.db");
        let store = SqliteStore::open(&database_path).unwrap();
        store.create_org("acme", "Acme").unwrap();
        drop(store);

        let mut config = microwave_config::Config::default();
        config.paths.data_dir = dir.join("data").display().to_string();
        config.database.url = Some(format!("sqlite://{}", database_path.display()));
        config.object_store.backend = ObjectStoreBackend::S3;
        config.object_store.endpoint = Some("http://127.0.0.1:1".into());
        config.object_store.access_key = Some("shared-access".into());
        config.object_store.secret_key = Some("shared-secret".into());
        config.object_store.require_org_bucket = true;
        config.object_store.health_bucket = Some("mw-health".into());

        Context {
            database_url: config.resolved_database_url(),
            database_path: Some(database_path),
            data_dir: dir.join("data"),
            config,
        }
    }

    /// A strict S3 context with `ghost` mapped and no database file yet.
    fn unmapped_context(dir: &Path) -> Context {
        let database_path = dir.join("microwave.db");
        let mut config = microwave_config::Config::default();
        config.paths.data_dir = dir.join("data").display().to_string();
        config.database.url = Some(format!("sqlite://{}", database_path.display()));
        config.object_store.backend = ObjectStoreBackend::S3;
        config.object_store.endpoint = Some("http://127.0.0.1:1".into());
        config.object_store.access_key = Some("shared-access".into());
        config.object_store.secret_key = Some("shared-secret".into());
        config.object_store.require_org_bucket = true;
        config.object_store.health_bucket = Some("mw-health".into());
        config.object_store.organizations.insert(
            "ghost".to_string(),
            ObjectStoreOrgConfig {
                bucket: "mw-ghost".into(),
                ..Default::default()
            },
        );

        Context {
            database_url: config.resolved_database_url(),
            database_path: Some(database_path),
            data_dir: dir.join("data"),
            config,
        }
    }

    /// Complete the tenant map for `acme` with a bucket and both secrets.
    fn map_acme(context: &mut Context) {
        context.config.object_store.organizations.insert(
            "acme".to_string(),
            ObjectStoreOrgConfig {
                bucket: "mw-acme".into(),
                ..Default::default()
            },
        );
        let store = SqliteStore::open(context.database_path.as_deref().unwrap()).unwrap();
        store
            .set_secret(OBJECT_STORE_SECRET_GROUP, "acme.access_key", "org-access")
            .unwrap();
        store
            .set_secret(OBJECT_STORE_SECRET_GROUP, "acme.secret_key", "org-secret")
            .unwrap();
    }

    /// A one-shot HTTP server answering each accepted connection with the
    /// next status in `statuses`.
    fn mock_bucket_server(statuses: &[u16]) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let statuses = statuses.to_vec();
        let handle = std::thread::spawn(move || {
            for status in statuses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let reason = match status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Error",
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
                )
                .unwrap();
                stream.flush().unwrap();
            }
        });
        (endpoint, handle)
    }

    #[test]
    fn diagnostics_and_check_config_report_an_unmapped_org() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = tenant_context(dir.path());

        let error = diagnostics(&context, false).unwrap_err();
        assert!(
            error.to_string().contains("acme"),
            "unexpected error: {error}"
        );
        let error = check_config(&context).unwrap_err();
        assert!(
            error.to_string().contains("acme"),
            "unexpected error: {error}"
        );

        map_acme(&mut context);
        assert!(check_config(&context).is_ok());
        assert!(diagnostics(&context, false).is_ok());
    }

    #[test]
    fn an_unreadable_tenant_table_is_unknown_not_an_org_error() {
        // No database file yet: startup must not reject a fresh install.
        let dir = tempfile::tempdir().unwrap();
        let context = unmapped_context(dir.path());
        assert!(check_config(&context).is_ok());
        assert!(diagnostics(&context, false).is_ok());

        // A file that exists but was never migrated is the same state.
        std::fs::write(context.database_path.as_deref().unwrap(), b"").unwrap();
        assert!(check_config(&context).is_ok());

        // A migrated, empty table is known-empty: the entry is then unknown.
        drop(SqliteStore::open(context.database_path.as_deref().unwrap()).unwrap());
        let error = check_config(&context).unwrap_err();
        assert!(error.to_string().contains("ghost"), "unexpected: {error}");
    }

    #[test]
    fn diagnostics_only_probes_with_the_flag() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = tenant_context(dir.path());
        map_acme(&mut context);
        context.config.object_store.require_org_bucket = false;
        context.config.object_store.bucket = Some("mw-bucket".into());
        context.config.object_store.health_bucket = None;
        context.config.object_store.organizations.clear();

        // Nothing listens on port 1, so the offline run must not connect.
        assert!(diagnostics(&context, false).is_ok());
        let error = diagnostics(&context, true).unwrap_err();
        assert!(
            error.to_string().contains("mw-bucket"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn probe_reports_a_missing_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = tenant_context(dir.path());
        map_acme(&mut context);
        let (endpoint, server) = mock_bucket_server(&[404, 404]);
        context.config.object_store.endpoint = Some(endpoint);

        let error = diagnostics(&context, true).unwrap_err();
        server.join().unwrap();
        assert!(
            error.to_string().contains("mw-acme"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn probe_passes_when_every_bucket_answers() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = tenant_context(dir.path());
        map_acme(&mut context);
        let (endpoint, server) = mock_bucket_server(&[200, 200]);
        context.config.object_store.endpoint = Some(endpoint);

        diagnostics(&context, true).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn probe_falls_back_to_stored_shared_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = tenant_context(dir.path());
        map_acme(&mut context);
        context.config.object_store.require_org_bucket = false;
        context.config.object_store.bucket = Some("mw-global".into());
        context.config.object_store.access_key = None;
        context.config.object_store.secret_key = None;
        let store = SqliteStore::open(context.database_path.as_deref().unwrap()).unwrap();
        store
            .set_secret("bookshelf", "access_key_id", "shared-access")
            .unwrap();
        store
            .set_secret("bookshelf", "secret_access_key", "shared-secret")
            .unwrap();
        drop(store);

        // Global bucket, health bucket, and the org bucket.
        let (endpoint, server) = mock_bucket_server(&[200, 200, 200]);
        context.config.object_store.endpoint = Some(endpoint);

        diagnostics(&context, true).unwrap();
        server.join().unwrap();
    }

    fn state_with(organizations: usize) -> TenantState {
        TenantState::Known {
            orgs: (0..organizations)
                .map(|index| format!("org{index}"))
                .collect::<BTreeSet<_>>(),
            credentials: BTreeMap::new(),
        }
    }

    #[test]
    fn summary_reports_the_strict_and_organization_state() {
        assert_eq!(summary(false, &None), "strict: disabled\norganizations: 0");
        assert_eq!(
            summary(true, &Some(Ok(state_with(2)))),
            "strict: enabled\norganizations: 2"
        );
        assert_eq!(
            summary(true, &Some(Err("boom".to_string()))),
            "strict: enabled\norganizations: ?"
        );
        assert_eq!(
            summary(true, &Some(Ok(TenantState::Unknown))),
            "strict: enabled\norganizations: ?"
        );
    }

    #[test]
    fn reload_summary_reports_never_unreadable_and_the_last_outcome() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(reload_summary(dir.path()), "reload: never");

        let status = dir.path().join(crate::reload::STATUS_FILE);
        std::fs::write(&status, "not json").unwrap();
        assert_eq!(reload_summary(dir.path()), "reload: unreadable");

        std::fs::write(
            &status,
            "{\"boot_id\":\"b1\",\"token\":\"secret-token\",\"trigger\":\"sighup\",\"outcome\":\"ok\"}",
        )
        .unwrap();
        let line = reload_summary(dir.path());
        assert!(line.contains("outcome=ok"), "line: {line}");
        assert!(line.contains("trigger=sighup"), "line: {line}");
        assert!(line.contains("boot id=b1"), "line: {line}");
        assert!(!line.contains("secret-token"), "line: {line}");
    }
}
