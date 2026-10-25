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
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Hard upper bound for the cache. Configuration rejects larger values; this
/// defensive cap keeps a direct caller from allocating an absurd queue. Keep
/// in sync with `microwave_config::KeygenConfig::MAX_SIZE`.
pub const MAX_CACHE_CAPACITY: usize = 1024;

/// Hard upper bound for background workers. Keep in sync with
/// `microwave_config::KeygenConfig::MAX_WORKERS`.
pub const MAX_WORKERS: usize = 8;

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
    /// Cumulative background generation failures since start. This counter
    /// does not reset; `worker_healthy` reflects the latest generation
    /// outcome.
    pub worker_failures: u64,
    /// Number of times synchronous `warm_up()` failed to generate a keypair.
    pub warm_up_failures: u64,
    /// Number of times `take()` had to generate a keypair inline.
    pub generated_on_demand: u64,
}

/// Bounded, thread-safe cache of pre-generated 2048-bit RSA keypairs.
pub struct KeygenCache {
    inner: Arc<Inner>,
    workers: Vec<JoinHandle<()>>,
}

struct Inner {
    queue: Mutex<VecDeque<KeyPair>>,
    available: Condvar,
    capacity: usize,
    started_workers: AtomicUsize,
    worker_healthy: AtomicBool,
    worker_failures: AtomicU64,
    warm_up_failures: AtomicU64,
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
    ///
    /// Values are defensively clamped to [`MAX_CACHE_CAPACITY`] and
    /// [`MAX_WORKERS`]. Configuration rejects those values before startup, so
    /// this only protects direct callers and typos from huge allocations.
    pub fn new(capacity: usize, workers: usize) -> Self {
        let capacity = capacity.min(MAX_CACHE_CAPACITY);
        let workers = workers.min(MAX_WORKERS).min(capacity);
        let mut queue = VecDeque::new();
        // Avoid `with_capacity`'s abort-on-allocation-failure behavior. The
        // queue grows normally if this reservation fails.
        let _ = queue.try_reserve(capacity);
        let inner = Arc::new(Inner {
            queue: Mutex::new(queue),
            available: Condvar::new(),
            capacity,
            started_workers: AtomicUsize::new(0),
            // Healthy until a worker fails to start or a generation fails.
            // With zero workers generation is deliberately on demand.
            worker_healthy: AtomicBool::new(true),
            worker_failures: AtomicU64::new(0),
            warm_up_failures: AtomicU64::new(0),
            generated_on_demand: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
        });

        let mut handles = Vec::with_capacity(workers);
        for index in 0..workers {
            let worker = inner.clone();
            let spawned = thread::Builder::new()
                .name(format!("mw-keygen-{index}"))
                .spawn(move || worker_loop(worker));
            match spawned {
                Ok(handle) => {
                    inner.started_workers.fetch_add(1, Ordering::Relaxed);
                    handles.push(handle);
                }
                Err(_) => {
                    // Thread creation failures are rare, but they are a real
                    // health problem: record it rather than panicking at start.
                    inner.worker_healthy.store(false, Ordering::Relaxed);
                    inner.worker_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        Self {
            inner,
            workers: handles,
        }
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
            // Wake exactly one worker. Waking every worker after a single take
            // would make them all race to generate a keypair.
            self.inner.available.notify_one();
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
    /// background so startup does not wait for RSA generation. A generation
    /// failure is counted in `warm_up_failures` and marks worker health
    /// unhealthy so `/_status` exposes it.
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
                Err(_) => {
                    self.inner.warm_up_failures.fetch_add(1, Ordering::Relaxed);
                    self.inner.worker_healthy.store(false, Ordering::Relaxed);
                    break;
                }
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
            warm_up_failures: self.inner.warm_up_failures.load(Ordering::Relaxed),
            generated_on_demand: self.inner.generated_on_demand.load(Ordering::Relaxed),
        }
    }
}

impl Drop for KeygenCache {
    fn drop(&mut self) {
        // Set the flag while holding the queue mutex. A worker checks the flag
        // and enters `wait` under the same mutex, so this ordering cannot lose
        // the wakeup between the check and the wait.
        {
            let _queue = lock(&self.inner.queue);
            self.inner.shutting_down.store(true, Ordering::SeqCst);
        }
        self.inner.available.notify_all();
        // Join the workers so a dropped cache cannot leave detached
        // generators behind. Generation is CPU-bounded, so this is bounded by
        // one RSA key generation per worker.
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
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
                inner.worker_healthy.store(true, Ordering::Relaxed);
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
