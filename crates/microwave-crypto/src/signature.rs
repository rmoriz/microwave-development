use crate::auth::{canonical_request, content_hash, CanonicalRequest, SignVersion};
use crate::error::CryptoError;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey, EncodeRsaPublicKey};
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::pkcs8::{
    DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding,
};
use rsa::rand_core::OsRng;
use rsa::signature::{SignatureEncoding, Signer, Verifier};
use rsa::traits::{PrivateKeyParts, PublicKeyParts};
use rsa::{BigUint, RsaPrivateKey, RsaPublicKey};
use sha1::{Digest, Sha1};
use sha2::Sha256;

/// Parsed Chef authentication headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthHeaders {
    pub version: SignVersion,
    pub user_id: String,
    pub timestamp: String,
    pub content_hash: String,
    pub signature: Vec<u8>,
}

/// Generate a 2048-bit RSA keypair as `(private_pem, public_pem)`.
pub fn generate_rsa_keypair() -> Result<(String, String), CryptoError> {
    let mut rng = OsRng;
    let private_key = RsaPrivateKey::new(&mut rng, 2048)
        .map_err(|error| CryptoError::InvalidKey(error.to_string()))?;
    let private_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|error| CryptoError::InvalidKey(error.to_string()))?
        .to_string();
    let public_pem = private_key
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .map_err(|error| CryptoError::InvalidKey(error.to_string()))?;
    Ok((private_pem, public_pem))
}

/// Compute a Chef-compatible public key fingerprint.
///
/// The fingerprint is the lowercase SHA-1 digest of the PKCS#1 DER encoding of
/// the public key, written as colon-separated hex octets. Chef uses this value
/// as the default key name when a key is generated or imported without one.
pub fn public_key_fingerprint(public_key_pem: &str) -> Result<String, CryptoError> {
    let public_key = parse_public_key_pem(public_key_pem)?;
    let der = public_key
        .to_pkcs1_der()
        .map_err(|error| CryptoError::InvalidKey(error.to_string()))?;
    let digest = Sha1::digest(der.as_bytes());
    Ok(digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":"))
}

/// Decode the `X-Ops-Authorization-1..N` header values into signature bytes.
pub fn decode_signature(lines: &[&str]) -> Result<Vec<u8>, CryptoError> {
    let joined = lines.iter().map(|line| line.trim()).collect::<String>();
    STANDARD
        .decode(joined.as_bytes())
        .map_err(CryptoError::InvalidBase64)
}

/// Sign a request with a PEM-encoded RSA private key.
pub fn sign_request(
    private_key_pem: &str,
    request: &CanonicalRequest<'_>,
) -> Result<AuthHeaders, CryptoError> {
    let private_key = parse_private_key_pem(private_key_pem)?;
    let canonical = canonical_request(request)?;

    let signature = match request.version {
        SignVersion::V1_3 => {
            let signing_key = SigningKey::<Sha256>::new(private_key);
            signing_key.sign(canonical.as_bytes()).to_bytes().to_vec()
        }
        SignVersion::V1_0 | SignVersion::V1_1 => raw_sign(&private_key, canonical.as_bytes())?,
    };

    Ok(AuthHeaders {
        version: request.version,
        user_id: request.user_id.to_string(),
        timestamp: request.timestamp.to_string(),
        content_hash: content_hash(request.body, request.version),
        signature,
    })
}

/// Verify a request signature with a PEM-encoded RSA public key.
pub fn verify_request(
    public_key_pem: &str,
    headers: &AuthHeaders,
    request: &CanonicalRequest<'_>,
) -> Result<(), CryptoError> {
    if headers.version != request.version
        || headers.user_id != request.user_id
        || headers.content_hash != content_hash(request.body, request.version)
    {
        return Err(CryptoError::VerificationFailed);
    }

    let public_key = parse_public_key_pem(public_key_pem)?;
    let canonical = canonical_request(request)?;

    match request.version {
        SignVersion::V1_3 => {
            let signature = Signature::try_from(headers.signature.as_slice())
                .map_err(|_| CryptoError::InvalidSignatureLength(headers.signature.len()))?;
            let verifying_key = VerifyingKey::<Sha256>::new(public_key);
            verifying_key
                .verify(canonical.as_bytes(), &signature)
                .map_err(|_| CryptoError::VerificationFailed)
        }
        SignVersion::V1_0 | SignVersion::V1_1 => {
            raw_verify(&public_key, canonical.as_bytes(), &headers.signature)
        }
    }
}

