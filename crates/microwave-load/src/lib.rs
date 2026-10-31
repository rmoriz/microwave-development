//! Load-test harness for the converge path.
//!
//! The binary has two halves: `seed` builds a deterministic fixture, generates
//! the actor keys and applies the fixture through `StoreSnapshot`, and `run`
//! converges every target node over signed HTTP. This module owns the CLI
//! dispatch and the seed pipeline; the HTTP driver lives in later modules.

mod cli;
pub mod client;
pub mod fingerprint;
mod fixture;
pub mod keys;
pub mod metrics;
pub mod report;
pub mod retry;
pub mod sequence;

pub use cli::{Cli, Command, RunArgs, SeedArgs};
pub use client::{NodeClient, Outcome};
pub use fingerprint::Fingerprint;
pub use fixture::{BookBlob, Fixture, KeyEntry, KeyKind, Sizes, ORG_ID, ORG_NAME, SUPERUSER};
pub use keys::ActorKey;
pub use metrics::Metrics;
pub use retry::{RetryCount, SolverOverloaded};
pub use sequence::converge;

use clap::Parser;
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{Compression, IdentityRepository, SqliteStore};
use std::path::Path;
use std::process::ExitCode;
use thiserror::Error;

/// Process exit status; diagnostic, never a gate.
///
/// [`Self::Completed`] also covers a run whose retries were exhausted: the
/// report says how many nodes did not complete instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitStatus {
    /// The run completed; `0`.
    Completed = 0,
    /// The server was unreachable at start or the fixture is missing; `2`.
    Unavailable = 2,
    /// The fingerprint or the report failed; `3`.
    Diagnostic = 3,
}

impl ExitStatus {
    pub fn code(self) -> ExitCode {
        ExitCode::from(self as u8)
    }
}

/// Everything a failed seed can report.
#[derive(Debug, Error)]
pub enum LoadError {
    #[error("{0}")]
    Message(String),
    #[error("store: {0}")]
    Store(#[from] microwave_store::StoreError),
    #[error("object store: {0}")]
    ObjectStore(#[from] microwave_objectstore::ObjectStoreError),
    #[error("crypto: {0}")]
    Crypto(#[from] microwave_crypto::CryptoError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The server was unreachable at start or the fixture is missing.
    #[error("{0}")]
    Fixture(String),
    /// Fingerprint or report failure.
    #[error("{0}")]
    Diagnostic(String),
    #[error("transport after {retries} retries ({solver_503} solver 503s): {source}")]
    Transport {
        retries: RetryCount,
        solver_503: SolverOverloaded,
        #[source]
        source: reqwest::Error,
    },
}

impl LoadError {
    /// The exit status this error maps to. Everything that is neither a
    /// fixture nor a diagnostic problem is setup trouble and exits `2`.
    pub fn status(&self) -> ExitStatus {
        match self {
            Self::Diagnostic(_) => ExitStatus::Diagnostic,
            Self::Fixture(_)
            | Self::Message(_)
            | Self::Store(_)
            | Self::ObjectStore(_)
            | Self::Crypto(_)
            | Self::Io(_)
            | Self::Json(_)
            | Self::Transport { .. } => ExitStatus::Unavailable,
        }
    }

    /// A transport error from an attempt that has counted nothing yet; the
    /// retry loop attaches its counters when the budget is gone.
    pub(crate) fn transport(source: reqwest::Error) -> Self {
        Self::Transport {
            retries: RetryCount::default(),
            solver_503: SolverOverloaded::default(),
            source,
        }
    }

    /// Carry the retry and solver counters into an exhausted transport error.
    pub(crate) fn counted(self, retries: RetryCount, solver_503: SolverOverloaded) -> Self {
        match self {
            Self::Transport { source, .. } => Self::Transport {
                retries,
                solver_503,
                source,
            },
            other => other,
        }
    }
}

/// Parse process arguments, dispatch, and turn a failure into its exit status.
pub fn run() -> ExitStatus {
    match dispatch(Cli::parse()) {
        Ok(()) => ExitStatus::Completed,
        Err(error) => {
            eprintln!("error: {error}");
            error.status()
        }
    }
}

/// Execute an already-parsed command.
pub fn dispatch(cli: Cli) -> Result<(), LoadError> {
    match cli.command {
        Command::Seed(args) => seed(args),
        Command::Run(args) => run_load(args),
    }
}

/// Open the store, refuse a populated one, then build the fixture, write keys
/// and blobs, and apply the snapshot.
///
/// The fixture keeps fixed ids, so the target store must be empty; a populated
/// SQLite file would collide on the global `UNIQUE (org_id, name)` container
/// rows and silently rebind existing cookbooks. The emptiness check therefore
/// runs before the fixture build and keygen: a doomed re-seed must not spend
/// minutes on keys and overwrite the `--keys` manifest and its PEM files while
/// the surviving database keeps the previous seed's public keys.
pub fn seed(args: SeedArgs) -> Result<(), LoadError> {
    let sizes = Sizes {
        nodes: args.nodes,
        roles: args.roles,
        versions: args.versions,
        targets: args.targets,
        canary: args.canary,
    };

    let actors = match &args.postgres_url {
        Some(url) => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let store =
                    microwave_store_postgres::PostgresStore::connect_with_compression_and_pool(
                        url,
                        Compression::None,
                        2,
                    )
                    .await?;
                require_empty(store.list_orgs().await?.len(), "--postgres-url")?;
                let (fixture, books, actors) = prepare(&args, sizes)?;
                write_books(&args.data_dir, &books)?;
                store.import_snapshot(&fixture.snapshot).await?;
                for (id, constraints) in constrained(&fixture) {
                    store
                        .set_environment_constraints(&id, Some(constraints))
                        .await?;
                }
                Ok::<_, LoadError>(actors)
            })?
        }
        None => {
            let database = args.data_dir.join("microwave.db");
            let store = SqliteStore::open_with_compression(&database, Compression::None)?;
            require_empty(store.list_orgs()?.len(), "--data-dir")?;
            let (fixture, books, actors) = prepare(&args, sizes)?;
            write_books(&args.data_dir, &books)?;
            store.import_snapshot(&fixture.snapshot)?;
            for (id, constraints) in constrained(&fixture) {
                store.set_environment_constraints(&id, Some(constraints))?;
            }
            actors
        }
    };

