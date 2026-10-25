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
    /// A wrapped command exited with this status. The binaries translate it
    /// into the process exit code without printing an additional error.
    #[error("command exited with status {0}")]
    Exit(u8),
    #[error("{0}")]
    Message(String),
}

impl CtlError {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    /// Convert a child process status into a [`CtlError::Exit`].
    pub fn exit(status: std::process::ExitStatus) -> Self {
        Self::Exit(status.code().unwrap_or(1).clamp(0, 255) as u8)
    }
}
