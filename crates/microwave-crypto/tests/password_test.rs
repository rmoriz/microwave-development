use microwave_crypto::{hash_password, needs_upgrade, verify_password, PasswordHash};

#[test]
fn hashes_and_verifies_bcrypt() {
    let hash = hash_password("correct horse battery staple").unwrap();
    assert!(verify_password("correct horse battery staple", &hash));
    assert!(!verify_password("wrong", &hash));
    assert!(!needs_upgrade(&hash));
}

#[test]
fn verifies_sha1_bcrypt_migration_type() {
    let hash = PasswordHash {
        hash: "$2y$12$zVarEPSYI6CcFMEAOIZDVOxw1z0kzJ49K5nU1mgPM9pf2ooj7zUF2".into(),
        salt: "mysalt".into(),
        hash_type: "SHA1-bcrypt".into(),
    };
    assert!(verify_password("secret", &hash));
    assert!(!verify_password("wrong", &hash));
    assert!(needs_upgrade(&hash));
}

#[test]
fn verifies_sha1_plus_bcrypt_migration_type() {
    let hash = PasswordHash {
        hash: "$2y$12$E/74LX6k2Oxa5Ak3VfOu9Oku.921BuB6C6KYa9dVKSn4p32mqapJi".into(),
        salt: "$2y$12$E/74LX6k2Oxa5Ak3VfOu9O\tmysalt".into(),
        hash_type: "sha1+bcrypt".into(),
    };
    assert!(verify_password("secret", &hash));
    assert!(!verify_password("wrong", &hash));
}

#[test]
fn verifies_legacy_plain_sha1_ec_and_osc() {
    let ec = PasswordHash {
        hash: "51bbc1a1cf81f721a87db4c4d054452dbe9ea19e".into(),
        salt: "mysalt".into(),
        hash_type: "ec".into(),
    };
    let osc = PasswordHash {
        hash: "485a402adb0453904334f76598eadef77b69ba42".into(),
        salt: "mysalt".into(),
        hash_type: "osc".into(),
    };
    assert!(verify_password("secret", &ec));
    assert!(verify_password("secret", &osc));
    assert!(!verify_password("wrong", &ec));
}

#[test]
fn unknown_hash_type_is_rejected() {
    let hash = PasswordHash {
        hash: "x".into(),
        salt: "y".into(),
        hash_type: "unknown".into(),
    };
    assert!(!verify_password("secret", &hash));
}

#[test]
fn empty_password_is_rejected_for_hashing() {
    assert!(hash_password("").is_err());
}
