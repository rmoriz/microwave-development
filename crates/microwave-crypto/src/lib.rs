//! Chef-compatible request authentication primitives.

pub mod auth;
pub mod error;
pub mod password;
pub mod signature;

pub use auth::{
    canonical_path, canonical_request, content_hash, parse_sign_header, CanonicalRequest,
    SignVersion,
};
pub use error::CryptoError;
pub use password::{hash_password, needs_upgrade, verify_password, PasswordHash};
pub use signature::{
    decode_signature, parse_private_key_pem, parse_public_key_pem, sign_request, verify_request,
    AuthHeaders,
};
