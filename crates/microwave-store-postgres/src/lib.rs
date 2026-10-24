//! PostgreSQL metadata store for Microwave Server.
//!
//! This crate is the first slice of the HA metadata backend described in the
//! design document. It implements six of the backend-neutral repository traits
//! from `microwave-store` (`IdentityRepository`, `AuthzRepository`,
//! `ObjectRepository`, `SandboxRepository`, `CookbookRepository` and
//! `PolicyRepository`) against the unified PostgreSQL schema in
//! `migrations/0001_initial.sql`.
//!
//! `PostgresStore` does not yet satisfy `MetadataStore`: the contract also
//! requires `ArtifactRepository`, `MembershipRepository`, `OAuthRepository`,
//! `KvRepository` and `SecretsRepository`, which currently exist only for the
//! SQLite backend.
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
//! See `README.md` for the remaining `AppState` integration work.

mod authz;
mod cookbooks;
mod identity;
mod objects;
mod policies;
mod sandboxes;
mod store;

pub use store::PostgresStore;
