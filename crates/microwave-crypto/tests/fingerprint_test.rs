use microwave_crypto::{generate_rsa_keypair, public_key_fingerprint};

#[test]
fn fingerprints_are_colon_separated_sha1_hex() {
    let (_, public) = generate_rsa_keypair().unwrap();
    let fingerprint = public_key_fingerprint(&public).unwrap();

    let parts = fingerprint.split(':').collect::<Vec<_>>();
    assert_eq!(parts.len(), 20, "SHA-1 fingerprints have 20 octets");
    for part in parts {
        assert_eq!(part.len(), 2);
        assert!(part
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase()));
    }
}

#[test]
fn fingerprints_are_stable_for_the_same_key() {
    let (_, public) = generate_rsa_keypair().unwrap();
    assert_eq!(
        public_key_fingerprint(&public).unwrap(),
        public_key_fingerprint(&public).unwrap()
    );
}

#[test]
fn different_keys_have_different_fingerprints() {
    let (_, first) = generate_rsa_keypair().unwrap();
    let (_, second) = generate_rsa_keypair().unwrap();
    assert_ne!(
        public_key_fingerprint(&first).unwrap(),
        public_key_fingerprint(&second).unwrap()
    );
}

#[test]
fn matches_the_upstream_chef_fingerprint_vector() {
    // Public key and fingerprint from upstream chef-server-ctl key_control_spec.rb.
    let public_key = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAvPo+oNPB7uuNkws0fC02
KxSwdyqPLu0fhI1pOweNKAZeEIiEz2PkybathHWy8snSXGNxsITkf3eyvIIKa8OZ
WrlqpI3yv/5DOP8HTMCxnFuMJQtDwMcevlqebX4bCxcByuBpNYDcAHjjfLGSfMjn
E5lZpgYWwnpic4kSjYcL9ORK9nYvlWV9P/kCYmRhIjB4AhtpWRiOfY/TKi3P2LxT
IjSmiN/ihHtlhV/VSnBJ5PzT/lRknlrJ4kACoz7Pq9jv+aAx5ft/xE9yDa2DYs0q
Tfuc9dUYsFjptWYrV6pfEQ+bgo1OGBXORBFcFL+2D7u9JYquKrMgosznHoEkQNLo
0wIDAQAB
-----END PUBLIC KEY-----";

    assert_eq!(
        public_key_fingerprint(public_key).unwrap(),
        "12:3e:33:73:0b:f4:ec:72:dc:f0:4c:51:62:27:08:76:96:24:f4:4a"
    );
}

#[test]
fn invalid_pem_is_rejected() {
    assert!(public_key_fingerprint("not a public key").is_err());
}
