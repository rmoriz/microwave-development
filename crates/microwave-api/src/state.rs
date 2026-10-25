use crate::search::{build_search_backend, SearchBackend, SearchError};
use microwave_config::Config;
use microwave_objectstore::ObjectStore;
use microwave_store::{MetadataStore, Org, Principal, SqliteStore};
use std::sync::Arc;
use std::time::Duration;

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
    /// Shared connection pool for outbound compatibility forwards. Per-request
    /// deadlines are applied by each caller so the pool can be reused safely.
    pub(crate) http_client: reqwest::Client,
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
        let http_client = build_http_client(&config)?;
        Ok(Self {
            store,
            sqlite: None,
            config,
            object_store,
            search,
            http_client,
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
        let http_client = build_http_client(&config)?;
        Ok(Self {
            store: store.clone(),
            sqlite: Some(store),
            config,
            object_store,
            search,
            http_client,
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

fn build_http_client(config: &Config) -> Result<reqwest::Client, SearchError> {
    let request_timeout = Duration::from_millis(config.data_collector.timeout.max(1));
    reqwest::Client::builder()
        .connect_timeout(request_timeout.min(Duration::from_secs(10)))
        // `read_timeout` is what aborts a stalled response body after headers
        // have arrived; the total deadline is applied per request.
        .read_timeout(request_timeout)
        // Compatibility forwards preserve upstream redirects rather than
        // following them, matching nginx's proxy behavior.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            SearchError::Configuration(format!("failed to build shared HTTP client: {error}"))
        })
}

#[derive(Debug, Clone, Copy)]
pub struct ApiVersion(pub u8);

#[derive(Clone)]
pub struct AuthContext {
    pub principal: Principal,
    pub org: Option<Org>,
    /// Name of the actor key that verified the request signature, when known.
    pub key_name: Option<String>,
}

impl microwave_oauth::ProvidesOAuthState for AppState {
    fn oauth_state(&self) -> microwave_oauth::OAuthState {
        microwave_oauth::OAuthState {
            store: self.store.clone(),
            config: self.config.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build_http_client;
    use microwave_config::Config;

    #[test]
    fn builds_a_shared_http_client_from_defaults() {
        assert!(build_http_client(&Config::default()).is_ok());
    }
}