/// Parse a PEM public key in PKCS#1, PKCS#8/SPKI, or private-key form.
pub fn parse_public_key_pem(pem: &str) -> Result<RsaPublicKey, CryptoError> {
    let pem = pem.trim();
    if pem.contains("BEGIN RSA PUBLIC KEY") {
        RsaPublicKey::from_pkcs1_pem(pem)
            .map_err(|error| CryptoError::InvalidKey(error.to_string()))
    } else if pem.contains("BEGIN PUBLIC KEY") {
        RsaPublicKey::from_public_key_pem(pem)
            .map_err(|error| CryptoError::InvalidKey(error.to_string()))
    } else if pem.contains("PRIVATE KEY") {
        parse_private_key_pem(pem).map(|private| private.to_public_key())
    } else {
        Err(CryptoError::InvalidKey("unsupported PEM type".into()))
    }
}

/// Parse a PEM private key in PKCS#1 or PKCS#8 form.
pub fn parse_private_key_pem(pem: &str) -> Result<RsaPrivateKey, CryptoError> {
    let pem = pem.trim();
    let key = if pem.contains("BEGIN RSA PRIVATE KEY") {
        RsaPrivateKey::from_pkcs1_pem(pem)
            .map_err(|error| CryptoError::InvalidKey(error.to_string()))?
    } else if pem.contains("BEGIN PRIVATE KEY") {
        RsaPrivateKey::from_pkcs8_pem(pem)
            .map_err(|error| CryptoError::InvalidKey(error.to_string()))?
    } else {
        return Err(CryptoError::InvalidKey("unsupported PEM type".into()));
    };

    if key.n().bits() < 2048 {
        return Err(CryptoError::InvalidKey(
            "RSA key must be at least 2048 bits".into(),
        ));
    }

    Ok(key)
}

fn raw_sign(private_key: &RsaPrivateKey, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if private_key.n().bits() < 2048 {
        return Err(CryptoError::InvalidKey(
            "RSA key must be at least 2048 bits".into(),
        ));
    }

    let size = private_key.size();
    let encoded = pkcs1_type1_pad(message, size)?;
    let message_integer = BigUint::from_bytes_be(&encoded);
    let signature_integer = message_integer.modpow(private_key.d(), private_key.n());
    left_pad(signature_integer.to_bytes_be(), size)
}

fn raw_verify(
    public_key: &RsaPublicKey,
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    let size = public_key.size();
    if signature.len() != size {
        return Err(CryptoError::InvalidSignatureLength(signature.len()));
    }

    let signature_integer = BigUint::from_bytes_be(signature);
    let message_integer = signature_integer.modpow(public_key.e(), public_key.n());
    let encoded = left_pad(message_integer.to_bytes_be(), size)?;
    let expected = pkcs1_type1_pad(message, size)?;

    if encoded == expected {
        Ok(())
    } else {
        Err(CryptoError::VerificationFailed)
    }
}

fn pkcs1_type1_pad(message: &[u8], modulus_size: usize) -> Result<Vec<u8>, CryptoError> {
    if message.len() + 11 > modulus_size {
        return Err(CryptoError::InvalidKey(
            "message too long for RSA key".into(),
        ));
    }

    let padding_length = modulus_size - message.len() - 3;
    let mut encoded = vec![0u8; modulus_size];
    encoded[1] = 0x01;
    encoded[2..2 + padding_length].fill(0xFF);
    encoded[2 + padding_length] = 0x00;
    encoded[3 + padding_length..].copy_from_slice(message);
    Ok(encoded)
}

fn left_pad(bytes: Vec<u8>, size: usize) -> Result<Vec<u8>, CryptoError> {
    if bytes.len() > size {
        return Err(CryptoError::InvalidSignatureLength(bytes.len()));
    }
    let mut padded = vec![0u8; size];
    padded[size - bytes.len()..].copy_from_slice(&bytes);
    Ok(padded)
}
