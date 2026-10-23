//! Chef-compatible request authentication primitives.

pub mod auth;
pub mod error;

pub use auth::{
    canonical_path, canonical_request, content_hash, parse_sign_header, CanonicalRequest,
    SignVersion,
};
pub use error::CryptoError;
