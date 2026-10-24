use crate::search::{build_search_backend, SearchBackend, SearchError};
use microwave_config::Config;
use microwave_objectstore::LocalObjectStore;
use microwave_store::{Org, Principal, SqliteStore};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<SqliteStore>,
    pub config: Arc<Config>,
    pub object_store: Arc<LocalObjectStore>,
    /// Configured search backend (embedded by default, or OpenSearch /
    /// Elasticsearch when `search.backend` selects one).
    pub search: Arc<dyn SearchBackend>,
}

impl AppState {
    /// Builds application state and selects the configured search backend.
    pub fn new(
        store: Arc<SqliteStore>,
        config: Arc<Config>,
        object_store: Arc<LocalObjectStore>,
    ) -> Result<Self, SearchError> {
        let search = build_search_backend(&config.search, store.clone())?;
        Ok(Self {
            store,
            config,
            object_store,
            search,
        })
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
