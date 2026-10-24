use axum::extract::FromRef;
use microwave_config::Config;
use microwave_store::SqliteStore;
use std::sync::Arc;

/// Dependencies the identity surface needs from the host application.
#[derive(Clone)]
pub struct OAuthState {
    pub store: Arc<SqliteStore>,
    pub config: Arc<Config>,
}

/// Implemented by the host application state so the identity router can be
/// merged into the main router without a dependency cycle.
pub trait ProvidesOAuthState: Clone + Send + Sync + 'static {
    fn oauth_state(&self) -> OAuthState;
}

impl<S> FromRef<S> for OAuthState
where
    S: ProvidesOAuthState,
{
    fn from_ref(state: &S) -> Self {
        state.oauth_state()
    }
}
