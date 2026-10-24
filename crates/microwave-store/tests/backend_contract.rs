//! Compile-time proof that `SqliteStore` satisfies the complete
//! backend-neutral contract, plus a few calls through `dyn MetadataStore`.

use microwave_store::{MetadataStore, SqliteStore};
use std::future::Future;
use std::pin::pin;
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
