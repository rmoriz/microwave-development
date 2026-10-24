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
    let url = store().presigned_url("PUT", "acme", HELLO_CHECKSUM);
    assert_eq!(
        url.split('?').next().unwrap(),
        "http://localhost:8000/bookshelf/organizations/acme/checksums/5d41402abc4b2a76b9719d911017c592"
    );
    let token = url.split("token=").nth(1).unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn local_store_token_verification_is_constant_time_and_method_bound() {
    let token =
        microwave_objectstore::bookshelf_token("test-secret", "PUT", "acme", HELLO_CHECKSUM);
    assert!(microwave_objectstore::bookshelf_token_matches(
        "test-secret",
        "PUT",
        "acme",
        HELLO_CHECKSUM,
        &token
    ));
    assert!(!microwave_objectstore::bookshelf_token_matches(
        "test-secret",
        "GET",
        "acme",
        HELLO_CHECKSUM,
        &token
    ));
    assert!(!microwave_objectstore::bookshelf_token_matches(
        "test-secret",
        "PUT",
        "acme",
        HELLO_CHECKSUM,
        "forged"
    ));
}
