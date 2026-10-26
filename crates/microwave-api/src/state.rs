use crate::health::HealthCache;
use crate::metrics::Metrics;
use crate::search::{build_search_backend, SearchBackend, SearchError};
use microwave_config::Config;
use microwave_crypto::{CryptoError, KeygenCache};
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
    /// Process-local request metrics served by `GET /_stats`.
    pub metrics: Arc<Metrics>,
    /// Pre-generated 2048-bit RSA keypairs used by API key creation paths.
    pub keygen: Arc<KeygenCache>,
    /// Short-lived dependency probe results served by `GET /_status`.
    pub(crate) health: Arc<HealthCache>,
    /// Shared connection pool for outbound compatibility forwards. Per-request
    /// deadlines are applied by each caller so the pool can be reused safely.
    pub(crate) http_client: reqwest::Client,
    /// Client used for compliance-proxy forwards. Kept separate because it may
    /// deliberately skip TLS verification for a private/test upstream.
    pub(crate) compliance_client: reqwest::Client,
    /// Opt-in data collector mutation notifications.
    pub(crate) actions: Arc<crate::actions::ActionDispatcher>,
    /// Bounds concurrent dependency solver runs. Acquired without waiting so
    /// excess requests fail fast with 503.
    pub(crate) depsolver: Arc<tokio::sync::Semaphore>,
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
        let compliance_client = build_compliance_client(&config)?;
        let keygen = build_keygen_cache(&config);
        let actions =
            crate::actions::ActionDispatcher::new(&config, http_client.clone(), store.clone());
        let depsolver = build_depsolver_semaphore(&config);
        Ok(Self {
            store,
            sqlite: None,
            config,
            object_store,
            search,
            metrics: Arc::new(Metrics::new()),
            keygen,
            actions,
            health: Arc::new(HealthCache::new()),
            http_client,
            compliance_client,
            depsolver,
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
        let compliance_client = build_compliance_client(&config)?;
        let keygen = build_keygen_cache(&config);
        let actions =
            crate::actions::ActionDispatcher::new(&config, http_client.clone(), store.clone());
        let depsolver = build_depsolver_semaphore(&config);
        Ok(Self {
            store: store.clone(),
            sqlite: Some(store),
            actions,
            config,
            object_store,
            search,
            metrics: Arc::new(Metrics::new()),
            keygen,
            health: Arc::new(HealthCache::new()),
            http_client,
            compliance_client,
            depsolver,
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

    /// Produce a fresh 2048-bit RSA keypair for an API response.
    ///
    /// A cached keypair is returned immediately. On a cache miss, generation
    /// runs on Tokio's blocking pool so the async runtime is never stalled by
    /// RSA key generation.
    pub async fn generate_keypair(&self) -> Result<(String, String), CryptoError> {
        if let Some(pair) = self.keygen.try_take() {
            return Ok(pair);
        }
        let cache = self.keygen.clone();
        tokio::task::spawn_blocking(move || cache.take())
            .await
            .map_err(|_| CryptoError::InvalidKey("key generation task failed".into()))?
    }

    /// Try to reserve one dependency-solver slot without waiting.
    ///
    /// Returns `None` when every configured slot is busy. Exposed so
    /// integration tests can simulate an overloaded solver.
    #[doc(hidden)]
    pub fn try_acquire_depsolver(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.depsolver.clone().try_acquire_owned().ok()
    }
}

fn build_depsolver_semaphore(config: &Config) -> Arc<tokio::sync::Semaphore> {
    // Configuration validation rejects zero, but tests may build a `Config`
    // directly; clamping keeps such states usable instead of rejecting every
    // solve.
    Arc::new(tokio::sync::Semaphore::new(
        config.depsolver.max_concurrent.max(1),
    ))
}

fn build_keygen_cache(config: &Config) -> Arc<KeygenCache> {
    let cache = KeygenCache::new(config.keygen.size, config.keygen.workers);
    if config.keygen.warm_up {
        cache.warm_up();
    }
    Arc::new(cache)
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

fn build_compliance_client(config: &Config) -> Result<reqwest::Client, SearchError> {
    let request_timeout = Duration::from_millis(config.compliance_proxy.timeout.max(1));
    let mut builder = reqwest::Client::builder()
        .connect_timeout(request_timeout.min(Duration::from_secs(10)))
        .read_timeout(request_timeout)
        .redirect(reqwest::redirect::Policy::none());
    if config.compliance_proxy.insecure_skip_tls_verify {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder.build().map_err(|error| {
        SearchError::Configuration(format!("failed to build compliance HTTP client: {error}"))
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
    /// Whether the principal is the configured platform superuser.
    pub is_superuser: bool,
}

impl microwave_oauth::ProvidesOAuthState for AppState {
    fn oauth_state(&self) -> microwave_oauth::OAuthState {
        microwave_oauth::OAuthState {
            store: self.store.clone(),
            config: self.config.clone(),
            keygen: self.keygen.clone(),
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
