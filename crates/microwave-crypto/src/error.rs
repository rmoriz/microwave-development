use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("unsupported sign version: {0}")]
    UnsupportedSignVersion(String),
    #[error("malformed X-Ops-Sign header: {0}")]
    MalformedSignHeader(String),
    #[error("algorithm {algorithm} is not valid for sign version {version}")]
    AlgorithmVersionMismatch { algorithm: String, version: String },
    #[error("invalid X-Ops-Timestamp: {0}")]
    InvalidTimestamp(String),
    #[error("invalid base64 signature: {0}")]
    InvalidBase64(#[from] base64::DecodeError),
    #[error("invalid RSA key: {0}")]
    InvalidKey(String),
    #[error("invalid RSA signature length: {0}")]
    InvalidSignatureLength(usize),
    #[error("password must not be empty")]
    InvalidPassword,
    #[error("password hashing failed: {0}")]
    PasswordHash(String),
    #[error("signature verification failed")]
    VerificationFailed,
}
