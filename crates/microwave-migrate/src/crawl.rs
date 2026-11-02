//! Shared plumbing for crawlers that read a live legacy Chef Infra API.
//!
//! Every request is signed with the configured client key using Chef's
//! `X-Ops-Sign: version=1.3` protocol. Collection parsing, deterministic ids,
//! and cookbook blob downloads live here so every legacy crawler reuses the
//! same transport instead of copying it.
//!
//! The legacy API does not expose the source database's internal ids or authz
//! ids. Those are derived deterministically from the organization and object
//! names so repeated crawls produce the same snapshot and importing the same
//! snapshot twice is idempotent, unless the snapshot is a goiardi snapshot:
//! that layout does not merge and is refused when the target already holds
//! data.

use crate::MigrateError;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{SecondsFormat, Utc};
use microwave_crypto::{sign_request, CanonicalRequest, SignVersion};
use microwave_objectstore::{md5_hex, LocalObjectStore, ObjectStore};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Largest single cookbook blob a crawler will download.
pub(crate) const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;
/// Largest total volume of cookbook blobs downloaded in one crawl.
pub(crate) const MAX_TOTAL_BLOB_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Counts from the optional cookbook blob copy during a live crawl.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BlobSummary {
    /// Blobs written to the local object store.
    pub downloaded: usize,
    /// Blobs that already existed locally and were not requested again.
    pub skipped: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct CollectionEntry {
    pub(crate) name: String,
    pub(crate) value: Value,
    pub(crate) url: Option<String>,
}

pub(crate) struct Fetcher {
    agent: ureq::Agent,
    base_url: String,
    client_name: String,
    private_key: String,
    api_version: String,
}

impl Fetcher {
    pub(crate) fn new(
        base_url: &str,
        client_name: &str,
        key_path: &Path,
        api_version: u32,
        socks5: Option<&str>,
    ) -> Result<Self, MigrateError> {
        let normalized = base_url.trim().trim_end_matches('/').to_string();
        if !(normalized.starts_with("http://") || normalized.starts_with("https://")) {
            return Err(MigrateError::InvalidUrl(base_url.to_string()));
        }
        let private_key = crate::from_api::read_private_key(key_path)?;
        // Fail before the first request when the key is unusable.
        microwave_crypto::parse_private_key_pem(&private_key)?;

        let mut config = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(15)))
            .timeout_global(Some(std::time::Duration::from_secs(120)))
            .http_status_as_error(false);
        if let Some(host_port) = socks5 {
            let Some((_host, port)) = host_port.rsplit_once(':') else {
                return Err(MigrateError::Config(format!(
                    "invalid --socks5 {host_port:?}; expected HOST:PORT"
                )));
            };
            if port.parse::<u16>().is_err() {
                return Err(MigrateError::Config(format!(
                    "invalid --socks5 {host_port:?}; the port must be a number"
                )));
            }
            let proxy = ureq::Proxy::new(&format!("socks5://{host_port}")).map_err(|error| {
                MigrateError::Config(format!("invalid --socks5 {host_port:?}: {error}"))
            })?;
            config = config.proxy(Some(proxy));
        }

        Ok(Self {
            agent: config.build().new_agent(),
            base_url: normalized,
            client_name: client_name.to_string(),
            private_key,
            api_version: api_version.to_string(),
        })
    }

    pub(crate) fn resolve(&self, reference: &str) -> String {
        if is_absolute_url(reference) {
            reference.to_string()
        } else if reference.starts_with('/') {
            format!("{}{}", self.base_url, reference)
        } else {
            format!("{}/{}", self.base_url, reference)
        }
    }

    pub(crate) fn signed_get(&self, url: &str) -> Result<(u16, String), MigrateError> {
        match self.signed_request(url)?.call() {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let body = response.body_mut().read_to_string().map_err(|error| {
                    MigrateError::InvalidApiResponse {
                        url: redacted_url(url),
                        message: error.to_string(),
                    }
                })?;
                Ok((status, body))
            }
            Err(error) => Err(MigrateError::Transport {
                url: redacted_url(url),
                message: error.to_string(),
            }),
        }
    }

    pub(crate) fn get_optional_raw(
        &self,
        url: &str,
    ) -> Result<Option<(Value, String)>, MigrateError> {
        let (status, body) = self.signed_get(url)?;
        if status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(MigrateError::Api {
                url: redacted_url(url),
                status,
                message: api_message(status, &body),
            });
        }
        let value =
            serde_json::from_str(&body).map_err(|error| MigrateError::InvalidApiResponse {
                url: redacted_url(url),
                message: error.to_string(),
            })?;
        Ok(Some((value, body)))
    }

    pub(crate) fn get_optional_json(&self, url: &str) -> Result<Option<Value>, MigrateError> {
        Ok(self.get_optional_raw(url)?.map(|(value, _)| value))
    }

    pub(crate) fn get_collection(&self, path: &str) -> Result<Vec<CollectionEntry>, MigrateError> {
        let mut entries = Vec::new();
        let mut next = Some(self.resolve(path));
        let mut visited = HashSet::new();
        let mut pages = 0usize;

        while let Some(url) = next.take() {
            if !visited.insert(url.clone()) {
                break;
            }
            pages += 1;
            if pages > 1000 {
                return Err(MigrateError::InvalidApiResponse {
                    url,
                    message: "pagination exceeded 1000 pages".to_string(),
                });
            }
            let Some((value, _)) = self.get_optional_raw(&url)? else {
                break;
            };
            let (mut page, next_url) = parse_collection(value);
            entries.append(&mut page);
            next = next_url.map(|reference| self.resolve(&reference));
        }
        Ok(entries)
    }

    pub(crate) fn get_signed_bytes(
        &self,
        url: &str,
        redacted: &str,
    ) -> Result<(u16, Vec<u8>), MigrateError> {
        match self.signed_request(url)?.call() {
            Ok(response) => read_blob_response(response, redacted),
            Err(error) => Err(MigrateError::Transport {
                url: redacted.to_string(),
                message: error.to_string().replace(url, redacted),
            }),
        }
    }

    pub(crate) fn get_unsigned_bytes(
        &self,
        url: &str,
        redacted: &str,
    ) -> Result<(u16, Vec<u8>), MigrateError> {
        match self
            .agent
            .get(url)
            .header("Accept", "application/octet-stream")
            .call()
        {
            Ok(response) => read_blob_response(response, redacted),
            Err(error) => Err(MigrateError::Transport {
                url: redacted.to_string(),
                message: error.to_string().replace(url, redacted),
            }),
        }
    }

    fn signed_request(
        &self,
        url: &str,
    ) -> Result<ureq::RequestBuilder<ureq::typestate::WithoutBody>, MigrateError> {
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let path = path_for_signing(url);
        let request = CanonicalRequest {
            method: "GET",
            path,
            body: b"",
            user_id: &self.client_name,
            timestamp: &timestamp,
            api_version: Some(&self.api_version),
            version: SignVersion::V1_3,
        };
        let signed = sign_request(&self.private_key, &request)?;

        let mut request = self
            .agent
            .get(url)
            .header("Accept", "application/json")
            .header("X-Ops-Sign", "algorithm=sha256;version=1.3;")
            .header("X-Ops-Userid", &self.client_name)
            .header("X-Ops-Timestamp", &signed.timestamp)
            .header("X-Ops-Content-Hash", &signed.content_hash)
            .header("X-Ops-Server-API-Version", &self.api_version);

        let encoded = STANDARD.encode(&signed.signature);
        for (index, chunk) in encoded.as_bytes().chunks(60).enumerate() {
            let chunk =
                std::str::from_utf8(chunk).map_err(|error| MigrateError::InvalidApiResponse {
                    url: redacted_url(url),
                    message: error.to_string(),
                })?;
            request = request.header(format!("X-Ops-Authorization-{}", index + 1).as_str(), chunk);
        }
        Ok(request)
    }
}

