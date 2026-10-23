use crate::error::CryptoError;
use sha1::{Digest, Sha1};
use subtle::ConstantTimeEq;

const BCRYPT_COST: u32 = 12;

/// Password hash data in the form stored by the upstream server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordHash {
    pub hash: String,
    pub salt: String,
    pub hash_type: String,
}

/// Hash a password with bcrypt at the default cost.
pub fn hash_password(password: &str) -> Result<PasswordHash, CryptoError> {
    if password.is_empty() {
        return Err(CryptoError::InvalidPassword);
    }

    let hash = bcrypt::hash(password, BCRYPT_COST)
        .map_err(|error| CryptoError::PasswordHash(error.to_string()))?;
    let salt = hash.get(..29).unwrap_or("").to_string();

    Ok(PasswordHash {
        hash,
        salt,
        hash_type: "bcrypt".into(),
    })
}

/// Verify a password against any supported upstream hash format.
pub fn verify_password(password: &str, stored: &PasswordHash) -> bool {
    match stored.hash_type.as_str() {
        "bcrypt" | "erlang-bcrypt-0.5.0" => bcrypt::verify(password, &stored.hash).unwrap_or(false),
        "SHA1-bcrypt" => {
            let interim = sha1_hex(&format!("{}--{}--", stored.salt, password));
            bcrypt::verify(&interim, &stored.hash).unwrap_or(false)
        }
        "sha1+bcrypt" => {
            let Some((_bcrypt_salt, original_salt)) = stored.salt.split_once('\t') else {
                return false;
            };
            let interim = sha1_hex(&format!("--{}--{}--", original_salt, password));
            bcrypt::verify(&interim, &stored.hash).unwrap_or(false)
        }
        "ec" | "sha1" | "plain" => constant_time_eq(
            &sha1_hex(&format!("{}--{}--", stored.salt, password)),
            &stored.hash,
        ),
        "osc" => constant_time_eq(
            &sha1_hex(&format!("--{}--{}--", stored.salt, password)),
            &stored.hash,
        ),
        _ => false,
    }
}

/// Whether the stored hash should be replaced with a fresh bcrypt hash after login.
pub fn needs_upgrade(stored: &PasswordHash) -> bool {
    stored.hash_type != "bcrypt"
}

fn sha1_hex(input: &str) -> String {
    hex::encode(Sha1::digest(input.as_bytes()))
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}
