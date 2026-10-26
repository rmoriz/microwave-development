use microwave_objectstore::{LocalObjectStore, ObjectStore};
use std::sync::Arc;

fn store() -> Arc<dyn ObjectStore + Send + Sync> {
    let root = std::env::temp_dir().join(format!("mw-objectstore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    Arc::new(LocalObjectStore::new(
        root,
        "test-secret",
        "http://localhost:8000",
    ))
}

const HELLO_CHECKSUM: &str = "5d41402abc4b2a76b9719d911017c592";

#[test]
fn local_store_round_trip_through_the_trait() {
    let store = store();
    assert!(!store.exists("acme", HELLO_CHECKSUM));
    assert!(matches!(
        store.get("acme", HELLO_CHECKSUM),
        Err(microwave_objectstore::ObjectStoreError::NotFound)
    ));

    store.put("acme", HELLO_CHECKSUM, b"hello").unwrap();
    assert!(store.exists("acme", HELLO_CHECKSUM));
    assert_eq!(store.get("acme", HELLO_CHECKSUM).unwrap(), b"hello");

    assert!(store.delete("acme", HELLO_CHECKSUM).unwrap());
    assert!(!store.exists("acme", HELLO_CHECKSUM));
    assert!(!store.delete("acme", HELLO_CHECKSUM).unwrap());
}

#[test]
fn local_store_rejects_checksum_mismatch() {
    let store = store();
    assert!(matches!(
        store.put("acme", HELLO_CHECKSUM, b"not hello"),
        Err(microwave_objectstore::ObjectStoreError::ChecksumMismatch)
    ));
    assert!(!store.exists("acme", HELLO_CHECKSUM));
}

#[test]
fn local_store_presigned_url_uses_bookshelf_path_and_token() {
    let store = LocalObjectStore::new(
        std::env::temp_dir().join(format!("mw-objectstore-url-{}", std::process::id())),
        "test-secret",
        "http://localhost:8000",
    );
    let now = microwave_objectstore::unix_timestamp();
    let url = store.presigned_url("PUT", "acme", HELLO_CHECKSUM);
    assert_eq!(
        url.split('?').next().unwrap(),
        "http://localhost:8000/bookshelf/organizations/acme/checksums/5d41402abc4b2a76b9719d911017c592"
    );
    let query = url.split_once('?').unwrap().1;
    let token = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
        .expect("token parameter");
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    let expires_at = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("exp="))
        .expect("exp parameter")
        .parse::<u64>()
        .expect("exp is Unix seconds");
    assert!(
        expires_at >= now + microwave_objectstore::DEFAULT_PRESIGN_EXPIRY_SECONDS,
        "presigned URL must carry the default expiry"
    );
    assert!(store.verify("PUT", "acme", HELLO_CHECKSUM, expires_at, token));
}

#[test]
fn local_store_token_verification_is_constant_time_and_method_bound() {
    let expires_at = microwave_objectstore::unix_timestamp() + 900;
    let token = microwave_objectstore::bookshelf_token(
        "test-secret",
        "PUT",
        "acme",
        HELLO_CHECKSUM,
        expires_at,
    );
    assert!(microwave_objectstore::bookshelf_token_matches(
        "test-secret",
        "PUT",
        "acme",
        HELLO_CHECKSUM,
        expires_at,
        &token
    ));
    assert!(!microwave_objectstore::bookshelf_token_matches(
        "test-secret",
        "GET",
        "acme",
        HELLO_CHECKSUM,
        expires_at,
        &token
    ));
    assert!(!microwave_objectstore::bookshelf_token_matches(
        "test-secret",
        "PUT",
        "acme",
        HELLO_CHECKSUM,
        expires_at,
        "forged"
    ));
    assert!(
        !microwave_objectstore::bookshelf_token_matches(
            "test-secret",
            "PUT",
            "acme",
            HELLO_CHECKSUM,
            expires_at + 1,
            &token
        ),
        "the expiry is part of the signed payload"
    );
}

#[test]
fn local_store_rejects_expired_and_future_distant_tokens() {
    let now = microwave_objectstore::unix_timestamp();
    let store = LocalObjectStore::new(
        std::env::temp_dir().join(format!("mw-objectstore-exp-{}", std::process::id())),
        "test-secret",
        "http://localhost:8000",
    );

    for expires_at in [
        now.saturating_sub(1),
        now + microwave_objectstore::MAX_BOOKSHELF_TOKEN_LIFETIME_SECONDS + 1,
    ] {
        let token = store.token_with_expiry("GET", "acme", HELLO_CHECKSUM, expires_at);
        assert!(!store.verify("GET", "acme", HELLO_CHECKSUM, expires_at, &token));
    }
}

#[test]
fn local_store_presign_expiry_is_configurable() {
    let now = microwave_objectstore::unix_timestamp();
    let store = LocalObjectStore::new(
        std::env::temp_dir().join(format!("mw-objectstore-expiry-{}", std::process::id())),
        "test-secret",
        "http://localhost:8000",
    )
    .with_presign_expiry_seconds(120);
    assert_eq!(store.presign_expiry_seconds(), 120);
    let url = store.presigned_url("GET", "acme", HELLO_CHECKSUM);
    let expires_at = url.split("exp=").nth(1).unwrap().parse::<u64>().unwrap();
    assert!(expires_at >= now + 120);
    assert!(expires_at <= now + 120 + 5);
}
