use crate::search::{build_search_backend, SearchBackend, SearchError};
use microwave_config::Config;
use microwave_objectstore::ObjectStore;
use microwave_store::{MetadataStore, Org, Principal, SqliteStore};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    /// Backend-neutral metadata store. API handlers must only use this handle.
    pub store: Arc<dyn MetadataStore>,
    /// SQLite-only escape hatch for tests and SQLite-specific helpers. Always
    /// `None` when the server runs against PostgreSQL.
    #[doc(hidden)]
    pub sqlite: Option<Arc<SqliteStore>>,
    pub config: Arc<Config>,
    pub object_store: Arc<dyn ObjectStore + Send + Sync>,
    pub search: Arc<dyn SearchBackend>,
}

impl AppState {
    /// Build application state for any metadata backend and select the
    /// configured search backend.
    pub fn new(
        store: Arc<dyn MetadataStore>,
        config: Arc<Config>,
        object_store: Arc<dyn ObjectStore + Send + Sync>,
    ) -> Result<Self, SearchError> {
        let search = build_search_backend(&config.search, store.clone())?;
        Ok(Self {
            store,
            sqlite: None,
            config,
            object_store,
            search,
        })
    }

    /// Build application state around a SQLite store.
    ///
    /// SQLite remains the default backend and this constructor keeps the
    /// concrete handle available through [`AppState::sqlite`] for tests and
    /// migration-only helpers.
    pub fn from_sqlite(
        store: Arc<SqliteStore>,
        config: Arc<Config>,
        object_store: Arc<dyn ObjectStore + Send + Sync>,
    ) -> Result<Self, SearchError> {
        let search = build_search_backend(&config.search, store.clone())?;
        Ok(Self {
            store: store.clone(),
            sqlite: Some(store),
            config,
            object_store,
            search,
        })
    }

    /// Borrow the concrete SQLite store.
    ///
    /// # Panics
    ///
    /// Panics when the server is running against a non-SQLite backend.
    #[doc(hidden)]
    pub fn sqlite(&self) -> &SqliteStore {
        self.sqlite
            .as_deref()
            .expect("AppState::sqlite() called while running against a non-SQLite backend")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ApiVersion(pub u8);

#[derive(Clone)]
pub struct AuthContext {
    pub principal: Principal,
    pub org: Option<Org>,
}

impl microwave_oauth::ProvidesOAuthState for AppState {
    fn oauth_state(&self) -> microwave_oauth::OAuthState {
        microwave_oauth::OAuthState {
            store: self.store.clone(),
            config: self.config.clone(),
        }
    }
}
