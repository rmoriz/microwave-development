//! Chef-compatible request authentication primitives.

pub mod auth;
pub mod error;
pub mod keygen;
pub mod password;
pub mod signature;

pub use auth::{
    canonical_path, canonical_request, canonical_request_with_content_hash, content_hash,
    parse_sign_header, CanonicalRequest, SignVersion,
};
pub use error::CryptoError;
pub use keygen::{KeyPair, KeygenCache, KeygenStatus};
pub use password::{hash_password, needs_upgrade, verify_password, PasswordHash};
pub use signature::{
    decode_signature, generate_rsa_keypair, parse_private_key_pem, parse_public_key_pem,
    public_key_fingerprint, sign_request, verify_request, verify_request_with_content_hash,
    AuthHeaders,
};
