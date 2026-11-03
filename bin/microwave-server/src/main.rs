mod bootstrap;

use clap::{Parser, Subcommand};
use microwave_api::{router, AppState};
use microwave_config::{
    Config, DatabaseDriver, ObjectStoreBackend, SearchBackend, StorageCompression,
};
use microwave_metrics::{DependencyState, KeygenGauges, Metrics, PoolGauges};
use microwave_objectstore::{LocalObjectStore, ObjectStore, S3Config, S3ObjectStore};
use microwave_store::{Compression, MetadataStore, SqliteStore};
use microwave_store_postgres::PostgresStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Time allowed to drain in-flight requests after SIGTERM/SIGINT on the TLS
/// listener. Plain HTTP uses axum's graceful shutdown, which waits for active
/// requests without a separate timer.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Parser)]
#[command(name = "microwave-server", version, about = "Microwave Server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the server version.
    Version,
    /// Print the resolved configuration as JSON.
    Config {
        /// Path to a TOML configuration file.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// Start the server.
    Serve {
        /// Path to a TOML configuration file.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Load configuration and initialize storage, then exit.
        #[arg(long)]
        check: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
        }
        Command::Config { config } => {
            let mut config = Config::load(config.as_deref())?;
            // `config` is a user-facing diagnostic. Never print credentials.
            redact_config_for_output(&mut config);
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        Command::Serve { config, check } => {
            let mut config = Config::load(config.as_deref())?;
            warn_implicit_mcp_superuser(&config);
            // Load and validate the pair before touching the database so a bad
            // certificate fails fast for both `serve` and `serve --check`.
            let tls = if config.server.tls.is_enabled() {
                Some(microwave_tls::load(&config.server.tls).map_err(|error| error.to_string())?)
            } else {
                None
            };
            let scheme = if tls.is_some() { "https" } else { "http" };
            let reload_interval_seconds = config.server.tls.reload_interval_seconds;

            // Build the rustls handle and install the SIGHUP handler before any
            // database work, bootstrap, or listener bind. An early `kill -HUP`
            // must never hit the default terminate action. The handler is
            // installed for plain HTTP too and reports that there is nothing to
            // reload, so a configured `ExecReload` stays safe.
            let tls_config = tls
                .as_ref()
                .map(|tls| axum_server::tls_rustls::RustlsConfig::from_config(tls.config.clone()));
            let tls_reloader = Arc::new(TlsReloader::new(
                tls_config.clone(),
                tls.as_ref()
                    .map(|tls| (tls.cert_path.clone(), tls.key_path.clone())),
            ));
            spawn_sighup_handler(tls_reloader.clone());
            if reload_interval_seconds > 0 {
                spawn_periodic_reloader(tls_reloader.clone(), reload_interval_seconds);
            }

            // Register SIGTERM/SIGINT before database bootstrap and the
            // listener bind. The notification is retained if a signal arrives
            // before the server waits for it, so `docker stop` during startup
            // still drains instead of being a default terminate.
            let shutdown = Arc::new(tokio::sync::Notify::new());
            spawn_shutdown_handler(shutdown.clone());

            // One registry for the whole process: the request middleware, the
            // `/_stats` view and the OTLP recorder all record into it.
            let metrics = Arc::new(Metrics::new());

            let database_url = config.resolved_database_url();
            let compression = store_compression(config.storage.compression);
            let (store, sqlite): (Arc<dyn MetadataStore>, Option<Arc<SqliteStore>>) =
                match config.database.driver {
                    DatabaseDriver::Sqlite => {
                        let path = sqlite_path(&database_url);
                        let store = Arc::new(SqliteStore::open_with_compression(
                            Path::new(path),
                            compression,
                        )?);
                        store.set_metrics(metrics.clone());
                        (store.clone(), Some(store))
                    }
                    DatabaseDriver::Postgres => {
                        if !(database_url.starts_with("postgres://")
                            || database_url.starts_with("postgresql://"))
                        {
                            return Err("database.driver = postgres requires database.url / \
                                 MW_DATABASE__URL with a postgres:// URL"
                                .into());
                        }
                        let store = Arc::new(
                            PostgresStore::connect_with_compression_and_pool(
                                &database_url,
                                compression,
                                config.database.pool_size,
                            )
                            .await?,
                        );
                        store.set_metrics(metrics.clone());
                        (store, None)
                    }
                };
            bootstrap::ensure_superuser(store.as_ref(), &config).await?;
            // Configuration and environment win over the stored header
            // secret; loading it here fails startup when the stored value is
            // malformed instead of failing the first export.
            bootstrap::apply_otel_secrets(&mut config, store.as_ref()).await?;
            let secret = match config.object_store.backend {
                ObjectStoreBackend::Local => {
                    let secret =
                        bootstrap::ensure_object_store_secret(&config, store.as_ref()).await?;
                    config.object_store.secret = Some(secret.clone());
                    secret
                }
                // S3/Garage clients use presigned URLs directly, so there is
                // no local bookshelf token secret to bootstrap. Credentials
                // may still come from the secret store.
                _ => {
                    bootstrap::apply_bookshelf_secrets(&mut config, store.as_ref()).await?;
                    config.object_store.secret.clone().unwrap_or_default()
                }
            };
            let object_store = build_object_store(&config, &secret)?;

            if check {
                // Never print the database URL: it may contain credentials.
                let backend = match config.database.driver {
                    DatabaseDriver::Sqlite => {
                        format!("sqlite ({})", sqlite_path(&database_url))
                    }
                    DatabaseDriver::Postgres => "postgres".to_string(),
                };
                eprintln!(
                    "serve: configuration ok (bind {}, scheme {scheme}, database backend {backend})",
                    config.server.bind
                );
                // Report the state without initializing the provider: `--check`
                // must never dial the collector.
                eprintln!(
                    "otel: {}",
                    if config.otel.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                return Ok(());
            }

            let bind = config.server.bind.clone();
            let gauge_refresh_seconds = config.otel.gauge_refresh_seconds;
            // Bind before the provider exists: a bind failure must not leave an
            // initialized exporter behind with nothing to serve.
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            let local_addr = listener.local_addr()?;

            let indexing_mode = indexing_mode(config.search.backend);
            let otel = if config.otel.enabled {
                Some(microwave_otel::init(
                    &config.otel,
                    metrics.clone(),
                    scheme,
                    indexing_mode,
                )?)
            } else {
                None
            };

            let state = match sqlite {
                Some(sqlite) => {
                    AppState::with_metrics(sqlite, Arc::new(config), object_store, metrics.clone())
                }
                None => {
                    AppState::with_metrics(store, Arc::new(config), object_store, metrics.clone())
                }
            };
            let state = match state {
                Ok(state) => state,
                Err(error) => {
                    if let Some(otel) = otel {
                        if let Err(error) = otel.shutdown().await {
                            tracing::warn!(%error, "OTLP metric shutdown did not complete");
                        }
                    }
                    return Err(error.into());
                }
            };
            tracing::info!(%bind, %local_addr, scheme, "microwave-server listening");
            let gauge_refresh = otel
                .as_ref()
                .map(|_| spawn_gauge_refresh(state.clone(), metrics, gauge_refresh_seconds));
            let served = serve(state, listener, tls_config, shutdown).await;

            // Stop sampling before the final flush, so the last export is not
            // raced by a tick that still writes gauge values.
            if let Some(gauge_refresh) = gauge_refresh {
                gauge_refresh.abort();
                let _ = gauge_refresh.await;
            }
            if let Some(otel) = otel {
                // The guard applies `shutdown_timeout_ms` itself.
                if let Err(error) = otel.shutdown().await {
                    tracing::warn!(%error, "OTLP metric shutdown did not complete");
                }
            }
            served?;
        }
    }

    Ok(())
}

/// Serve until shutdown, returning the accept-loop or TLS-serve error.
///
/// The caller keeps ownership of the metric guard and the refresh task, so an
/// early return here still reaches the flush path.
async fn serve(
    state: AppState,
    listener: tokio::net::TcpListener,
    tls_config: Option<axum_server::tls_rustls::RustlsConfig>,
    shutdown: Arc<tokio::sync::Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    match tls_config {
        Some(rustls_config) => {
            let listener = listener.into_std()?;
            let handle = axum_server::Handle::new();
            let shutdown_handle = handle.clone();
            tokio::spawn(async move {
                shutdown.notified().await;
                shutdown_handle.graceful_shutdown(Some(SHUTDOWN_GRACE));
            });
            axum_server::from_tcp_rustls(listener, rustls_config)?
                .handle(handle)
                .serve(router(state).into_make_service_with_connect_info::<std::net::SocketAddr>())
                .await?;
        }
        None => {
            let shutdown = shutdown.clone();
            axum::serve(
                listener,
                router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                shutdown.notified().await;
            })
            .await?;
        }
    }
    Ok(())
}

