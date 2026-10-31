//! Load-test harness for the converge path.
//!
//! The binary has two halves: `seed` builds a deterministic fixture, generates
//! the actor keys and applies the fixture through `StoreSnapshot`, and `run`
//! converges every target node over signed HTTP. This module owns the CLI
//! dispatch and the seed pipeline; the HTTP driver lives in later modules.

mod cli;
mod fixture;
pub mod keys;

pub use cli::{Cli, Command, RunArgs, SeedArgs};
pub use fixture::{BookBlob, Fixture, KeyEntry, KeyKind, Sizes, ORG_ID, ORG_NAME, SUPERUSER};
pub use keys::ActorKey;

use clap::Parser;
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{Compression, SqliteStore};
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

/// Build the fixture, write keys and blobs, and apply the snapshot.
pub fn seed(args: SeedArgs) -> Result<(), LoadError> {
    let sizes = Sizes {
        nodes: args.nodes,
        roles: args.roles,
        versions: args.versions,
        targets: args.targets,
        canary: args.canary,
    };
    let mut fixture = Fixture::build(sizes).map_err(LoadError::Message)?;
    let actors = keys::generate(&mut fixture, args.shared_key)?;
    keys::write_manifest(&args.keys, &actors)?;

    let books = std::mem::take(&mut fixture.books);
    let store = LocalObjectStore::new(args.data_dir.join("bookshelf"), "load", "");
    for book in &books {
        store.put(ORG_ID, &book.checksum, &book.data)?;
    }

    match &args.postgres_url {
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
                store.import_snapshot(&fixture.snapshot).await
            })?;
        }
        None => {
            let database = args.data_dir.join("microwave.db");
            let store = SqliteStore::open_with_compression(&database, Compression::None)?;
            store.import_snapshot(&fixture.snapshot)?;
        }
    }

    println!(
        "seeded org {ORG_NAME} into {}: {} nodes, {} roles, {} cookbook versions, {} clients, {} canaries",
        target(&args),
        fixture.sizes.nodes,
        fixture.sizes.roles,
        fixture.sizes.versions,
        fixture.sizes.targets,
        fixture.sizes.canary
    );
    println!("{} keypairs in {}", actors.len(), args.keys.display());
    Ok(())
}

fn target(args: &SeedArgs) -> String {
    match &args.postgres_url {
        Some(url) => format!("postgres {url}"),
        None => args.data_dir.join("microwave.db").display().to_string(),
    }
}

/// Placeholder for the HTTP driver: prints the resolved configuration.
fn run_load(args: RunArgs) -> Result<(), LoadError> {
    println!("server: {}", args.server_url);
    println!("manifest: {}", args.keys.display());
    println!("concurrency: {}", args.concurrency);
    println!("canary: {}", args.canary);
    println!("report: {}", args.out.display());
    println!("run is not implemented yet");
    Ok(())
}
