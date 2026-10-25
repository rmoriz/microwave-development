use microwave_crypto::{parse_private_key_pem, parse_public_key_pem, KeygenCache, KeygenStatus};
use rsa::traits::PublicKeyParts;
use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

fn wait_for_cached(cache: &KeygenCache, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while cache.cached() < count {
        assert!(
            Instant::now() < deadline,
            "cache did not reach {count} keypairs in time (status: {:?})",
            cache.status()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_valid_keypair(pair: &(String, String)) {
    let private_key = parse_private_key_pem(&pair.0).expect("cached private key parses");
    let public_key = parse_public_key_pem(&pair.1).expect("cached public key parses");
    assert!(private_key.n().bits() >= 2048);
    assert!(public_key.n().bits() >= 2048);
}

#[test]
fn background_workers_fill_the_cache_with_distinct_valid_keypairs() {
    let cache = KeygenCache::new(2, 1);
    wait_for_cached(&cache, 2);

    let status: KeygenStatus = cache.status();
    assert_eq!(status.capacity, 2);
    assert_eq!(status.workers, 1);
    assert!(status.worker_healthy);
    assert_eq!(status.worker_failures, 0);
    assert_eq!(status.generated_on_demand, 0);

    let first = cache.take().unwrap();
    let second = cache.take().unwrap();
    assert_valid_keypair(&first);
    assert_valid_keypair(&second);
    assert_ne!(first.0, second.0, "cached private keys must be distinct");
    assert_ne!(first.1, second.1, "cached public keys must be distinct");
}

#[test]
fn take_generates_on_demand_when_the_cache_is_disabled() {
    let cache = KeygenCache::new(0, 0);
    assert_eq!(cache.cached(), 0);

    let pair = cache.take().unwrap();
    assert_valid_keypair(&pair);
    assert_eq!(cache.status().generated_on_demand, 1);
    assert_eq!(cache.status().capacity, 0);
}

#[test]
fn warm_up_fills_the_cache_synchronously() {
    let cache = KeygenCache::new(2, 0);
    assert_eq!(cache.warm_up(), 2);
    assert_eq!(cache.cached(), 2);
    assert_eq!(cache.warm_up(), 0, "a full cache is not refilled");
    assert_eq!(cache.status().generated_on_demand, 0);
}

#[test]
fn workers_are_capped_at_capacity() {
    let cache = KeygenCache::new(1, 8);
    assert_eq!(cache.status().workers, 1);
    wait_for_cached(&cache, 1);
    assert!(cache.status().worker_healthy);
}

#[test]
fn oversized_capacity_is_clamped_without_a_large_allocation() {
    let cache = KeygenCache::new(usize::MAX, 0);
    assert_eq!(
        cache.capacity(),
        microwave_crypto::keygen::MAX_CACHE_CAPACITY
    );
    assert_eq!(cache.status().workers, 0);
}

#[test]
fn concurrent_takers_receive_distinct_keypairs() {
    let cache = KeygenCache::new(1, 1);
    cache.warm_up();
    assert_eq!(cache.cached(), 1);

    let pairs = thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| scope.spawn(|| cache.take().unwrap()))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });

    let mut private_keys = HashSet::new();
    for pair in &pairs {
        assert_valid_keypair(pair);
        assert!(
            private_keys.insert(pair.0.clone()),
            "concurrent takers must not receive the same keypair"
        );
    }
    assert_eq!(private_keys.len(), 2);
}