/// The `indexing.mode` attribute on the build-info series.
fn indexing_mode(backend: SearchBackend) -> &'static str {
    match backend {
        SearchBackend::Embedded => "embedded",
        SearchBackend::OpenSearch => "opensearch",
        SearchBackend::Elasticsearch => "elasticsearch",
    }
}

/// Sample the store, keygen and dependency probes into the registry once per
/// interval. The first tick fires immediately.
///
/// Backend errors are logged and skipped so one failing read does not stop the
/// loop; the next tick retries.
fn spawn_gauge_refresh(
    state: AppState,
    metrics: Arc<Metrics>,
    interval_seconds: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // `Config::validate` rejects zero, but an env-only config can still
        // reach this point with one; the interval panics on zero.
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_seconds.max(1)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let dependencies = state
                .dependency_states()
                .await
                .into_iter()
                .map(|(name, up)| DependencyState { name, up })
                .collect();
            metrics.set_dependencies(dependencies);
            metrics.set_pool(state.store.pool_stats().await.map(|pool| PoolGauges {
                idle: pool.idle,
                in_use: pool.in_use,
                max: pool.max,
            }));
            match state.store.maintenance_enabled().await {
                Ok(enabled) => metrics.set_maintenance(enabled),
                Err(error) => {
                    tracing::warn!(%error, "failed to read maintenance mode for metrics")
                }
            }
            match state.store.count_nodes().await {
                Ok(nodes) => metrics.set_nodes(nodes),
                Err(error) => tracing::warn!(%error, "failed to count nodes for metrics"),
            }
            let keygen = state.keygen.status();
            metrics.set_keygen(KeygenGauges {
                cached: keygen.cached,
                capacity: keygen.capacity,
                workers: keygen.workers,
                worker_failures: keygen.worker_failures,
                warm_up_failures: keygen.warm_up_failures,
                generated_on_demand: keygen.generated_on_demand,
                worker_healthy: keygen.worker_healthy,
            });
        }
    })
}

