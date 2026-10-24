//! Compile-time proof that `SqliteStore` satisfies the complete
//! backend-neutral contract, plus a few calls through `dyn MetadataStore`.

use microwave_store::{MetadataStore, Org, SqliteStore};
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

fn assert_metadata_store<T: MetadataStore>() {}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = pin!(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("sqlite store futures must not yield"),
    }
}

#[test]
fn sqlite_store_implements_the_contract() {
    assert_metadata_store::<SqliteStore>();
}

#[test]
fn contract_methods_are_usable_through_a_trait_object() {
    let store: Box<dyn MetadataStore> = Box::new(SqliteStore::open_in_memory().unwrap());

    let org = block_on(store.create_org("acme", "Acme Corp")).unwrap();
    assert_eq!(org.name, "acme");

    let found = block_on(store.find_org_by_id(&org.id)).unwrap().unwrap();
    assert_eq!(found, org);
    assert_eq!(block_on(store.count_nodes()).unwrap(), 0);

    assert_eq!(block_on(store.get_kv("contract:key")).unwrap(), None);
    block_on(store.set_kv("contract:key", "value")).unwrap();
    assert_eq!(
        block_on(store.get_kv("contract:key")).unwrap().as_deref(),
        Some("value")
    );
    assert!(block_on(store.remove_kv("contract:key")).unwrap());

    block_on(store.set_secret("object_store", "secret", "hmac")).unwrap();
    assert_eq!(
        block_on(store.get_secret("object_store", "secret"))
            .unwrap()
            .as_deref(),
        Some("hmac")
    );
    block_on(store.remove_secret("object_store", "secret")).unwrap();
}

fn cookbook_store() -> (Arc<dyn MetadataStore>, Org) {
    let store: Arc<dyn MetadataStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
    let org = block_on(store.create_org("acme", "Acme Corp")).unwrap();
    block_on(store.create_container_for_org(&org.id, "cookbooks")).unwrap();
    (store, org)
}

fn create_cookbook(
    store: &Arc<dyn MetadataStore>,
    org: &Org,
    version: &str,
    checksums: &[String],
    frozen: bool,
) -> microwave_store::StoredCookbookVersion {
    block_on(store.create_cookbook_version(
        &org.id,
        "apt",
        version,
        "{}",
        checksums,
        frozen,
        &org.authz_id,
    ))
    .unwrap()
}

#[test]
fn metadata_store_is_usable_behind_arc() {
    let store: Arc<dyn MetadataStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
    let org = block_on(store.create_org("arc", "Arc Corp")).unwrap();
    assert_eq!(
        block_on(store.find_org_by_id(&org.id)).unwrap().unwrap(),
        org
    );
}

#[test]
fn cookbook_create_persists_frozen() {
    let (store, org) = cookbook_store();
    let created = create_cookbook(&store, &org, "1.0.0", &[], true);
    assert!(created.frozen);

    let found = block_on(store.find_cookbook_version(&org.id, "apt", "1.0.0"))
        .unwrap()
        .unwrap();
    assert!(found.frozen);
    assert_eq!(found.version, "1.0.0");

    let listed = block_on(store.list_cookbook_versions(&org.id)).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].frozen);
}

#[test]
fn cookbook_update_persists_frozen_and_returns_purged_checksums() {
    let (store, org) = cookbook_store();
    let created = create_cookbook(&store, &org, "1.0.0", &[], false);
    assert!(!created.frozen);

    let (updated, purged) = block_on(store.update_cookbook_version(
        &org.id,
        "apt",
        "1.0.0",
        "{\"updated\":true}",
        &[],
        true,
        &org.authz_id,
    ))
    .unwrap();
    assert!(updated.frozen);
    assert_eq!(updated.manifest, "{\"updated\":true}");
    assert!(purged.is_empty());

    let found = block_on(store.find_cookbook_version(&org.id, "apt", "1.0.0"))
        .unwrap()
        .unwrap();
    assert!(found.frozen);
}

#[test]
fn cookbook_delete_returns_deleted_record_and_purges_checksums() {
    let (store, org) = cookbook_store();
    let checksums = vec!["checksum-a".to_string()];
    let (sandbox_id, _) = block_on(store.create_sandbox(&org.id, &checksums)).unwrap();
    block_on(store.commit_sandbox(&org.id, &sandbox_id)).unwrap();
    assert!(block_on(store.find_checksum(&org.id, "checksum-a"))
        .unwrap()
        .is_some());

    let created = create_cookbook(&store, &org, "1.0.0", &checksums, true);
    let (deleted, purged) =
        block_on(store.delete_cookbook_version(&org.id, "apt", "1.0.0")).unwrap();

    assert_eq!(deleted.id, created.id);
    assert_eq!(deleted.name, "apt");
    assert_eq!(deleted.version, "1.0.0");
    assert_eq!(deleted.manifest, created.manifest);
    assert_eq!(deleted.checksums, checksums);
    assert!(deleted.frozen);
    assert_eq!(purged, checksums);

    assert_eq!(
        block_on(store.find_cookbook_version(&org.id, "apt", "1.0.0")).unwrap(),
        None
    );
    assert_eq!(
        block_on(store.find_checksum(&org.id, "checksum-a")).unwrap(),
        None
    );
}
