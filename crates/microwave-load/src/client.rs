//! Signed HTTP client for one converge actor.
//!
//! Every request is signed like `chef-client` does it: SHA-256 over the
//! canonical request, version 1.3, with the base64 signature split into
//! 60-byte `X-Ops-Authorization-N` headers. A presigned URL is fetched
//! unsigned because it carries its own token and its query string is not part
//! of the canonical path.

use crate::retry::{self, RetryCount, SolverOverloaded};
use crate::LoadError;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{SecondsFormat, Utc};
use microwave_crypto::{sign_request, CanonicalRequest, SignVersion};
use microwave_objectstore::md5_base64;
use reqwest::{Method, RequestBuilder, StatusCode};

/// Result of a request that survived (or used up) the retry budget.
#[derive(Debug)]
pub struct Outcome {
    pub status: StatusCode,
    pub body: Vec<u8>,
    pub retries: RetryCount,
    pub solver_overloaded: SolverOverloaded,
}

/// Build the one HTTP client of a whole run, with an idle pool that holds every
/// concurrency slot.
///
/// The pool size lives here so no call site can build its own unbounded client
/// and silently drop the keep-alive reuse the measurement depends on.
pub fn run_http(concurrency: usize) -> Result<reqwest::Client, LoadError> {
    reqwest::Client::builder()
        .pool_max_idle_per_host(concurrency)
        .build()
        .map_err(LoadError::transport)
}

/// Signs and sends requests for one actor.
#[derive(Debug, Clone)]
pub struct NodeClient {
    http: reqwest::Client,
    server_url: String,
    user: String,
    key: String,
    api_version: &'static str,
}

impl NodeClient {
    /// Wrap the run-wide `http` client, whose idle pool the caller sizes to the
    /// concurrency, for one actor. `server_url` is the base the relative paths
    /// are appended to.
    pub fn new(http: reqwest::Client, server_url: String, user: String, key: String) -> Self {
        Self {
            http,
            server_url: server_url.trim_end_matches('/').to_string(),
            user,
            key,
            api_version: "2",
        }
    }

    /// Send one request. A relative `path` starts with `/` and is signed; an
    /// absolute URL such as a presigned `/bookshelf` URL is fetched unsigned,
    /// with the `content-md5` and `content-type` headers its signature binds.
    ///
    /// # Errors
    ///
    /// A relative path without the leading slash is rejected before it hits the
    /// wire, and a transport error that survives the retry budget comes back as
    /// [`LoadError::Transport`] with the counters the budget gathered.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Outcome, LoadError> {
        if !is_absolute(path) && !path.starts_with('/') {
            return Err(LoadError::Message(format!(
                "relative path '{path}' does not start with '/'"
            )));
        }
        let (attempt, retries, solver_overloaded) =
            retry::execute(|| async { self.attempt(method.clone(), path, body.as_deref()).await })
                .await?;
        Ok(Outcome {
            status: attempt.status,
            body: attempt.body,
            retries,
            solver_overloaded,
        })
    }

    /// One request. A relative `path` is signed, an absolute URL is not.
    async fn attempt(
        &self,
        method: Method,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<retry::Attempt, LoadError> {
        let signed = !is_absolute(path);
        let url = if signed {
            format!("{}{}", self.server_url, path)
        } else {
            path.to_string()
        };
        // Presigned URLs answer with raw blob bytes; the API answers JSON.
        let accept = if signed {
            "application/json"
        } else {
            "application/octet-stream"
        };
        let mut request = self
            .http
            .request(method.clone(), &url)
            .header("Accept", accept);

        if let Some(body) = body {
            request = if signed {
                request
                    .header("Content-Type", "application/json")
                    .body(body.to_vec())
            } else {
                // The S3 presigner binds `content-md5` and
                // `content-type: application/x-binary` into the signature, so
                // an upload that omits either one is rejected.
                request
                    .header("Content-Type", "application/x-binary")
                    .header("Content-MD5", md5_base64(body))
                    .body(body.to_vec())
            };
        }
        if signed {
            request = self.sign(request, &method, path, body.unwrap_or_default())?;
        }

        let response = request.send().await.map_err(LoadError::transport)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(LoadError::transport)?
            .to_vec();
        Ok(retry::Attempt { status, body })
    }

    /// Add the Chef 1.3 signature headers to `request`.
    fn sign(
        &self,
        mut request: RequestBuilder,
        method: &Method,
        path: &str,
        body: &[u8],
    ) -> Result<RequestBuilder, LoadError> {
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let path = path.split(['?', '#']).next().unwrap_or(path);
        let canonical = CanonicalRequest {
            method: method.as_str(),
            path,
            body,
            user_id: &self.user,
            timestamp: &timestamp,
            api_version: Some(self.api_version),
            version: SignVersion::V1_3,
        };
        let signed = sign_request(&self.key, &canonical)?;

        request = request
            .header("X-Ops-Sign", "algorithm=sha256;version=1.3;")
            .header("X-Ops-Userid", &self.user)
            .header("X-Ops-Timestamp", &signed.timestamp)
            .header("X-Ops-Content-Hash", &signed.content_hash)
            .header("X-Ops-Server-API-Version", self.api_version);

        let encoded = STANDARD.encode(&signed.signature);
        for (index, chunk) in encoded.as_bytes().chunks(60).enumerate() {
            let chunk = std::str::from_utf8(chunk)
                .map_err(|error| LoadError::Message(format!("signature chunk: {error}")))?;
            request = request.header(format!("X-Ops-Authorization-{}", index + 1).as_str(), chunk);
        }
        Ok(request)
    }
}