fn sqlite_path(database_url: &str) -> &str {
    database_url
        .strip_prefix("sqlite://")
        .unwrap_or(database_url)
}

/// Warn when an MCP organization resolves to the superuser principal (because
/// `principal` is unset or names the superuser) while writes are enabled. The
/// safe defaults stay unchanged; this only makes the risky combination visible
/// at startup.
fn warn_implicit_mcp_superuser(config: &Config) {
    if !config.mcp.enabled {
        return;
    }
    for (organization, entry) in &config.mcp.organizations {
        let configured = entry
            .principal
            .as_deref()
            .map(str::trim)
            .filter(|principal| !principal.is_empty());
        let implicit =
            configured.is_none() || configured == Some(config.auth.superuser_name.as_str());
        if implicit && entry.writes {
            tracing::warn!(
                organization = %organization,
                principal = configured.unwrap_or(config.auth.superuser_name.as_str()),
                "MCP organization uses the superuser principal with writes enabled; configure an explicit non-superuser principal or set writes = false"
            );
        }
    }
}

fn store_compression(compression: StorageCompression) -> Compression {
    match compression {
        StorageCompression::Zlib => Compression::Zlib,
        StorageCompression::None => Compression::None,
    }
}

/// Hide credentials before printing configuration.
fn redact_config_for_output(config: &mut Config) {
    config.database.url = config.database.url.as_deref().map(redact_database_url);
    config.data_collector.token = config
        .data_collector
        .token
        .as_ref()
        .map(|_| "***".to_string());
    config.object_store.secret = config
        .object_store
        .secret
        .as_ref()
        .map(|_| "***".to_string());
    config.object_store.access_key = config
        .object_store
        .access_key
        .as_ref()
        .map(|_| "***".to_string());
    config.object_store.secret_key = config
        .object_store
        .secret_key
        .as_ref()
        .map(|_| "***".to_string());
    config.otel.headers = config
        .otel
        .headers
        .iter()
        .map(|header| match header.split_once(':') {
            Some((name, _)) => format!("{name}: ***"),
            None => header.clone(),
        })
        .collect();
}

