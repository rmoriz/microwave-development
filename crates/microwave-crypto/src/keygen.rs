//! Pre-generated RSA keypair cache.
//!
//! RSA-2048 generation is CPU-bound and can take tens to hundreds of
//! milliseconds. API key creation paths should not make a client wait for a
//! fresh key, so this module keeps a bounded queue of ready-made PEM keypairs
//! that worker threads refill in the background.
//!
//! The cache never stores or logs key material anywhere except the in-memory
//! queue. Dropping the cache stops its workers.

use crate::error::CryptoError;
use crate::signature::generate_rsa_keypair;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

/// A PEM keypair: `(private_pem, public_pem)`.
pub type KeyPair = (String, String);

/// Snapshot of cache and worker state, suitable for health reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeygenStatus {
    /// Keypairs currently waiting in the cache.
    pub cached: usize,
    /// Configured maximum number of cached keypairs.
    pub capacity: usize,
    /// Background worker threads that started successfully.
    pub workers: usize,
    /// True when every requested worker started and no generation failed.
    pub worker_healthy: bool,
    /// Number of background generation failures since start.
    pub worker_failures: u64,
    /// Number of times `take()` had to generate a keypair inline.
    pub generated_on_demand: u64,
}

/// Bounded, thread-safe cache of pre-generated 2048-bit RSA keypairs.
pub struct KeygenCache {
    inner: Arc<Inner>,
}

struct Inner {
    queue: Mutex<VecDeque<KeyPair>>,
    available: Condvar,
    capacity: usize,
    started_workers: AtomicUsize,
    worker_healthy: AtomicBool,
    worker_failures: AtomicU64,
    generated_on_demand: AtomicU64,
    shutting_down: AtomicBool,
}

impl KeygenCache {
    /// Start a cache with room for `capacity` keypairs and `workers`
    /// background generators.
    ///
    /// The worker count is capped at the capacity so an empty cache never
    /// starts pointless threads. A capacity of zero disables background
    /// generation and makes every [`KeygenCache::take`] generate on demand.
    pub fn new(capacity: usize, workers: usize) -> Self {
        let workers = workers.min(capacity);
        let inner = Arc::new(Inner {
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            available: Condvar::new(),
            capacity,
            started_workers: AtomicUsize::new(0),
            // Healthy until a worker fails to start or a generation fails.
            // With zero workers generation is deliberately on demand.
            worker_healthy: AtomicBool::new(true),
            worker_failures: AtomicU64::new(0),
            generated_on_demand: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
        });

        for index in 0..workers {
            let worker = inner.clone();
            let spawned = thread::Builder::new()
                .name(format!("mw-keygen-{index}"))
                .spawn(move || worker_loop(worker));
            match spawned {
                Ok(_) => {
                    inner.started_workers.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    // Thread creation failures are rare, but they are a real
                    // health problem: record it rather than panicking at start.
                    inner.worker_healthy.store(false, Ordering::Relaxed);
                    inner.worker_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        Self { inner }
    }

    /// Number of keypairs currently cached.
    pub fn cached(&self) -> usize {
        lock(&self.inner.queue).len()
    }

    /// Configured cache capacity.
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// Return a cached keypair without generating, if one is available.
    pub fn try_take(&self) -> Option<KeyPair> {
        let pair = lock(&self.inner.queue).pop_front();
        if pair.is_some() {
            // A worker may be waiting for room.
            self.inner.available.notify_all();
        }
        pair
    }

    /// Return a cached keypair, or generate one on demand when the cache is
    /// empty. This never waits for a worker; the inline fallback is the only
    /// potentially slow path and should normally run on a blocking thread.
    pub fn take(&self) -> Result<KeyPair, CryptoError> {
        if let Some(pair) = self.try_take() {
            return Ok(pair);
        }
        self.inner
            .generated_on_demand
            .fetch_add(1, Ordering::Relaxed);
        generate_rsa_keypair()
    }

    /// Synchronously fill the cache up to its capacity.
    ///
    /// Returns the number of keypairs produced. Startup callers can use this
    /// for an optional warm-up; by default workers fill the cache in the
    /// background so startup does not wait for RSA generation.
    pub fn warm_up(&self) -> usize {
        let mut produced = 0;
        while self.cached() < self.inner.capacity {
            match generate_rsa_keypair() {
                Ok(pair) => {
                    if !push(&self.inner, pair) {
                        break;
                    }
                    produced += 1;
                }
                Err(_) => break,
            }
        }
        produced
    }

    /// Snapshot the cache and worker state.
    pub fn status(&self) -> KeygenStatus {
        KeygenStatus {
            cached: self.cached(),
            capacity: self.inner.capacity,
            workers: self.inner.started_workers.load(Ordering::Relaxed),
            worker_healthy: self.inner.worker_healthy.load(Ordering::Relaxed),
            worker_failures: self.inner.worker_failures.load(Ordering::Relaxed),
            generated_on_demand: self.inner.generated_on_demand.load(Ordering::Relaxed),
        }
    }
}

impl Drop for KeygenCache {
    fn drop(&mut self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        self.inner.available.notify_all();
    }
}

fn worker_loop(inner: Arc<Inner>) {
    loop {
        let mut queue = lock(&inner.queue);
        while queue.len() >= inner.capacity && !inner.shutting_down.load(Ordering::SeqCst) {
            queue = match inner.available.wait(queue) {
                Ok(queue) => queue,
                // A poisoned lock still contains a usable queue; recover it
                // rather than leaving the worker permanently wedged.
                Err(poisoned) => poisoned.into_inner(),
            };
        }
        if inner.shutting_down.load(Ordering::SeqCst) {
            break;
        }
        drop(queue);

        match generate_rsa_keypair() {
            Ok(pair) => {
                let mut queue = lock(&inner.queue);
                if queue.len() < inner.capacity {
                    queue.push_back(pair);
                }
            }
            Err(_) => {
                // Never log key material or detailed key errors here. Record
                // only the failure count and back off to avoid a hot loop.
                inner.worker_healthy.store(false, Ordering::Relaxed);
                inner.worker_failures.fetch_add(1, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
}

fn push(inner: &Inner, pair: KeyPair) -> bool {
    let mut queue = lock(&inner.queue);
    if queue.len() >= inner.capacity {
        return false;
    }
    queue.push_back(pair);
    true
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::KeygenCache;

    #[test]
    fn status_reports_configured_capacity() {
        let cache = KeygenCache::new(3, 1);
        let status = cache.status();
        assert_eq!(status.capacity, 3);
        assert_eq!(status.workers, 1);
        assert!(status.worker_healthy);
    }

    #[test]
    fn zero_capacity_never_starts_workers() {
        let cache = KeygenCache::new(0, 4);
        let status = cache.status();
        assert_eq!(status.capacity, 0);
        assert_eq!(status.workers, 0);
        assert_eq!(cache.cached(), 0);
    }
}