    println!(
        "seeded org {ORG_NAME} for {} into {}: {} nodes, {} roles, {} cookbook versions, {} clients, {} canaries",
        args.server_url,
        target(&args),
        sizes.nodes,
        sizes.roles,
        sizes.versions,
        sizes.targets,
        sizes.canary
    );
    println!("{actors} keypairs in {}", args.keys.display());
    Ok(())
}

/// Build the fixture, generate the actor keys and write the manifest.
///
/// Returns the fixture, its blobs and the keypair count. Only called once the
/// store proved empty, because it writes into `--keys` and `keys.d/`.
fn prepare(args: &SeedArgs, sizes: Sizes) -> Result<(Fixture, Vec<BookBlob>, usize), LoadError> {
    let mut fixture = Fixture::build(sizes).map_err(LoadError::Message)?;
    let actors = keys::generate(&mut fixture, args.shared_key)?;
    keys::write_manifest(&args.keys, &actors)?;
    let books = std::mem::take(&mut fixture.books);
    Ok((fixture, books, actors.len()))
}

/// Fail before the fixture build and keygen when the target store already has
/// data.
fn require_empty(orgs: usize, flag: &str) -> Result<(), LoadError> {
    if orgs == 0 {
        return Ok(());
    }
    Err(LoadError::Message(format!(
        "store already holds {orgs} organization(s); the fixture keeps fixed ids, so it needs a fresh \
         database — point --data-dir at a new SQLite file or --postgres-url at an empty PostgreSQL \
         database ({flag})"
    )))
}

/// Write every cookbook blob into `<data-dir>/bookshelf`.
fn write_books(data_dir: &Path, books: &[BookBlob]) -> Result<(), LoadError> {
    let store = LocalObjectStore::new(data_dir.join("bookshelf"), "load", "");
    for book in books {
        store.put(ORG_ID, &book.checksum, &book.data)?;
    }
    Ok(())
}