/// Hide the password in a database URL before printing configuration.
///
/// `postgres://user:secret@host/db` becomes `postgres://user:***@host/db`.
/// URLs without credentials are returned unchanged.
fn redact_database_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let Some((userinfo, host)) = rest.rsplit_once('@') else {
        return url.to_string();
    };
    let Some((user, _password)) = userinfo.split_once(':') else {
        return url.to_string();
    };
    format!("{scheme}://{user}:***@{host}")
}

fn listener_scheme(config: &Config) -> &'static str {
    if config.server.tls.is_enabled() {
        "https"
    } else {
        "http"
    }
}

/// Base URL for local `/bookshelf` presigned links.
///
/// `server.public_url` wins so a proxy-hosted deployment can hand out links on
/// the name clients use instead of the socket it listens on. Without it the
/// listener scheme and `server.bind` are used, because the server may serve
/// HTTPS directly.
fn local_object_store_base_url(config: &Config) -> String {
    match config.server.public_url.as_deref() {
        Some(url) => url.trim_end_matches('/').to_string(),
        None => format!("{}://{}", listener_scheme(config), config.server.bind),
    }
}

fn build_object_store(
    config: &Config,
    secret: &str,
) -> Result<Arc<dyn ObjectStore + Send + Sync>, Box<dyn std::error::Error>> {
    match config.object_store.backend {
        ObjectStoreBackend::Local => Ok(Arc::new(LocalObjectStore::new(
            Path::new(&config.paths.data_dir).join("bookshelf"),
            secret,
            local_object_store_base_url(config),
        ))),
        ObjectStoreBackend::S3 | ObjectStoreBackend::Garage => {
            let endpoint = config
                .object_store
                .endpoint
                .clone()
                .ok_or("object_store.endpoint is required for the s3/garage backend")?;
            let bucket = config
                .object_store
                .bucket
                .clone()
                .ok_or("object_store.bucket is required for the s3/garage backend")?;
            let access_key = config
                .object_store
                .access_key
                .clone()
                .ok_or("object_store.access_key is required for the s3/garage backend")?;
            let secret_key = config
                .object_store
                .secret_key
                .clone()
                .ok_or("object_store.secret_key is required for the s3/garage backend")?;
            let mut s3_config = S3Config::new(
                endpoint,
                bucket,
                config.object_store.region.clone(),
                access_key,
                secret_key,
            );
            s3_config.path_style = config.object_store.path_style;
            Ok(Arc::new(S3ObjectStore::new(s3_config)?))
        }
    }
}

/// Shared TLS reload state for the signal handler and the polling timer.
///
/// Reloads are serialized: a SIGHUP that arrives while a timer reload is
/// running waits for it and then reloads again instead of racing the file reads
/// and the `ArcSwap` update.
struct TlsReloader {
    target: Option<TlsReloadTarget>,
    lock: tokio::sync::Mutex<()>,
}

struct TlsReloadTarget {
    handle: axum_server::tls_rustls::RustlsConfig,
    cert_path: PathBuf,
    key_path: PathBuf,
}

