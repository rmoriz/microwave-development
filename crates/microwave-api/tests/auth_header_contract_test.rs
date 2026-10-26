mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use microwave_crypto::{sign_request, CanonicalRequest, SignVersion};
use tower::ServiceExt;

fn signed_at(timestamp: &str) -> Request<Body> {
    let request = CanonicalRequest {
        method: "GET",
        path: "/users",
        body: b"",
        user_id: "pivotal",
        timestamp,
        api_version: Some("2"),
        version: SignVersion::V1_3,
    };
    let signed = sign_request(support::PRIVATE_KEY, &request).unwrap();
    let encoded = STANDARD.encode(signed.signature);
    let mut builder = Request::builder()
        .method("GET")
        .uri("/users")
        .header("x-ops-sign", "algorithm=sha256;version=1.3;")
        .header("x-ops-userid", "pivotal")
        .header("x-ops-timestamp", timestamp)
        .header("x-ops-content-hash", signed.content_hash)
        .header("x-ops-server-api-version", "2");
    for (index, chunk) in encoded.as_bytes().chunks(60).enumerate() {
        builder = builder.header(
            format!("x-ops-authorization-{}", index + 1),
            String::from_utf8(chunk.to_vec()).unwrap(),
        );
    }
    builder.body(Body::empty()).unwrap()
}

async fn status(request: Request<Body>) -> StatusCode {
    let app = support::app(support::state_with_pivotal());
    app.oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn missing_or_empty_required_headers_are_bad_requests() {
    for header in ["x-ops-sign", "x-ops-timestamp", "x-ops-content-hash"] {
        for empty in [false, true] {
            let mut request = signed_at("2026-10-24T12:00:00Z");
            if empty {
                request.headers_mut().insert(header, "".parse().unwrap());
            } else {
                request.headers_mut().remove(header);
            }
            assert_eq!(
                status(request).await,
                StatusCode::BAD_REQUEST,
                "header {header}, empty={empty}"
            );
        }
    }
}

#[tokio::test]
async fn unsupported_sign_version_is_a_bad_request() {
    let mut request = signed_at("2026-10-24T12:00:00Z");
    request.headers_mut().insert(
        "x-ops-sign",
        "algorithm=sha256;version=99;".parse().unwrap(),
    );
    assert_eq!(status(request).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn stale_and_future_timestamps_are_unauthorized() {
    let now = chrono::Utc::now();
    let stale =
        (now - chrono::Duration::seconds(1200)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let future =
        (now + chrono::Duration::seconds(1200)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    assert_eq!(status(signed_at(&stale)).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(signed_at(&future)).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_current_timestamp_still_authenticates() {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    assert_eq!(status(signed_at(&now)).await, StatusCode::OK);
}

#[tokio::test]
async fn missing_signature_lines_remain_unauthorized() {
    let mut request = signed_at("2026-10-24T12:00:00Z");
    let names: Vec<_> = request
        .headers()
        .keys()
        .filter(|name| name.as_str().starts_with("x-ops-authorization-"))
        .cloned()
        .collect();
    for name in names {
        request.headers_mut().remove(name);
    }
    assert_eq!(status(request).await, StatusCode::UNAUTHORIZED);
}
