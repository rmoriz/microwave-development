//! PostgreSQL metadata store for Microwave Server.
//!
//! This crate implements the backend-neutral repository traits from
//! `microwave-store` (`IdentityRepository`, `AuthzRepository`,
//! `ObjectRepository`, `SandboxRepository`, `CookbookRepository`,
//! `ArtifactRepository`, `PolicyRepository`, `MembershipRepository`,
//! `OAuthRepository`, `KvRepository`, `SecretsRepository` and
//! `ReportRepository`) against the
//! unified PostgreSQL schema in `migrations/0001_initial.sql`. `PostgresStore`
//! therefore satisfies [`MetadataStore`](microwave_store::MetadataStore).
//!
//! ```no_run
//! use microwave_store::IdentityRepository;
//! use microwave_store_postgres::PostgresStore;
//!
//! # async fn example() -> Result<(), microwave_store::StoreError> {
//! let store = PostgresStore::connect("postgres://microwave:microwave@127.0.0.1/microwave").await?;
//! let orgs = store.list_orgs().await?;
//! # Ok(())
//! # }
//! ```
//!
//! The server selects this backend through `database.driver = "postgres"`; see
//! `README.md` for the runtime wiring and deferred operational work.

mod artifacts;
mod authz;
mod cookbooks;
mod health;
mod identity;
mod kv;
mod membership;
mod migration;
mod oauth;
mod objects;
mod policies;
mod reports;
mod sandboxes;
mod secrets;
mod store;

pub use store::PostgresStore;
