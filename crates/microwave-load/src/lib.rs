//! Load-test harness for the converge path.
//!
//! The binary has two halves: `seed` builds a deterministic fixture, generates
//! the actor keys and applies the fixture through `StoreSnapshot`, and `run`
//! converges every target node over signed HTTP. This module owns the CLI
//! dispatch and the seed pipeline; the HTTP driver lives in later modules.

mod cli;
pub mod client;
mod fixture;
pub mod keys;
pub mod metrics;
pub mod retry;
pub mod sequence;

pub use cli::{Cli, Command, RunArgs, SeedArgs};
pub use client::{NodeClient, Outcome};
pub use fixture::{BookBlob, Fixture, KeyEntry, KeyKind, Sizes, ORG_ID, ORG_NAME, SUPERUSER};
pub use keys::ActorKey;
pub use metrics::Metrics;
pub use retry::{RetryCount, SolverOverloaded};
pub use sequence::converge;

use clap::Parser;
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{Compression, IdentityRepository, SqliteStore};
use std::path::Path;
use thiserror::Error;

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
    #[error("transport after {retries} retries ({solver_503} solver 503s): {source}")]
    Transport {
        retries: RetryCount,
        solver_503: SolverOverloaded,
        #[source]
        source: reqwest::Error,
    },
}

impl LoadError {
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

/// Parse process arguments and dispatch.
pub fn run() -> Result<(), LoadError> {
    dispatch(Cli::parse())
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

/// Read the manifest, build the one run client, converge every target and print
/// the endpoint table.
///
/// A client entry names its node: the fixture pairs `load-client-NNNNN` with
/// `load-node-NNNNN`, and canary entries are left to a real `chef-client`.
fn run_load(args: RunArgs) -> Result<(), LoadError> {
    let manifest = keys::read_manifest(&args.keys)?;
    let superuser = manifest
        .iter()
        .find(|entry| entry.kind == KeyKind::Superuser)
        .cloned();
    if args.superuser && superuser.is_none() {
        return Err(LoadError::Message(format!(
            "manifest {} has no superuser entry required by --superuser",
            args.keys.display()
        )));
    }

    let concurrency = args.concurrency.max(1);
    let http = client::run_http(concurrency)?;
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
        return Err(LoadError::Message(format!(
            "manifest {} has no client entries",
            args.keys.display()
        )));
    }

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

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (mut metrics, pool_samples) = runtime.block_on(async {
        let poller = metrics::StatsPoller::start(http, stats_url, credentials);
        let metrics = sequence::run_all(&config, targets).await;
        let pool_samples = poller.finish();
        (metrics, pool_samples)
    });
    metrics.pool_samples = pool_samples;

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
    Ok(())
}
