//! S3-compatible object store using AWS SigV4 signing.
//!
//! Supports AWS S3, Garage, and MinIO. Requests made by the server are signed
//! with the `Authorization` header; URLs handed to clients are presigned with
//! query-string parameters.

use crate::{
    md5_base64, md5_hex, object_key, ObjectStore, ObjectStoreError, DEFAULT_PRESIGN_EXPIRY_SECONDS,
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::time::Duration;

type HmacSha256 = Hmac<Sha256>;

const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SERVICE: &str = "s3";
const CONTENT_TYPE: &str = "application/x-binary";

/// Maximum time the `/_status` object-store probe may spend on the wire.
const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_millis(1_500);

/// Connection settings for an S3-compatible backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Config {
    /// Base endpoint, for example `http://garage:3900` or `https://s3.amazonaws.com`.
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    /// Use `{endpoint}/{bucket}/{key}` instead of `{bucket}.{endpoint}/{key}`.
    pub path_style: bool,
    /// Lifetime of presigned URLs in seconds.
    pub presign_expiry_seconds: u64,
}

impl S3Config {
    pub fn new(
        endpoint: impl Into<String>,
        bucket: impl Into<String>,
        region: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            bucket: bucket.into(),
            region: region.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            path_style: true,
            presign_expiry_seconds: DEFAULT_PRESIGN_EXPIRY_SECONDS,
        }
    }
}

/// Object store backed by S3-compatible HTTP storage.
#[derive(Clone)]
pub struct S3ObjectStore {
    config: S3Config,
    agent: ureq::Agent,
}

impl S3ObjectStore {
    pub fn new(config: S3Config) -> Result<Self, ObjectStoreError> {
        if config.endpoint.trim().is_empty() {
            return Err(ObjectStoreError::InvalidConfig(
                "endpoint is required".into(),
            ));
        }
        if config.bucket.trim().is_empty() {
            return Err(ObjectStoreError::InvalidConfig("bucket is required".into()));
        }
        if config.region.trim().is_empty() {
            return Err(ObjectStoreError::InvalidConfig("region is required".into()));
        }
        if config.access_key.trim().is_empty() {
            return Err(ObjectStoreError::InvalidConfig(
                "access_key is required".into(),
            ));
        }
        if config.secret_key.trim().is_empty() {
            return Err(ObjectStoreError::InvalidConfig(
                "secret_key is required".into(),
            ));
        }

        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        Ok(Self { config, agent })
    }

    pub fn config(&self) -> &S3Config {
        &self.config
    }

    /// Presigned URL using a caller-supplied clock. Exposed for deterministic tests.
    pub fn presigned_url_at(
        &self,
        method: &str,
        organization_id: &str,
        checksum: &str,
        now: DateTime<Utc>,
    ) -> String {
        let method = method.to_ascii_uppercase();
        let target = self.target(&object_key(organization_id, checksum));
        let amz_date = amz_date(now);
        let date_stamp = now.format("%Y%m%d").to_string();
        let scope = format!("{date_stamp}/{}/{SERVICE}/aws4_request", self.config.region);

        let mut headers = vec![("host".to_string(), target.host.clone())];
        if method == "PUT" {
            // Matches upstream Chef: the checksum is known at presign time and
            // is bound into the signature as content-md5 + content-type.
            let content_md5 = crate::checksum_md5_base64(checksum)
                .unwrap_or_else(|| md5_base64(checksum.as_bytes()));
            headers.push(("content-md5".to_string(), content_md5));
            headers.push(("content-type".to_string(), CONTENT_TYPE.to_string()));
        }
        headers.sort_by(|a, b| a.0.cmp(&b.0));
        let signed_headers = signed_header_names(&headers);
        let canonical_headers = canonical_headers(&headers);

        let query = vec![
            ("X-Amz-Algorithm".to_string(), ALGORITHM.to_string()),
            (
                "X-Amz-Credential".to_string(),
                format!("{}/{scope}", self.config.access_key),
            ),
            ("X-Amz-Date".to_string(), amz_date.clone()),
            (
                "X-Amz-Expires".to_string(),
                self.config.presign_expiry_seconds.to_string(),
            ),
            ("X-Amz-SignedHeaders".to_string(), signed_headers.clone()),
        ];
        let query_string = canonical_query(&query);

        let canonical_request = format!(
            "{method}\n{}\n{query_string}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD",
            target.canonical_uri
        );
        let signature = self.signature(&date_stamp, &amz_date, &canonical_request);

        format!(
            "{}?{}&X-Amz-Signature={signature}",
            target.url, query_string
        )
    }