impl TlsReloader {
    fn new(
        handle: Option<axum_server::tls_rustls::RustlsConfig>,
        paths: Option<(PathBuf, PathBuf)>,
    ) -> Self {
        let target = match (handle, paths) {
            (Some(handle), Some((cert_path, key_path))) => Some(TlsReloadTarget {
                handle,
                cert_path,
                key_path,
            }),
            _ => None,
        };
        Self {
            target,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    async fn reload(&self, trigger: &'static str) {
        let Some(target) = &self.target else {
            tracing::info!(trigger, "TLS is disabled; nothing to reload");
            return;
        };
        let _guard = self.lock.lock().await;
        reload_tls(&target.handle, &target.cert_path, &target.key_path, trigger).await;
    }
}

/// Install the SIGHUP handler synchronously, before database bootstrap and the
/// listener bind. Plain-HTTP deployments get the handler too, so a configured
/// `ExecReload`/`docker kill --signal=HUP` is always safe.
fn spawn_sighup_handler(reloader: Arc<TlsReloader>) {
    #[cfg(unix)]
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
        Ok(mut hangup) => {
            tracing::info!("installed SIGHUP handler");
            tokio::spawn(async move {
                while hangup.recv().await.is_some() {
                    reloader.reload("SIGHUP").await;
                }
            });
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "failed to install the SIGHUP handler; signals will not reload TLS"
            );
        }
    }

    #[cfg(not(unix))]
    let _ = reloader;
}

/// Install SIGTERM and SIGINT handlers before database bootstrap and the
/// listener bind. The notification is retained if a signal arrives before the
/// server starts waiting, so a container stop during startup still drains.
fn spawn_shutdown_handler(shutdown: Arc<tokio::sync::Notify>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(mut terminate), Ok(mut interrupt)) => {
                tracing::info!("installed SIGTERM/SIGINT shutdown handlers");
                tokio::spawn(async move {
                    let signal = tokio::select! {
                        _ = terminate.recv() => "SIGTERM",
                        _ = interrupt.recv() => "SIGINT",
                    };
                    tracing::info!(
                        signal,
                        "shutdown signal received; draining in-flight requests"
                    );
                    shutdown.notify_one();
                });
            }
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(
                    %error,
                    "failed to install SIGTERM/SIGINT handlers; the process will terminate without draining"
                );
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::spawn(async move {
            match tokio::signal::ctrl_c().await {
                Ok(()) => {
                    tracing::info!(
                        signal = "SIGINT",
                        "shutdown signal received; draining in-flight requests"
                    );
                    shutdown.notify_one();
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "failed to wait for Ctrl-C; the process will terminate without draining"
                    );
                }
            }
        });
    }
}

/// Poll the configured certificate/key files. The first tick is immediate and
/// performs one unconditional reload, covering a rotation that landed during
/// startup or database bootstrap. Later ticks reload only when the file
/// metadata changed.
fn spawn_periodic_reloader(reloader: Arc<TlsReloader>, interval_seconds: u64) {
    tokio::spawn(async move {
        let Some(target) = reloader.target.as_ref() else {
            return;
        };
        // Capture the stamps before the priming reload. Any write after this
        // point changes the stamp and triggers another reload, so a change
        // racing the priming read cannot be missed.
        let mut last_seen = pair_stamp(&target.cert_path, &target.key_path);
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_seconds));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        reloader.reload("startup-check").await;
        loop {
            ticker.tick().await;
            let current = pair_stamp(&target.cert_path, &target.key_path);
            if current == last_seen {
                continue;
            }
            // Record the observed files even when loading fails, so an
            // unchanged invalid pair produces one clear error instead of one
            // per tick. The next write changes the stamp and retries.
            last_seen = current;
            reloader.reload("interval").await;
        }
    });
}

/// Metadata used to notice replaced certificate/key files without re-reading
/// unchanged files on every timer tick.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

