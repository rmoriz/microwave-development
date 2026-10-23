use microwave_crypto::{
    decode_signature, sign_request, verify_request, AuthHeaders, CanonicalRequest, CryptoError,
    SignVersion,
};

const MIXLIB_10: &[&str] = &[
    "VXBGnqbTS7nauNcWfXjEHbCFh6hdf4sEn9fC+AeUtB+yVbeIaqLAm4/l4vYA",
    "zwLzH2DXq9PexaPZzauy+b0SMfMQ16spcg+D8pMsywvIXUngreQ1x8H9Bi+M",
    "Y7mnFuv4DAfrHQCn/4PJyc9n1zlbVl7nIwyNUWFidX55DCMjQM3SgZSKF64I",
    "xXk4AieqFHjDCKloNlfP3AgcxjTWL+4TWn2sWrQXiFfqqS9Xtge/T1SY0Nbu",
    "3p1gjIE91nTFNCzi2RdbGXLz7tJVig3EJTFtDB7/A+fN04llaVnAkLcriCpl",
    "qMWstXLffA3YPxDgjNRbaLFCRL/Xr1sHXiHr04AE1Q==",
];

const MIXLIB_11: &[&str] = &[
    "FPMi+xf9c8Q9LPc7XHmPFf4XxrvFryHvNBgOHe2Dui/ZnpdbUY0A5wunk6ub",
    "+HFcJXb4v1WKf1vS2t7nb2zMLL33dhBHRen+9uA6++WX165gHbq/LhmGHEeX",
    "k7JgQFQMF3s4Es0QQajGujqCeDSru7Uguq9Z4j6rsQKTCtdXCXEQeuOlib5q",
    "w/BjaC4i2A57DCMYLgBAeMgXwCzlzEt26cePOFMFwuNlo2R+SckpIlhnuTD3",
    "ApPApxpVv8R7cFgp+WWiMLoBiiFmfRlByHWQ854F3KbEufZAk74cWGFhZQWI",
    "iDz4QVh6PH8GIy+19OYA+oVHQgQXItSe8r+dUKBmcA==",
];

const MIXLIB_13: &[&str] = &[
    "B355ImwZe8RPUaTqMeoGMW1ALl7W/SThGfBm/DLAI2bmK6A6XGEVCyb72JDL",
    "8+Fwt36rnIXPAAD0VEavHbKMaz0whYIUFdASv9jQbIEp6hzxd7e5r5/G6bY0",
    "wh9eXhp7pGd/MByv4lLzRSY/cAOcc0OKqjbT5OKbo/RIcA/bT0BmiqV+5zZA",
    "lOkH62BqycDDZI5NzCU5ZtLzAF46jnq1RDNrAMN+SKi+Nr1/SSLujvSMbQx1",
    "id3XAtx22q4/VsGKHkg63DvPmFgCmqxf9vi3Aqeh0bilt1CBR0VgISHNW+6O",
    "fpIxrTnggAC2+vbDy53tOiwiClj86azD3EknthCm0A==",
];

fn request(version: SignVersion, body: &'static [u8]) -> CanonicalRequest<'static> {
    CanonicalRequest {
        method: "PUT",
        path: "/organizations/acme/nodes/web1",
        body,
        user_id: "web1",
        timestamp: "2026-10-24T12:00:00Z",
        api_version: Some("2"),
        version,
    }
}

#[test]
fn verifies_signatures_generated_by_mixlib_authentication() {
    let private = include_str!("fixtures/test_key.pem");
    let public = include_str!("fixtures/test_key_pub.pem");
    let body = br#"{"name":"web1"}"#;

    let cases = [
        (SignVersion::V1_0, "oGUhJkg6S3tblBYxpQLULLrxuZI=", MIXLIB_10),
        (SignVersion::V1_1, "oGUhJkg6S3tblBYxpQLULLrxuZI=", MIXLIB_11),
        (
            SignVersion::V1_3,
            "1XD5eQOx6tNwvB8oHUYZyuA6QojRZXbLT8p2g3T/W78=",
            MIXLIB_13,
        ),
    ];

    for (version, expected_hash, lines) in cases {
        let req = request(version, body);
        let signature = decode_signature(lines).unwrap();
        let headers = AuthHeaders {
            version,
            user_id: "web1".into(),
            timestamp: "2026-10-24T12:00:00Z".into(),
            content_hash: expected_hash.into(),
            signature: signature.clone(),
        };

        verify_request(public, &headers, &req).unwrap();
        assert_eq!(sign_request(private, &req).unwrap().signature, signature);
    }
}

#[test]
fn round_trips_all_supported_versions() {
    let private = include_str!("fixtures/test_key.pem");
    let public = include_str!("fixtures/test_key_pub.pem");

    for version in [SignVersion::V1_0, SignVersion::V1_1, SignVersion::V1_3] {
        let req = request(version, br#"{"name":"web1"}"#);
        let signed = sign_request(private, &req).unwrap();
        verify_request(public, &signed, &req).unwrap();
    }
}

#[test]
fn rejects_tampered_body_for_all_versions() {
    let private = include_str!("fixtures/test_key.pem");
    let public = include_str!("fixtures/test_key_pub.pem");

    for version in [SignVersion::V1_0, SignVersion::V1_1, SignVersion::V1_3] {
        let signed = sign_request(private, &request(version, b"a")).unwrap();
        let tampered = request(version, b"b");
        assert!(matches!(
            verify_request(public, &signed, &tampered),
            Err(CryptoError::VerificationFailed)
        ));
    }
}

#[test]
fn rejects_wrong_public_key() {
    let private = include_str!("fixtures/test_key.pem");
    let public = include_str!("fixtures/test_key_pub.pem");
    let signed = sign_request(private, &request(SignVersion::V1_3, b"a")).unwrap();

    let other_private = include_str!("fixtures/other_key.pem");
    let other_public = include_str!("fixtures/other_key_pub.pem");
    assert!(sign_request(other_private, &request(SignVersion::V1_3, b"a")).is_ok());
    assert!(verify_request(other_public, &signed, &request(SignVersion::V1_3, b"a")).is_err());
    assert!(verify_request(public, &signed, &request(SignVersion::V1_3, b"a")).is_ok());
}