/// Whether `path` is already a full URL, such as a presigned `/bookshelf` URL.
fn is_absolute(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, Method as AxumMethod, Uri};
    use axum::response::{IntoResponse, Response};
    use axum::routing::any;
    use axum::Router;
    use microwave_crypto::{
        decode_signature, verify_request, AuthHeaders, CanonicalRequest, SignVersion,
    };
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const PRIVATE_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key.pem");
    const PUBLIC_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key_pub.pem");

    /// A running router plus the base URL to reach it.
    struct Mock {
        url: String,
        calls: Arc<AtomicUsize>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    impl Mock {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    /// Serve `app` on a free loopback port and return its base URL.
    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server runs");
        });
        (url, server)
    }

    /// Serve the given responses in order; extra requests get a `200`.
    async fn scripted(responses: Vec<(StatusCode, &'static str)>) -> Mock {
        #[derive(Clone)]
        struct Script {
            responses: Arc<Mutex<VecDeque<(StatusCode, &'static str)>>>,
            calls: Arc<AtomicUsize>,
        }

        async fn respond(State(script): State<Script>) -> Response {
            script.calls.fetch_add(1, Ordering::SeqCst);
            let next = script.responses.lock().expect("script lock").pop_front();
            match next {
                Some((status, body)) => (status, body).into_response(),
                None => (StatusCode::OK, "ok").into_response(),
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let script = Script {
            responses: Arc::new(Mutex::new(responses.into())),
            calls: calls.clone(),
        };
        let app = Router::new().fallback(respond).with_state(script);
        let (url, server) = serve(app).await;
        Mock { url, calls, server }
    }

    /// Serve requests whose signature must verify; answer `401` otherwise.
    async fn verifying() -> Mock {
        #[derive(Clone)]
        struct Verifier {
            public_key: &'static str,
            calls: Arc<AtomicUsize>,
        }

        async fn respond(
            State(verifier): State<Verifier>,
            headers: HeaderMap,
            method: AxumMethod,
            uri: Uri,
            body: Bytes,
        ) -> StatusCode {
            verifier.calls.fetch_add(1, Ordering::SeqCst);
            let Some(user) = header(&headers, "x-ops-userid") else {
                return StatusCode::UNAUTHORIZED;
            };
            let Some(timestamp) = header(&headers, "x-ops-timestamp") else {
                return StatusCode::UNAUTHORIZED;
            };
            let Some(content_hash) = header(&headers, "x-ops-content-hash") else {
                return StatusCode::UNAUTHORIZED;
            };
            if header(&headers, "x-ops-sign").as_deref() != Some("algorithm=sha256;version=1.3;") {
                return StatusCode::UNAUTHORIZED;
            }
            if header(&headers, "x-ops-server-api-version").as_deref() != Some("2") {
                return StatusCode::UNAUTHORIZED;
            }
            let lines: Vec<String> = (1..=16)
                .filter_map(|index| header(&headers, &format!("x-ops-authorization-{index}")))
                .collect();
            let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
            let Ok(signature) = decode_signature(&lines) else {
                return StatusCode::UNAUTHORIZED;
            };
            let signed = AuthHeaders {
                version: SignVersion::V1_3,
                user_id: user.clone(),
                timestamp: timestamp.clone(),
                content_hash,
                signature,
            };
            let request = CanonicalRequest {
                method: method.as_str(),
                path: uri.path(),
                body: &body,
                user_id: &user,
                timestamp: &timestamp,
                api_version: Some("2"),
                version: SignVersion::V1_3,
            };
            match verify_request(verifier.public_key, &signed, &request) {
                Ok(()) => StatusCode::OK,
                Err(_) => StatusCode::UNAUTHORIZED,
            }
        }

        fn header(headers: &HeaderMap, name: &str) -> Option<String> {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let verifier = Verifier {
            public_key: PUBLIC_KEY,
            calls: calls.clone(),
        };
        let app = Router::new().fallback(respond).with_state(verifier);
        let (url, server) = serve(app).await;
        Mock { url, calls, server }
    }

    fn client(mock: &Mock) -> NodeClient {
        NodeClient::new(
            reqwest::Client::new(),
            mock.url.clone(),
            "load-client-00000".to_string(),
            PRIVATE_KEY.to_string(),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn retries_a_503_and_records_it() {
        let mock = scripted(vec![
            (StatusCode::SERVICE_UNAVAILABLE, "busy"),
            (StatusCode::OK, "ok"),
        ])
        .await;

        let outcome = client(&mock)
            .send(Method::GET, "/organizations/load/nodes/n1", None)
            .await
            .expect("request succeeds");

        assert_eq!(outcome.status, StatusCode::OK);
        assert_eq!(outcome.body, b"ok");
        assert_eq!(outcome.retries, RetryCount(1));
        assert_eq!(outcome.solver_overloaded, SolverOverloaded(0));
        assert_eq!(mock.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn six_503s_exhaust_the_budget_and_return_the_last_one() {
        let mock = scripted(vec![(StatusCode::SERVICE_UNAVAILABLE, "busy"); 6]).await;

        let outcome = client(&mock)
            .send(Method::GET, "/organizations/load/nodes/n1", None)
            .await
            .expect("exhausted retries are still an outcome");

        assert_eq!(outcome.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(outcome.retries, RetryCount(retry::MAX_RETRIES));
        assert_eq!(mock.calls(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn a_412_is_not_retried() {
        let mock = scripted(vec![(StatusCode::PRECONDITION_FAILED, "stale")]).await;

        let outcome = client(&mock)
            .send(Method::GET, "/organizations/load/nodes/n1", None)
            .await
            .expect("request completes");

        assert_eq!(outcome.status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(outcome.retries, RetryCount(0));
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_solver_503_is_counted() {
        let mock = scripted(vec![
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Dependency solver overloaded",
            ),
            (StatusCode::OK, "ok"),
        ])
        .await;

        let outcome = client(&mock)
            .send(Method::GET, "/organizations/load/nodes/n1", None)
            .await
            .expect("request succeeds");

        assert_eq!(outcome.status, StatusCode::OK);
        assert_eq!(outcome.retries, RetryCount(1));
        assert_eq!(outcome.solver_overloaded, SolverOverloaded(1));
    }

    #[tokio::test]
    async fn signed_requests_verify_against_the_actor_key() {
        let mock = verifying().await;

        let outcome = client(&mock)
            .send(
                Method::PUT,
                "/organizations/load/nodes/n1",
                Some(br#"{"name":"n1"}"#.to_vec()),
            )
            .await
            .expect("request succeeds");

        assert_eq!(outcome.status, StatusCode::OK);
        assert_eq!(outcome.retries, RetryCount(0));
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test]
    async fn presigned_urls_are_fetched_unsigned() {
        async fn unsigned(
            State(calls): State<Arc<AtomicUsize>>,
            headers: HeaderMap,
            uri: Uri,
        ) -> StatusCode {
            calls.fetch_add(1, Ordering::SeqCst);
            if headers.contains_key("x-ops-sign") {
                return StatusCode::BAD_REQUEST;
            }
            if headers.get("accept").and_then(|value| value.to_str().ok())
                != Some("application/octet-stream")
            {
                return StatusCode::BAD_REQUEST;
            }
            if uri.query() != Some("token=abc") {
                return StatusCode::BAD_REQUEST;
            }
            StatusCode::OK
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/bookshelf/blob", any(unsigned))
            .with_state(calls.clone());
        let (url, server) = serve(app).await;
        let mock = Mock {
            url: url.clone(),
            calls,
            server,
        };

        let outcome = client(&mock)
            .send(
                Method::GET,
                &format!("{url}/bookshelf/blob?token=abc"),
                None,
            )
            .await
            .expect("request succeeds");

        assert_eq!(outcome.status, StatusCode::OK);
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test]
    async fn presigned_puts_bind_content_md5_and_the_binary_content_type() {
        async fn unsigned(State(calls): State<Arc<AtomicUsize>>, headers: HeaderMap) -> StatusCode {
            calls.fetch_add(1, Ordering::SeqCst);
            if headers.contains_key("x-ops-sign") {
                return StatusCode::BAD_REQUEST;
            }
            let content_type = headers
                .get("content-type")
                .and_then(|value| value.to_str().ok());
            if content_type != Some("application/x-binary") {
                return StatusCode::BAD_REQUEST;
            }
            let content_md5 = headers
                .get("content-md5")
                .and_then(|value| value.to_str().ok());
            if content_md5 != Some(md5_base64(b"blob").as_str()) {
                return StatusCode::BAD_REQUEST;
            }
            StatusCode::OK
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/bookshelf/blob", any(unsigned))
            .with_state(calls.clone());
        let (url, server) = serve(app).await;
        let mock = Mock {
            url: url.clone(),
            calls,
            server,
        };

        let outcome = client(&mock)
            .send(
                Method::PUT,
                &format!("{url}/bookshelf/blob?token=put"),
                Some(b"blob".to_vec()),
            )
            .await
            .expect("request succeeds");

        assert_eq!(outcome.status, StatusCode::OK);
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test]
    async fn a_relative_query_is_not_part_of_the_signature() {
        let mock = verifying().await;

        let outcome = client(&mock)
            .send(
                Method::GET,
                "/organizations/load/nodes/n1?verbose=true",
                None,
            )
            .await
            .expect("request succeeds");

        assert_eq!(outcome.status, StatusCode::OK);
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test]
    async fn a_relative_path_must_start_with_a_slash() {
        let mock = scripted(Vec::new()).await;

        let error = client(&mock)
            .send(Method::GET, "organizations/load/nodes/n1", None)
            .await
            .expect_err("relative paths need the leading slash");

        assert!(matches!(error, LoadError::Message(_)), "{error:?}");
        assert_eq!(mock.calls(), 0);
    }

    #[tokio::test]
    async fn the_run_client_carries_a_sized_pool() {
        let mock = verifying().await;
        let client = NodeClient::new(
            run_http(1).expect("client builds"),
            mock.url.clone(),
            "load-client-00000".to_string(),
            PRIVATE_KEY.to_string(),
        );

        let outcome = client
            .send(Method::GET, "/organizations/load/nodes/n1", None)
            .await
            .expect("request succeeds");

        assert_eq!(outcome.status, StatusCode::OK);
    }

    #[tokio::test(start_paused = true)]
    async fn a_transport_error_is_retried_until_the_budget_is_gone() {
        // Bind and drop, so the port stays free but nothing listens on it.
        let port = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listener binds");
            listener.local_addr().expect("addr").port()
        };
        let client = NodeClient::new(
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}"),
            "load-client-00000".to_string(),
            PRIVATE_KEY.to_string(),
        );

        let started = tokio::time::Instant::now();
        let error = client
            .send(Method::GET, "/organizations/load/nodes/n1", None)
            .await
            .expect_err("nothing listens");

        assert!(matches!(
            error,
            LoadError::Transport {
                retries: RetryCount(retry::MAX_RETRIES),
                solver_503: SolverOverloaded(0),
                ..
            }
        ));
        // Five backoffs of at least 80 % of 200, 400, 800, 1600 and 3200ms.
        assert!(started.elapsed() >= std::time::Duration::from_millis(4_960));
    }
}