/// Fetch one cookbook blob and write it to the local object store.
///
/// The reference is resolved against the base URL. Absolute bookshelf/S3 URLs
/// are fetched unsigned; a relative reference points at the API server, whose
/// blob route is signed like every other request. The body is rejected unless
/// it is non-empty, matches the expected MD5, and stays inside the total
/// download budget.
pub(crate) fn store_blob(
    store: &LocalObjectStore,
    org_id: &str,
    checksum: &str,
    reference: &str,
    fetcher: &Fetcher,
    summary: &mut BlobSummary,
    downloaded_bytes: &mut u64,
) -> Result<(), MigrateError> {
    let resolved = fetcher.resolve(reference);
    let redacted = redacted_url(&resolved);
    let (status, bytes) = if is_absolute_url(reference) {
        fetcher.get_unsigned_bytes(&resolved, &redacted)?
    } else {
        fetcher.get_signed_bytes(&resolved, &redacted)?
    };

    if !(200..300).contains(&status) {
        return Err(MigrateError::Api {
            url: redacted,
            status,
            message: format!("cookbook blob download failed with HTTP {status}"),
        });
    }
    if bytes.is_empty() {
        return Err(MigrateError::InvalidApiResponse {
            url: redacted,
            message: "cookbook blob response was empty".to_string(),
        });
    }

    let actual = md5_hex(&bytes);
    if !actual.eq_ignore_ascii_case(checksum) {
        return Err(MigrateError::InvalidApiResponse {
            url: redacted_url(reference),
            message: format!(
                "cookbook blob checksum mismatch for organization {org_id}: expected {checksum}, got {actual}"
            ),
        });
    }

    let total = *downloaded_bytes + bytes.len() as u64;
    if total > MAX_TOTAL_BLOB_BYTES {
        return Err(MigrateError::Config(format!(
            "cookbook blob download would exceed the {} GiB total limit; reduce the --org scope or copy the bookshelf separately",
            MAX_TOTAL_BLOB_BYTES / (1024 * 1024 * 1024)
        )));
    }

    // LocalObjectStore::put verifies the MD5 again and writes through a
    // temporary `.part` file before renaming, so no partial final blob
    // is ever visible.
    store.put(org_id, checksum, &bytes)?;
    *downloaded_bytes = total;
    summary.downloaded += 1;
    Ok(())
}