fn pair_stamp(cert_path: &Path, key_path: &Path) -> (Option<FileStamp>, Option<FileStamp>) {
    (file_stamp(cert_path), file_stamp(key_path))
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// Validate and install a replacement pair. On failure the previous pair stays
/// active. Errors mention the configured paths but never key material.
async fn reload_tls(
    handle: &axum_server::tls_rustls::RustlsConfig,
    cert_path: &Path,
    key_path: &Path,
    trigger: &'static str,
) {
    let cert_path_owned = cert_path.to_path_buf();
    let key_path_owned = key_path.to_path_buf();
    let loaded = tokio::task::spawn_blocking(move || {
        microwave_tls::load_paths(&cert_path_owned, &key_path_owned)
    })
    .await;

    match loaded {
        Ok(Ok(loaded)) => {
            handle.reload_from_config(loaded.config);
            tracing::info!(
                trigger,
                cert = %cert_path.display(),
                key = %key_path.display(),
                "reloaded TLS certificate and key"
            );
        }
        Ok(Err(error)) => {
            tracing::error!(
                trigger,
                cert = %cert_path.display(),
                key = %key_path.display(),
                %error,
                "TLS certificate reload failed; keeping the previous certificate"
            );
        }
        Err(error) => {
            tracing::error!(
                trigger,
                cert = %cert_path.display(),
                key = %key_path.display(),
                %error,
                "TLS certificate reload task failed; keeping the previous certificate"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        local_object_store_base_url, redact_config_for_output, redact_database_url,
        spawn_gauge_refresh, warn_implicit_mcp_superuser, PoolGauges,
    };
    use microwave_api::AppState;
    use microwave_config::{Config, McpOrgConfig};
    use microwave_metrics::Metrics;
    use microwave_objectstore::{LocalObjectStore, ObjectStore};
    use microwave_store::{NewNode, SqliteStore};
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct LogCapture(Arc<Mutex<Vec<u8>>>);

    impl Write for LogCapture {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for LogCapture {
        type Writer = LogCapture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// The startup warning is the only signal that a write-enabled organization
    /// still runs as the ACL-bypassing superuser. Pin its trigger conditions.
    #[test]
    fn warns_only_for_write_enabled_implicit_superuser_organizations() {
        let capture = LogCapture::default();
        let writer = capture.clone();
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .try_init()
            .ok();

        let mut config = Config::default();
        config.mcp.enabled = true;
        let superuser = config.auth.superuser_name.clone();
        config
            .mcp
            .organizations
            .insert("implicit".into(), McpOrgConfig::default());
        config.mcp.organizations.insert(
            superuser.clone(),
            McpOrgConfig {
                principal: Some(superuser),
                ..Default::default()
            },
        );
        config.mcp.organizations.insert(
            "explicit".into(),
            McpOrgConfig {
                principal: Some("agent".into()),
                ..Default::default()
            },
        );
        config.mcp.organizations.insert(
            "read-only".into(),
            McpOrgConfig {
                writes: false,
                ..Default::default()
            },
        );

        warn_implicit_mcp_superuser(&config);
        let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("organization=implicit"), "logs: {logs}");
        assert!(logs.contains("organization=pivotal"), "logs: {logs}");
        assert!(!logs.contains("organization=explicit"), "logs: {logs}");
        assert!(!logs.contains("organization=read-only"), "logs: {logs}");

        let mut disabled = Config::default();
        disabled
            .mcp
            .organizations
            .insert("disabled".into(), McpOrgConfig::default());
        capture.0.lock().unwrap().clear();
        warn_implicit_mcp_superuser(&disabled);
        assert!(capture.0.lock().unwrap().is_empty());
    }

    /// The interval is set to an hour, so only the immediate first tick can
    /// fill the gauges; a regression to a delayed first tick would time out.
    #[tokio::test]
    async fn gauge_refresh_populates_every_gauge_on_the_first_tick() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(&dir.path().join("test.db")).unwrap());
        let org = store.create_org("gauge-org", "Gauge Org").unwrap();
        let creator = store.create_actor().unwrap();
        store
            .create_container_for_org_with_acl(&org.id, "nodes", &creator)
            .unwrap();
        store
            .create_node(
                &org.id,
                NewNode {
                    name: "node-1",
                    body: r#"{"name":"node-1"}"#,
                    environment: "_default",
                    policy_name: None,
                    policy_group: None,
                    creator_authz_id: &creator,
                },
            )
            .unwrap();
        store.set_maintenance_enabled(true).unwrap();

        let config = Arc::new(Config::default());
        let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
            dir.path().join("objects"),
            "secret",
            "http://0.0.0.0:8000",
        ));
        let metrics = Arc::new(Metrics::new());
        let state = AppState::with_metrics(store, config, object_store, metrics.clone()).unwrap();

        let gauge_refresh = spawn_gauge_refresh(state.clone(), metrics.clone(), 3600);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if metrics.maintenance()
                    && metrics.nodes() == 1
                    && metrics.keygen().capacity > 0
                    && !metrics.dependencies().is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the refresh task must populate the gauges on its first tick");
        gauge_refresh.abort();
        let _ = gauge_refresh.await;

        assert_eq!(metrics.nodes(), 1);
        assert!(metrics.maintenance());
        assert_eq!(metrics.keygen().capacity, state.config.keygen.size);
        assert!(!metrics.dependencies().is_empty());
        assert_eq!(
            metrics.pool(),
            state.store.pool_stats().await.map(|pool| PoolGauges {
                idle: pool.idle,
                in_use: pool.in_use,
                max: pool.max,
            })
        );
    }

    #[test]
    fn local_object_store_uses_https_only_when_tls_is_enabled() {
        let plain = Config::default();
        assert_eq!(local_object_store_base_url(&plain), "http://0.0.0.0:8000");

        let mut tls = Config::default();
        tls.server.tls.cert_path = Some("/etc/microwave/tls.crt".into());
        tls.server.tls.key_path = Some("/etc/microwave/tls.key".into());
        assert_eq!(local_object_store_base_url(&tls), "https://0.0.0.0:8000");

        let mut disabled = tls;
        disabled.server.tls.enabled = Some(false);
        assert_eq!(
            local_object_store_base_url(&disabled),
            "http://0.0.0.0:8000"
        );
    }

    #[test]
    fn local_object_store_prefers_the_public_url() {
        let mut config = Config::default();
        config.server.public_url = Some("https://microwave.example.com".into());
        assert_eq!(
            local_object_store_base_url(&config),
            "https://microwave.example.com"
        );

        // A trailing slash would produce `//bookshelf` in the presigned path.
        config.server.public_url = Some("https://microwave.example.com/".into());
        assert_eq!(
            local_object_store_base_url(&config),
            "https://microwave.example.com"
        );
    }

    #[test]
    fn redacts_database_data_collector_and_object_store_credentials() {
        let mut config = Config::default();
        config.database.url =
            Some("postgres://microwave:s3cr3t@db.example.com/microwave".to_string());
        config.data_collector.token = Some("collector-token".to_string());
        config.object_store.secret = Some("bookshelf-secret".to_string());
        config.object_store.access_key = Some("access-key".to_string());
        config.object_store.secret_key = Some("secret-key".to_string());

        redact_config_for_output(&mut config);

        assert_eq!(
            config.database.url.as_deref(),
            Some("postgres://microwave:***@db.example.com/microwave")
        );
        assert_eq!(config.data_collector.token.as_deref(), Some("***"));
        assert_eq!(config.object_store.secret.as_deref(), Some("***"));
        assert_eq!(config.object_store.access_key.as_deref(), Some("***"));
        assert_eq!(config.object_store.secret_key.as_deref(), Some("***"));
    }

    #[test]
    fn redacts_otel_header_values_and_keeps_names() {
        let mut config = Config::default();
        config.otel.headers = vec![
            "Authorization: Bearer s3cr3t".to_string(),
            "X-Scope:".to_string(),
            "malformed".to_string(),
        ];

        redact_config_for_output(&mut config);

        assert_eq!(
            config.otel.headers,
            vec!["Authorization: ***", "X-Scope: ***", "malformed"]
        );
    }

    #[test]
    fn redacts_the_password_from_a_database_url() {
        assert_eq!(
            redact_database_url("postgres://microwave:s3cr3t@db.example.com/microwave"),
            "postgres://microwave:***@db.example.com/microwave"
        );
        assert_eq!(
            redact_database_url("postgresql://user:p:a@ss@db/microwave"),
            "postgresql://user:***@db/microwave"
        );
    }

    #[test]
    fn leaves_credential_free_urls_unchanged() {
        assert_eq!(
            redact_database_url("sqlite:///var/lib/microwave/microwave.db"),
            "sqlite:///var/lib/microwave/microwave.db"
        );
        assert_eq!(
            redact_database_url("postgres://db.example.com/microwave"),
            "postgres://db.example.com/microwave"
        );
    }
}
