use crate::error::CryptoError;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, SecondsFormat, Utc};
use sha1::{Digest, Sha1};
use sha2::Sha256;

/// Supported `X-Ops-Sign` protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignVersion {
    V1_0,
    V1_1,
    V1_3,
}

impl SignVersion {
    fn as_str(self) -> &'static str {
        match self {
            Self::V1_0 => "1.0",
            Self::V1_1 => "1.1",
            Self::V1_3 => "1.3",
        }
    }

    fn expected_algorithm(self) -> &'static str {
        match self {
            Self::V1_0 | Self::V1_1 => "sha1",
            Self::V1_3 => "sha256",
        }
    }
}

/// The inputs needed to construct the canonical string that is signed.
#[derive(Debug, Clone, Copy)]
pub struct CanonicalRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub body: &'a [u8],
    pub user_id: &'a str,
    pub timestamp: &'a str,
    pub api_version: Option<&'a str>,
    pub version: SignVersion,
}

/// Parse an `X-Ops-Sign` header, for example `algorithm=sha1;version=1.0;`.
pub fn parse_sign_header(value: &str) -> Result<SignVersion, CryptoError> {
    let mut algorithm = None;
    let mut version = None;

    for part in value.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| CryptoError::MalformedSignHeader(value.to_string()))?;
        match key.trim().to_ascii_lowercase().as_str() {
            "algorithm" => algorithm = Some(value.trim().to_ascii_lowercase()),
            "version" => version = Some(value.trim().to_string()),
            _ => {}
        }
    }

    let version = version.ok_or_else(|| CryptoError::MalformedSignHeader(value.to_string()))?;
    let sign_version = match version.as_str() {
        "1.0" => SignVersion::V1_0,
        "1.1" => SignVersion::V1_1,
        "1.3" => SignVersion::V1_3,
        _ => return Err(CryptoError::UnsupportedSignVersion(version)),
    };

    if let Some(algorithm) = algorithm {
        if algorithm != sign_version.expected_algorithm() {
            return Err(CryptoError::AlgorithmVersionMismatch {
                algorithm,
                version: sign_version.as_str().to_string(),
            });
        }
    }

    Ok(sign_version)
}

/// Collapse repeated slashes and remove a trailing slash unless the path is `/`.
pub fn canonical_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut previous_was_slash = false;

    for character in path.chars() {
        if character == '/' {
            if !previous_was_slash {
                out.push('/');
            }
            previous_was_slash = true;
        } else {
            out.push(character);
            previous_was_slash = false;
        }
    }

    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }

    out
}

/// Hash a request body with the digest required by the sign version.
pub fn content_hash(body: &[u8], version: SignVersion) -> String {
    match version {
        SignVersion::V1_0 | SignVersion::V1_1 => STANDARD.encode(Sha1::digest(body)),
        SignVersion::V1_3 => STANDARD.encode(Sha256::digest(body)),
    }
}

/// Build the canonical request string used by the signature operation.
pub fn canonical_request(request: &CanonicalRequest<'_>) -> Result<String, CryptoError> {
    let timestamp = canonical_time(request.timestamp)?;
    let body_hash = content_hash(request.body, request.version);

    let canonical = match request.version {
        SignVersion::V1_0 | SignVersion::V1_1 => {
            let user_id = match request.version {
                SignVersion::V1_1 => content_hash(request.user_id.as_bytes(), SignVersion::V1_0),
                _ => request.user_id.to_string(),
            };
            format!(
                "Method:{}\nHashed Path:{}\nX-Ops-Content-Hash:{}\nX-Ops-Timestamp:{}\nX-Ops-UserId:{}",
                request.method.to_ascii_uppercase(),
                content_hash(canonical_path(request.path).as_bytes(), SignVersion::V1_0),
                body_hash,
                timestamp,
                user_id,
            )
        }
        SignVersion::V1_3 => format!(
            "Method:{}\nPath:{}\nX-Ops-Content-Hash:{}\nX-Ops-Sign:version=1.3\nX-Ops-Timestamp:{}\nX-Ops-UserId:{}\nX-Ops-Server-API-Version:{}",
            request.method.to_ascii_uppercase(),
            canonical_path(request.path),
            body_hash,
            timestamp,
            request.user_id,
            request.api_version.unwrap_or("0"),
        ),
    };

    Ok(canonical)
}

fn canonical_time(timestamp: &str) -> Result<String, CryptoError> {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| CryptoError::InvalidTimestamp(timestamp.to_string()))
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, true))
}
