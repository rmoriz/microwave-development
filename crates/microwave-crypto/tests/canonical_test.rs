use microwave_crypto::{
    canonical_path, canonical_request, content_hash, parse_sign_header, CanonicalRequest,
    CryptoError, SignVersion,
};

#[test]
fn parses_supported_sign_versions() {
    assert_eq!(parse_sign_header("version=1.0").unwrap(), SignVersion::V1_0);
    assert_eq!(parse_sign_header("version=1.1").unwrap(), SignVersion::V1_1);
    assert_eq!(
        parse_sign_header("algorithm=sha256;version=1.3").unwrap(),
        SignVersion::V1_3
    );
    assert_eq!(
        parse_sign_header("algorithm=sha1;version=1.0;").unwrap(),
        SignVersion::V1_0
    );
}

#[test]
fn rejects_unknown_sign_version() {
    assert!(matches!(
        parse_sign_header("version=2.0"),
        Err(CryptoError::UnsupportedSignVersion(_))
    ));
}

#[test]
fn rejects_mismatched_algorithm() {
    assert!(matches!(
        parse_sign_header("algorithm=sha256;version=1.0"),
        Err(CryptoError::AlgorithmVersionMismatch { .. })
    ));
}

#[test]
fn normalizes_canonical_paths() {
    assert_eq!(canonical_path("/nodes/web1/"), "/nodes/web1");
    assert_eq!(
        canonical_path("//organizations//acme//nodes/"),
        "/organizations/acme/nodes"
    );
    assert_eq!(canonical_path("/"), "/");
}

#[test]
fn hashes_empty_and_non_empty_bodies() {
    assert_eq!(
        content_hash(b"", SignVersion::V1_0),
        "2jmj7l5rSw0yVb/vlWAYkK/YBwk="
    );
    assert_eq!(
        content_hash(b"", SignVersion::V1_3),
        "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="
    );
    assert_eq!(
        content_hash(br#"{"name":"web1"}"#, SignVersion::V1_0),
        "oGUhJkg6S3tblBYxpQLULLrxuZI="
    );
}

#[test]
fn builds_v1_0_canonical_request() {
    let req = CanonicalRequest {
        method: "PUT",
        path: "/organizations/acme/nodes/web1",
        body: br#"{"name":"web1"}"#,
        user_id: "web1",
        timestamp: "2026-10-24T12:00:00Z",
        api_version: Some("2"),
        version: SignVersion::V1_0,
    };
    assert_eq!(
        canonical_request(&req).unwrap(),
        "Method:PUT\n\
         Hashed Path:Oxqmc+v38KaZDtb2nDsnJ0wHoPE=\n\
         X-Ops-Content-Hash:oGUhJkg6S3tblBYxpQLULLrxuZI=\n\
         X-Ops-Timestamp:2026-10-24T12:00:00Z\n\
         X-Ops-UserId:web1"
    );
}

#[test]
fn builds_v1_1_canonical_request_with_hashed_user_id() {
    let req = CanonicalRequest {
        method: "PUT",
        path: "/organizations/acme/nodes/web1",
        body: br#"{"name":"web1"}"#,
        user_id: "web1",
        timestamp: "2026-10-24T12:00:00Z",
        api_version: Some("2"),
        version: SignVersion::V1_1,
    };
    assert_eq!(
        canonical_request(&req).unwrap(),
        "Method:PUT\n\
         Hashed Path:Oxqmc+v38KaZDtb2nDsnJ0wHoPE=\n\
         X-Ops-Content-Hash:oGUhJkg6S3tblBYxpQLULLrxuZI=\n\
         X-Ops-Timestamp:2026-10-24T12:00:00Z\n\
         X-Ops-UserId:l3WIqH4QWCAycWcSzPXYXRil/M8="
    );
}

#[test]
fn builds_v1_3_canonical_request() {
    let req = CanonicalRequest {
        method: "PUT",
        path: "/organizations/acme/nodes/web1",
        body: br#"{"name":"web1"}"#,
        user_id: "web1",
        timestamp: "2026-10-24T14:00:00+02:00",
        api_version: Some("2"),
        version: SignVersion::V1_3,
    };
    assert_eq!(
        canonical_request(&req).unwrap(),
        "Method:PUT\n\
         Path:/organizations/acme/nodes/web1\n\
         X-Ops-Content-Hash:1XD5eQOx6tNwvB8oHUYZyuA6QojRZXbLT8p2g3T/W78=\n\
         X-Ops-Sign:version=1.3\n\
         X-Ops-Timestamp:2026-10-24T12:00:00Z\n\
         X-Ops-UserId:web1\n\
         X-Ops-Server-API-Version:2"
    );
}
