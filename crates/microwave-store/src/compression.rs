//! Transparent compression for `serialized_object` columns.
//!
//! New values are stored as `mwz1:` followed by the standard base64 encoding
//! of a zlib stream containing the UTF-8 JSON document. Values without the
//! prefix are legacy plain JSON and are returned unchanged, so databases
//! written before compression was enabled keep working.
//!
//! Decompression is bounded because the input is database content: a small
//! corrupt or malicious value must not be able to allocate unbounded memory.

use crate::sqlite::StoreError;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression as FlateCompression;
use std::io::{Read, Write};

/// Prefix identifying a compressed serialized object.
pub const COMPRESSED_PREFIX: &str = "mwz1:";

/// Maximum number of bytes a compressed value may expand to.
pub const MAX_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;

/// Storage compression mode for newly written serialized objects.
///
/// Reads always decode the `mwz1:` prefix regardless of this setting, so
/// switching a deployment to [`Compression::None`] does not make existing rows
/// unreadable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Compression {
    /// Store new values as zlib-compressed, base64-encoded text.
    #[default]
    Zlib,
    /// Store new values as plain JSON text.
    None,
}

impl Compression {
    /// Parse the configuration spelling (`zlib` or `none`).
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "zlib" => Ok(Self::Zlib),
            "none" => Ok(Self::None),
            other => Err(StoreError::Invalid(format!(
                "unknown storage compression {other:?}; expected \"zlib\" or \"none\""
            ))),
        }
    }

    /// Configuration spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zlib => "zlib",
            Self::None => "none",
        }
    }
}

/// Encode a plain JSON serialized object for storage.
///
/// [`Compression::None`] leaves the value unchanged. [`Compression::Zlib`]
/// returns [`COMPRESSED_PREFIX`] plus base64(zlib(value)).
pub fn encode_serialized(value: &str, compression: Compression) -> Result<String, StoreError> {
    match compression {
        Compression::None => Ok(value.to_string()),
        Compression::Zlib => {
            let mut encoder = ZlibEncoder::new(Vec::new(), FlateCompression::default());
            encoder
                .write_all(value.as_bytes())
                .map_err(|error| StoreError::Compression(error.to_string()))?;
            let compressed = encoder
                .finish()
                .map_err(|error| StoreError::Compression(error.to_string()))?;
            Ok(format!("{COMPRESSED_PREFIX}{}", BASE64.encode(compressed)))
        }
    }
}

/// Decode a stored serialized object back to plain JSON.
///
/// Plain values (no [`COMPRESSED_PREFIX`]) are returned unchanged for backward
/// compatibility. Compressed values that are truncated, corrupt, invalid
/// base64, not UTF-8, or larger than [`MAX_DECOMPRESSED_BYTES`] return a
/// [`StoreError::Compression`] error.
pub fn decode_serialized(value: &str) -> Result<String, StoreError> {
    decode_serialized_with_limit(value, MAX_DECOMPRESSED_BYTES)
}

/// Encode an optional serialized object, preserving `None`.
pub fn encode_serialized_optional(
    value: Option<&str>,
    compression: Compression,
) -> Result<Option<String>, StoreError> {
    value
        .map(|value| encode_serialized(value, compression))
        .transpose()
}

/// Decode an optional serialized object, preserving `None`.
pub fn decode_serialized_optional(value: Option<&str>) -> Result<Option<String>, StoreError> {
    value.map(decode_serialized).transpose()
}

fn decode_serialized_with_limit(value: &str, limit: usize) -> Result<String, StoreError> {
    let Some(encoded) = value.strip_prefix(COMPRESSED_PREFIX) else {
        return Ok(value.to_string());
    };

    let compressed = BASE64
        .decode(encoded)
        .map_err(|error| StoreError::Compression(format!("invalid base64 payload: {error}")))?;

    let mut decoder = ZlibDecoder::new(compressed.as_slice());
    let mut decoded = Vec::new();
    let read_limit = limit.saturating_add(1) as u64;
    decoder
        .by_ref()
        .take(read_limit)
        .read_to_end(&mut decoded)
        .map_err(|error| StoreError::Compression(format!("invalid zlib payload: {error}")))?;
    if decoded.len() > limit {
        return Err(StoreError::Compression(format!(
            "decompressed value exceeds the {limit} byte limit"
        )));
    }

    String::from_utf8(decoded)
        .map_err(|error| StoreError::Compression(format!("payload is not valid UTF-8: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"name":"node-1","run_list":["recipe[apt]"]}"#;

    #[test]
    fn zlib_round_trip_uses_the_magic_prefix() {
        let encoded = encode_serialized(JSON, Compression::Zlib).unwrap();
        assert!(encoded.starts_with(COMPRESSED_PREFIX));
        assert!(!encoded.contains(JSON));
        assert_eq!(decode_serialized(&encoded).unwrap(), JSON);
    }

    #[test]
    fn none_round_trip_stores_plain_json() {
        let encoded = encode_serialized(JSON, Compression::None).unwrap();
        assert_eq!(encoded, JSON);
        assert_eq!(decode_serialized(&encoded).unwrap(), JSON);
    }

    #[test]
    fn legacy_plain_json_is_readable() {
        assert_eq!(decode_serialized(JSON).unwrap(), JSON);
    }

    #[test]
    fn corrupt_base64_returns_a_store_error() {
        let error = decode_serialized("mwz1:not base64!").unwrap_err();
        assert!(matches!(error, StoreError::Compression(_)));
    }

    #[test]
    fn corrupt_zlib_returns_a_store_error() {
        let encoded = format!("{COMPRESSED_PREFIX}{}", BASE64.encode(b"not zlib"));
        let error = decode_serialized(&encoded).unwrap_err();
        assert!(matches!(error, StoreError::Compression(_)));
    }

    #[test]
    fn decompression_is_bounded() {
        let encoded = encode_serialized("hello world", Compression::Zlib).unwrap();
        let error = decode_serialized_with_limit(&encoded, 4).unwrap_err();
        assert!(matches!(error, StoreError::Compression(_)));
    }

    #[test]
    fn non_utf8_payload_returns_a_store_error() {
        let mut encoder = ZlibEncoder::new(Vec::new(), FlateCompression::default());
        encoder.write_all(&[0xff, 0xfe]).unwrap();
        let encoded = format!(
            "{COMPRESSED_PREFIX}{}",
            BASE64.encode(encoder.finish().unwrap())
        );
        assert!(matches!(
            decode_serialized(&encoded),
            Err(StoreError::Compression(_))
        ));
    }

    #[test]
    fn compression_names_round_trip() {
        for mode in [Compression::Zlib, Compression::None] {
            assert_eq!(Compression::parse(mode.as_str()).unwrap(), mode);
        }
        assert!(Compression::parse("gzip").is_err());
    }
}