pub(crate) fn parse_collection(value: Value) -> (Vec<CollectionEntry>, Option<String>) {
    match value {
        Value::Array(items) => (items.into_iter().filter_map(array_entry).collect(), None),
        Value::Object(map) => {
            if let Some(next) = pagination_link(&map) {
                let items = map
                    .get("results")
                    .or_else(|| map.get("items"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                return (
                    items.into_iter().filter_map(array_entry).collect(),
                    Some(next),
                );
            }
            if let Some(items) = map.get("results").and_then(Value::as_array) {
                return (
                    items.clone().into_iter().filter_map(array_entry).collect(),
                    None,
                );
            }
            if let Some(items) = map.get("items").and_then(Value::as_array) {
                return (
                    items.clone().into_iter().filter_map(array_entry).collect(),
                    None,
                );
            }

            // A single resource object rather than a name -> URL map.
            if map.contains_key("name")
                || map.contains_key("username")
                || map.contains_key("clientname")
            {
                if let Some(entry) = map_entry_for_object(&map) {
                    return (vec![entry], None);
                }
            }

            let entries = map
                .iter()
                .filter_map(|(name, value)| match value {
                    Value::String(url) => Some(CollectionEntry {
                        name: name.clone(),
                        value: Value::Null,
                        url: Some(url.clone()),
                    }),
                    Value::Null => Some(CollectionEntry {
                        name: name.clone(),
                        value: Value::Null,
                        url: None,
                    }),
                    Value::Object(object) => Some(CollectionEntry {
                        name: name.clone(),
                        value: Value::Object(object.clone()),
                        url: url_field(&Value::Object(object.clone())),
                    }),
                    Value::Array(array) => Some(CollectionEntry {
                        name: name.clone(),
                        value: Value::Array(array.clone()),
                        url: None,
                    }),
                    _ => None,
                })
                .collect();
            (entries, None)
        }
        _ => (Vec::new(), None),
    }
}

fn pagination_link(map: &Map<String, Value>) -> Option<String> {
    for key in ["next", "next_page", "next_url"] {
        if let Some(Value::String(next)) = map.get(key) {
            if !next.is_empty() {
                return Some(next.clone());
            }
        }
    }
    None
}

fn array_entry(value: Value) -> Option<CollectionEntry> {
    match value {
        Value::String(url) => {
            let name = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            Some(CollectionEntry {
                name,
                value: Value::Null,
                url: Some(url),
            })
        }
        Value::Object(object) => map_entry_for_object(&object),
        _ => None,
    }
}

fn map_entry_for_object(object: &Map<String, Value>) -> Option<CollectionEntry> {
    let value = Value::Object(object.clone());
    let name = string_field(&value, "name")
        .or_else(|| string_field(&value, "username"))
        .or_else(|| string_field(&value, "clientname"))
        .or_else(|| string_field(&value, "groupname"))
        .or_else(|| string_field(&value, "id"))
        .or_else(|| {
            url_field(&value).map(|url| {
                url.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
        })?;
    Some(CollectionEntry {
        name,
        url: url_field(&value),
        value,
    })
}

/// Return each unique cookbook file checksum and the first URL advertised for
/// it. The legacy API usually provides a presigned bookshelf/S3 URL, but some
/// server versions return a relative path or omit the URL entirely.
pub(crate) fn cookbook_files(value: &Value) -> Vec<(String, Option<String>)> {
    const FILE_KEYS: &[&str] = &[
        "recipes",
        "definitions",
        "libraries",
        "attributes",
        "files",
        "templates",
        "resources",
        "providers",
        "root_files",
    ];
    let mut files: BTreeMap<String, Option<String>> = BTreeMap::new();
    for key in FILE_KEYS {
        if let Some(entries) = value.get(key).and_then(Value::as_array) {
            for file in entries {
                let Some(checksum) = string_field(file, "checksum") else {
                    continue;
                };
                let url = url_field(file);
                files
                    .entry(checksum)
                    .and_modify(|existing| {
                        if existing.is_none() {
                            *existing = url.clone();
                        }
                    })
                    .or_insert(url);
            }
        }
    }
    files.into_iter().collect()
}

fn path_for_signing(url: &str) -> &str {
    let after_scheme = url
        .find("://")
        .map(|index| &url[index + 3..])
        .unwrap_or(url);
    match after_scheme.find('/') {
        Some(index) => {
            let path = &after_scheme[index..];
            path.split(['?', '#']).next().unwrap_or(path)
        }
        None => "/",
    }
}

pub(crate) fn is_absolute_url(reference: &str) -> bool {
    reference.starts_with("http://") || reference.starts_with("https://")
}

/// Remove the query string and fragment from a URL before printing it.
///
/// Bookshelf/S3 URLs carry their credentials in query parameters, so an error
/// must never echo the raw URL.
pub(crate) fn redacted_url(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    if base.len() == url.len() {
        base.to_string()
    } else {
        format!("{base}?<redacted>")
    }
}

/// Read a bounded cookbook blob response. Non-2xx responses are returned with
/// an empty body so the caller can report the status without echoing a
/// server-supplied payload that may contain a signed URL.
fn read_blob_response(
    mut response: ureq::http::Response<ureq::Body>,
    redacted: &str,
) -> Result<(u16, Vec<u8>), MigrateError> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Ok((status, Vec::new()));
    }

    let declared = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_BLOB_BYTES) {
        return Err(MigrateError::InvalidApiResponse {
            url: redacted.to_string(),
            message: format!(
                "cookbook blob is larger than the {} MiB per-file limit",
                MAX_BLOB_BYTES / (1024 * 1024)
            ),
        });
    }

    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BLOB_BYTES)
        .read_to_vec()
        .map_err(|error| MigrateError::InvalidApiResponse {
            url: redacted.to_string(),
            message: format!(
                "could not read cookbook blob (limit {} MiB): {error}",
                MAX_BLOB_BYTES / (1024 * 1024)
            ),
        })?;
    Ok((status, body))
}

fn api_message(status: u16, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return match status {
            401 => "authentication failed (check --client-name and --key)".to_string(),
            403 => "authorization failed (the client may not be a superuser)".to_string(),
            _ => format!("unexpected HTTP status {status}"),
        };
    }
    let mut message = body.replace(['\n', '\r'], " ");
    if message.len() > 500 {
        message.truncate(500);
        message.push_str("...");
    }
    message
}

pub(crate) fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(crate) fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

pub(crate) fn url_field(value: &Value) -> Option<String> {
    string_field(value, "url").or_else(|| string_field(value, "uri"))
}

pub(crate) fn synthetic_id(scope: &str) -> String {
    let digest = Sha256::digest(format!("microwave-migrate/from-api/v1/{scope}").as_bytes());
    hex::encode(&digest[..16])
}

pub(crate) fn synthetic_i64(scope: &str) -> i64 {
    let digest = Sha256::digest(format!("microwave-migrate/from-api/v1/{scope}").as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) & 0x7fff_ffff_ffff_ffff) as i64
}

/// Percent-encode a single URL path segment. Chef names are already limited to
/// a small character set, but this keeps unusual names from breaking requests.
pub(crate) fn encode_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'@' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