/// `(environment id, derived constraint JSON)` for every environment whose body
/// carries `cookbook_versions`.
///
/// `import_snapshot` writes no `cookbook_constraints`, so without this every
/// solve parses the environment body instead of reading the column.
fn constrained(fixture: &Fixture) -> Vec<(String, String)> {
    fixture
        .snapshot
        .environments
        .iter()
        .filter_map(|environment| {
            let constraints = microwave_store::environment_constraints(
                environment.serialized_object.as_deref()?,
            )?;
            Some((environment.id.clone(), constraints))
        })
        .collect()
}

fn target(args: &SeedArgs) -> String {
    match &args.postgres_url {
        Some(url) => format!("postgres {url}"),
        None => args.data_dir.join("microwave.db").display().to_string(),
    }
}

/// Read the manifest, fingerprint the machine, build the one run client,
/// converge every target and write the report.
///
/// A client entry names its node: the fixture pairs `load-client-NNNNN` with
/// `load-node-NNNNN`, and canary entries are left to a real `chef-client`.
fn run_load(args: RunArgs) -> Result<(), LoadError> {
    let concurrency = args.concurrency.max(1);
    let http = client::run_http(concurrency)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(reachable(&http, &args.server_url))?;

    let manifest = keys::read_manifest(&args.keys)
        .map_err(|error| LoadError::Fixture(format!("fixture {}: {error}", args.keys.display())))?;
    let superuser = manifest
        .iter()
        .find(|entry| entry.kind == KeyKind::Superuser)
        .cloned();
    if args.superuser && superuser.is_none() {
        return Err(LoadError::Fixture(format!(
            "manifest {} has no superuser entry required by --superuser",
            args.keys.display()
        )));
    }

    // Targets before the fingerprint: a broken fixture must fail as a fixture
    // (exit `2`), not as a diagnostic `3`.
    let mut targets = Vec::new();
    for entry in manifest
        .iter()
        .filter(|entry| entry.kind == KeyKind::Client)
    {
        let signer = superuser.as_ref().unwrap_or(entry);
        targets.push(sequence::Target {
            node: entry.name.replacen("load-client-", "load-node-", 1),
            client: NodeClient::new(
                http.clone(),
                args.server_url.clone(),
                signer.name.clone(),
                signer.private_key.clone(),
            ),
        });
    }
    if targets.is_empty() {
        return Err(LoadError::Fixture(format!(
            "manifest {} has no client entries",
            args.keys.display()
        )));
    }

    let mut fingerprint = fingerprint(&args, &runtime)?;

    let credentials = match (args.stats_user.clone(), args.stats_pass.clone()) {
        (Some(user), Some(pass)) => Some((user, pass)),
        _ => None,
    };
    let config = sequence::RunConfig {
        org: ORG_NAME.to_string(),
        upload: args.upload,
        superuser: args.superuser,
        concurrency,
        ramp: std::time::Duration::from_secs(args.ramp),
    };
    let stats_url = format!(
        "{}/_stats?format=json",
        args.server_url.trim_end_matches('/')
    );
    let nodes = targets.len();
    let config_json = serde_json::json!({
        "server_url": args.server_url,
        "nodes": nodes,
        "concurrency": concurrency,
        "ramp_seconds": args.ramp,
        "upload": args.upload,
        "superuser": args.superuser,
        "canary_requested": args.canary,
        "chef_client": args.chef_client,
        "commit": &fingerprint.server_commit,
    });

    let (mut metrics, pool_samples) = runtime.block_on(async {
        let poller = metrics::StatsPoller::start(http, stats_url, credentials);
        let metrics = sequence::run_all(&config, targets).await;
        let pool_samples = poller.finish();
        (metrics, pool_samples)
    });
    metrics.pool_samples = pool_samples;
    if let Some(size) = resolved_pool_size(&metrics.pool_samples) {
        fingerprint.pool_size = Some(size);
    }

    println!(
        "converged {nodes} nodes as {}",
        if args.superuser {
            "superuser"
        } else {
            "their clients"
        }
    );
    println!(
        "completed: {}, failed: {}",
        metrics.completed, metrics.failed
    );
    for failure in metrics.failures() {
        println!("failed: {failure}");
    }
    if metrics.failed as usize > metrics.failures().len() {
        println!(
            "… {} more failures not listed",
            metrics.failed as usize - metrics.failures().len()
        );
    }
    for (endpoint, summary) in metrics.summaries() {
        println!(
            "{endpoint}: n={} status={:?} p50={}us p95={}us p99={}us max={}us retries={} solver_503={}",
            summary.count,
            summary.statuses,
            summary.p50,
            summary.p95,
            summary.p99,
            summary.max,
            summary.retries,
            summary.solver_503
        );
    }
    println!("pool samples: {}", metrics.pool_samples.len());
    if metrics.failed > 0 {
        println!("{} nodes did not complete", metrics.failed);
    }

    let results = report::Results::new(
        nodes,
        metrics.completed,
        metrics.failed,
        metrics.failures().to_vec(),
        metrics.summaries(),
        std::mem::take(&mut metrics.pool_samples),
        metrics.superuser,
    );
    let path = report::write(&args.out, &fingerprint, &config_json, &results)
        .map_err(|error| LoadError::Diagnostic(format!("report: {error}")))?;
    println!("report: {}", path.display());
    Ok(())
}

