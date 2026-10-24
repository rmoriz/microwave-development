use microwave_config::ConfigError;
use microwave_crypto::CryptoError;
use microwave_migrate::MigrateError;
use microwave_store::StoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CtlError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Migrate(#[from] MigrateError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
}

impl CtlError {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}