    fn request_headers(
        &self,
        method: &str,
        target: &Target,
        body: &[u8],
        now: DateTime<Utc>,
    ) -> Vec<(String, String)> {
        let amz_date = amz_date(now);
        let date_stamp = now.format("%Y%m%d").to_string();
        let payload_hash = hex::encode(Sha256::digest(body));

        let mut headers = vec![
            ("host".to_string(), target.host.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        if method == "PUT" {
            headers.push(("content-md5".to_string(), md5_base64(body)));
            headers.push(("content-type".to_string(), CONTENT_TYPE.to_string()));
        }
        headers.sort_by(|a, b| a.0.cmp(&b.0));

        let signed_headers = signed_header_names(&headers);
        let canonical_headers = canonical_headers(&headers);
        let canonical_request = format!(
            "{method}\n{}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
            target.canonical_uri
        );
        let signature = self.signature(&date_stamp, &amz_date, &canonical_request);
        let authorization = format!(
            "{ALGORITHM} Credential={}/{date_stamp}/{}/{SERVICE}/aws4_request, SignedHeaders={signed_headers}, Signature={signature}",
            self.config.access_key, self.config.region
        );

        let mut outgoing: Vec<(String, String)> = headers
            .into_iter()
            .filter(|(name, _)| name != "host")
            .collect();
        outgoing.push(("authorization".to_string(), authorization));
        outgoing
    }

    fn signature(&self, date_stamp: &str, amz_date: &str, canonical_request: &str) -> String {
        let scope = format!("{date_stamp}/{}/{SERVICE}/aws4_request", self.config.region);
        let string_to_sign = format!(
            "{ALGORITHM}\n{amz_date}\n{scope}\n{}",
            hex::encode(Sha256::digest(canonical_request.as_bytes()))
        );

        let date_key = hmac(
            format!("AWS4{}", self.config.secret_key).as_bytes(),
            date_stamp,
        );
        let region_key = hmac(&date_key, &self.config.region);
        let service_key = hmac(&region_key, SERVICE);
        let signing_key = hmac(&service_key, "aws4_request");
        hex::encode(hmac(&signing_key, &string_to_sign))
    }

    fn target(&self, key: &str) -> Target {
        let endpoint = self.config.endpoint.trim_end_matches('/');
        let (scheme, rest) = endpoint.split_once("://").unwrap_or(("http", endpoint));
        let (authority, base_path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        let encoded_key = uri_encode(key, false);
        let encoded_bucket = uri_encode(&self.config.bucket, true);

        if self.config.path_style {
            let canonical_uri = format!("{base_path}/{encoded_bucket}/{encoded_key}");
            Target {
                host: authority.to_string(),
                url: format!("{scheme}://{authority}{canonical_uri}"),
                canonical_uri,
            }
        } else {
            let host = format!("{}.{}", self.config.bucket, authority);
            let canonical_uri = format!("{base_path}/{encoded_key}");
            Target {
                url: format!("{scheme}://{host}{canonical_uri}"),
                canonical_uri,
                host,
            }
        }
    }

    fn bucket_target(&self) -> Target {
        let endpoint = self.config.endpoint.trim_end_matches('/');
        let (scheme, rest) = endpoint.split_once("://").unwrap_or(("http", endpoint));
        let (authority, base_path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        let encoded_bucket = uri_encode(&self.config.bucket, true);

        if self.config.path_style {
            let canonical_uri = format!("{base_path}/{encoded_bucket}");
            Target {
                host: authority.to_string(),
                url: format!("{scheme}://{authority}{canonical_uri}"),
                canonical_uri,
            }
        } else {
            let host = format!("{}.{}", self.config.bucket, authority);
            let canonical_uri = if base_path.is_empty() {
                "/".to_string()
            } else {
                base_path.to_string()
            };
            Target {
                url: format!("{scheme}://{host}{canonical_uri}"),
                canonical_uri,
                host,
            }
        }
    }

    fn execute(
        &self,
        method: &str,
        target: &Target,
        body: &[u8],
    ) -> Result<http::Response<ureq::Body>, ObjectStoreError> {
        let headers = self.request_headers(method, target, body, Utc::now());
        let response = match method {
            "GET" => with_headers(self.agent.get(&target.url), &headers).call(),
            "HEAD" => with_headers(self.agent.head(&target.url), &headers).call(),
            "DELETE" => with_headers(self.agent.delete(&target.url), &headers).call(),
            "PUT" => with_headers(self.agent.put(&target.url), &headers).send(body),
            "POST" => with_headers(self.agent.post(&target.url), &headers).send(body),
            _ => {
                return Err(ObjectStoreError::InvalidConfig(format!(
                    "unsupported HTTP method {method}"
                )))
            }
        }
        .map_err(|error| ObjectStoreError::Http(error.to_string()))?;

        let status = response.status().as_u16();
        if status == 404 {
            return Err(ObjectStoreError::NotFound);
        }
        if !(200..300).contains(&status) {
            return Err(ObjectStoreError::HttpStatus(status));
        }
        Ok(response)
    }
}

fn with_headers<B>(
    mut request: ureq::RequestBuilder<B>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<B> {
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    request
}

impl ObjectStore for S3ObjectStore {
    fn put(
        &self,
        organization_id: &str,
        checksum: &str,
        data: &[u8],
    ) -> Result<(), ObjectStoreError> {
        if md5_hex(data) != checksum {
            return Err(ObjectStoreError::ChecksumMismatch);
        }
        let target = self.target(&object_key(organization_id, checksum));
        self.execute("PUT", &target, data)?;
        Ok(())
    }

    fn get(&self, organization_id: &str, checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        let target = self.target(&object_key(organization_id, checksum));
        let mut response = self.execute("GET", &target, &[])?;
        response
            .body_mut()
            .read_to_vec()
            .map_err(|error| ObjectStoreError::Http(error.to_string()))
    }

    fn exists(&self, organization_id: &str, checksum: &str) -> bool {
        let target = self.target(&object_key(organization_id, checksum));
        match self.execute("HEAD", &target, &[]) {
            Ok(_) => true,
            Err(ObjectStoreError::NotFound) => false,
            Err(_) => false,
        }
    }

    fn delete(&self, organization_id: &str, checksum: &str) -> Result<bool, ObjectStoreError> {
        let target = self.target(&object_key(organization_id, checksum));
        match self.execute("DELETE", &target, &[]) {
            Ok(_) => Ok(true),
            Err(ObjectStoreError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn presigned_url(&self, method: &str, organization_id: &str, checksum: &str) -> String {
        self.presigned_url_at(method, organization_id, checksum, Utc::now())
    }

    fn health_check(&self) -> Result<(), ObjectStoreError> {
        let target = self.bucket_target();
        let headers = self.request_headers("HEAD", &target, &[], Utc::now());
        let response = with_headers(
            self.agent
                .head(&target.url)
                .config()
                .timeout_global(Some(HEALTH_CHECK_TIMEOUT))
                .build(),
            &headers,
        )
        .call()
        .map_err(|error| ObjectStoreError::Http(error.to_string()))?;

        let status = response.status().as_u16();
        if status == 404 {
            return Err(ObjectStoreError::NotFound);
        }
        if !(200..300).contains(&status) {
            return Err(ObjectStoreError::HttpStatus(status));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Target {
    host: String,
    canonical_uri: String,
    url: String,
}

fn amz_date(now: DateTime<Utc>) -> String {
    now.format("%Y%m%dT%H%M%SZ").to_string()
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn signed_header_names(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";")
}

fn canonical_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect()
}

fn canonical_query(params: &[(String, String)]) -> String {
    let mut encoded: Vec<(String, String)> = params
        .iter()
        .map(|(name, value)| (uri_encode(name, true), uri_encode(value, true)))
        .collect();
    encoded.sort();
    encoded
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// AWS SigV4 URI encoding: unreserved characters stay, everything else is
/// percent-encoded with uppercase hex.
fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut output = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(byte as char);
            }
            b'/' if !encode_slash => output.push('/'),
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> S3ObjectStore {
        S3ObjectStore::new(S3Config::new(
            "http://127.0.0.1:3900",
            "chef",
            "garage",
            "AKIAEXAMPLE",
            "SECRETEXAMPLE",
        ))
        .unwrap()
    }

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-24T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn presigned_get_url_has_expected_shape_and_key() {
        let url = store().presigned_url_at(
            "GET",
            "acme",
            "5d41402abc4b2a76b9719d911017c592",
            fixed_time(),
        );
        assert!(url.starts_with(
            "http://127.0.0.1:3900/chef/organization-acme/checksum-5d41402abc4b2a76b9719d911017c592?"
        ));
        assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
        assert!(
            url.contains("X-Amz-Credential=AKIAEXAMPLE%2F20261024%2Fgarage%2Fs3%2Faws4_request")
        );
        assert!(url.contains("X-Amz-Date=20261024T120000Z"));
        assert!(url.contains("X-Amz-Expires=900"));
        assert!(url.contains("X-Amz-SignedHeaders=host"));
        assert!(url.contains("X-Amz-Signature="));
    }

    #[test]
    fn presigned_get_signature_matches_reference_vector() {
        let url = store().presigned_url_at(
            "GET",
            "acme",
            "5d41402abc4b2a76b9719d911017c592",
            fixed_time(),
        );
        let signature = url.split("X-Amz-Signature=").nth(1).unwrap();
        assert_eq!(
            signature,
            "576a78ba6129a942e942b205b3a47b513956bb965b988271cf1692e147615234"
        );
    }

    #[test]
    fn presigned_put_binds_checksum_headers() {
        let url = store().presigned_url_at(
            "PUT",
            "acme",
            "5d41402abc4b2a76b9719d911017c592",
            fixed_time(),
        );
        let query = url.split_once('?').unwrap().1;
        let params: std::collections::HashMap<&str, &str> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        assert_eq!(
            params["X-Amz-SignedHeaders"],
            "content-md5%3Bcontent-type%3Bhost"
        );
        assert!(params.contains_key("X-Amz-Signature"));
    }

    #[test]
    fn presigned_url_accepts_lowercase_methods() {
        let store = store();
        let upper = store.presigned_url_at(
            "GET",
            "acme",
            "5d41402abc4b2a76b9719d911017c592",
            fixed_time(),
        );
        let lower = store.presigned_url_at(
            "get",
            "acme",
            "5d41402abc4b2a76b9719d911017c592",
            fixed_time(),
        );
        assert_eq!(upper, lower);
    }

    #[test]
    fn virtual_host_style_moves_bucket_into_host() {
        let mut config = S3Config::new(
            "https://s3.example.com",
            "chef",
            "us-east-1",
            "key",
            "secret",
        );
        config.path_style = false;
        let store = S3ObjectStore::new(config).unwrap();
        let url = store.presigned_url_at(
            "GET",
            "acme",
            "abc",
            DateTime::parse_from_rfc3339("2026-10-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert!(url.starts_with("https://chef.s3.example.com/organization-acme/checksum-abc?"));
    }

    #[test]
    fn rejects_incomplete_configuration() {
        let mut config = S3Config::new("http://localhost", "chef", "garage", "key", "secret");
        config.secret_key.clear();
        assert!(matches!(
            S3ObjectStore::new(config),
            Err(ObjectStoreError::InvalidConfig(_))
        ));
    }

    #[test]
    fn encodes_unreserved_and_reserved_bytes() {
        assert_eq!(uri_encode("a b/c", false), "a%20b/c");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(uri_encode("a-_.~", true), "a-_.~");
    }

    /// Keep local-network health tests from competing under parallel CI runs.
    static NETWORK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    type BucketRequest = (String, Option<String>);

    async fn bucket_server(
        status: u16,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<BucketRequest>>>) {
        use axum::http::StatusCode;
        use axum::Router;

        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let app = Router::new().fallback(move |request: axum::extract::Request| {
            let seen = seen.clone();
            async move {
                let line = format!("{} {}", request.method(), request.uri().path());
                let authorization = request
                    .headers()
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                seen.lock().unwrap().push((line, authorization));
                StatusCode::from_u16(status).unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), requests)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_check_heads_the_bucket_with_a_bounded_signed_request() {
        let _network = NETWORK_TEST_LOCK.lock().await;
        let (base_url, requests) = bucket_server(200).await;
        let store =
            S3ObjectStore::new(S3Config::new(base_url, "chef", "garage", "key", "secret")).unwrap();

        store.health_check().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "HEAD /chef");
        let authorization = requests[0].1.as_deref().expect("signed request");
        assert!(
            authorization.starts_with("AWS4-HMAC-SHA256 Credential=key/"),
            "unexpected authorization header: {authorization}"
        );
        assert!(authorization.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_check_reports_bucket_failures() {
        let _network = NETWORK_TEST_LOCK.lock().await;
        let (base_url, _requests) = bucket_server(503).await;
        let store =
            S3ObjectStore::new(S3Config::new(base_url, "chef", "garage", "key", "secret")).unwrap();

        match store.health_check() {
            Err(ObjectStoreError::HttpStatus(503)) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
