use crate::OAuthError;
use axum::extract::FromRef;
use microwave_config::Config;
use microwave_crypto::KeygenCache;
use microwave_store::MetadataStore;
use std::sync::Arc;

/// Dependencies the identity surface needs from the host application.
#[derive(Clone)]
pub struct OAuthState {
    pub store: Arc<dyn MetadataStore>,
    pub config: Arc<Config>,
    /// Shared pre-generated keypair cache.
    pub keygen: Arc<KeygenCache>,
    /// Outbound mail surface for password-reset and invitation emails.
    pub mailer: Arc<dyn microwave_mail::Mailer>,
}

impl OAuthState {
    /// Produce a new RSA keypair, preferring the shared cache and moving an
    /// on-demand generation to Tokio's blocking pool.
    pub async fn generate_keypair(&self) -> Result<(String, String), OAuthError> {
        if let Some(pair) = self.keygen.try_take() {
            return Ok(pair);
        }
        let cache = self.keygen.clone();
        tokio::task::spawn_blocking(move || cache.take())
            .await
            .map_err(|_| OAuthError::internal())?
            .map_err(|_| OAuthError::internal())
    }
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