/// Refuse to start against a server that does not answer its liveness probe.
///
/// Bounded: a blackholed host, or one that accepts the connection and never
/// answers, must end the run with exit `2` instead of hanging it.
async fn reachable(http: &reqwest::Client, server_url: &str) -> Result<(), LoadError> {
    let url = format!("{}/_status", server_url.trim_end_matches('/'));
    match http
        .get(&url)
        .timeout(client::PREFLIGHT_TIMEOUT)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(LoadError::Fixture(format!(
            "server {url} answered {}",
            response.status()
        ))),
        Err(error) => Err(LoadError::Fixture(format!(
            "server {url} unreachable: {error}"
        ))),
    }
}

/// Measure the machine and layer the wrapper file and the flags on top.
///
/// The wrapper file merges under the measured values; the `--commit` flag and
/// the poller's pool size go over them. Every failure here is a diagnostic
/// one: a report without a trustworthy fingerprint is not worth writing.
fn fingerprint(
    args: &RunArgs,
    runtime: &tokio::runtime::Runtime,
) -> Result<Fingerprint, LoadError> {
    std::fs::create_dir_all(&args.out).map_err(|error| {
        LoadError::Diagnostic(format!("output directory {}: {error}", args.out.display()))
    })?;
    let mut fingerprint = fingerprint::collect(&args.out);
    if let Some(path) = &args.fingerprint {
        let wrapper = fingerprint::read(path)
            .map_err(|error| LoadError::Diagnostic(format!("fingerprint: {error}")))?;
        fingerprint.merge_under(&wrapper);
    }
    if let Some(commit) = non_empty(&args.commit) {
        fingerprint.server_commit = Some(commit);
    }
    if let Some(url) = &args.postgres_url {
        runtime
            .block_on(fingerprint.measure_postgres(url))
            .map_err(|error| LoadError::Diagnostic(format!("fingerprint: {error}")))?;
    }
    Ok(fingerprint)
}

/// An optional flag value that is `None` when the flag was given empty.
fn non_empty(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The server's pool size as the last poller sample saw it.
fn resolved_pool_size(samples: &[serde_json::Value]) -> Option<u32> {
    samples
        .iter()
        .rev()
        .find_map(|sample| sample.get("max").and_then(serde_json::Value::as_u64))
        .and_then(|max| u32::try_from(max).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_size_is_the_last_reported_maximum() {
        let samples = vec![
            serde_json::json!({ "max": 10 }),
            serde_json::json!({ "max": 20 }),
            serde_json::json!({ "idle": 1 }),
        ];
        assert_eq!(resolved_pool_size(&samples), Some(20));
        assert_eq!(resolved_pool_size(&[]), None);
    }

    #[test]
    fn an_empty_commit_counts_as_absent() {
        assert_eq!(non_empty(&None), None);
        assert_eq!(non_empty(&Some(String::new())), None);
        assert_eq!(non_empty(&Some("abc1234".into())), Some("abc1234".into()));
    }

    #[test]
    fn diagnostic_errors_exit_three_and_setup_errors_exit_two() {
        assert_eq!(
            LoadError::Diagnostic("x".into()).status(),
            ExitStatus::Diagnostic
        );
        assert_eq!(
            LoadError::Fixture("x".into()).status(),
            ExitStatus::Unavailable
        );
        assert_eq!(
            LoadError::Message("x".into()).status(),
            ExitStatus::Unavailable
        );
    }
}
