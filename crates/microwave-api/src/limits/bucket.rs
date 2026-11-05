use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;
use tokio::sync::Semaphore;

/// How many shards every bucket map is split over.
const SHARDS: usize = 16;

/// Rate and burst of one bucket, in requests and requests per second.
#[derive(Clone, Copy, Debug)]
pub struct RateSpec {
    pub rate_per_second: f64,
    pub burst: f64,
}

/// A rejection from a bucket, carrying the whole seconds to the next token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BucketRejection {
    pub retry_after: u64,
}

struct Bucket {
    tokens: f64,
    burst: f64,
    updated: Instant,
}

impl Bucket {
    fn full(burst: f64, now: Instant) -> Self {
        Self {
            tokens: burst,
            burst,
            updated: now,
        }
    }

    fn try_take(&mut self, rate_per_second: f64, now: Instant) -> Result<(), BucketRejection> {
        let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate_per_second).min(self.burst);
        self.updated = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }
        let retry_after = ((1.0 - self.tokens) / rate_per_second).ceil().max(1.0) as u64;
        Err(BucketRejection { retry_after })
    }
}

struct Entry {
    bucket: Mutex<Bucket>,
    in_flight: Arc<Semaphore>,
    busy: AtomicUsize,
    last_seen: Mutex<Instant>,
}

impl Entry {
    fn new(burst: f64, in_flight: usize, now: Instant) -> Self {
        Self {
            bucket: Mutex::new(Bucket::full(burst, now)),
            in_flight: Arc::new(Semaphore::new(in_flight.min(Semaphore::MAX_PERMITS))),
            busy: AtomicUsize::new(0),
            last_seen: Mutex::new(now),
        }
    }

    fn busy(&self) -> usize {
        self.busy.load(Ordering::Relaxed)
    }

    fn last_seen(&self) -> Instant {
        *lock(&self.last_seen)
    }
}

/// A live claim on one bucket entry. Keeps the entry out of eviction until it
/// is dropped.
pub struct Slot(Arc<Entry>);

impl Slot {
    fn new(entry: Arc<Entry>) -> Self {
        entry.busy.fetch_add(1, Ordering::Relaxed);
        Self(entry)
    }

    /// Semaphore the caller waits on for an in-flight slot.
    pub fn in_flight(&self) -> Arc<Semaphore> {
        self.0.in_flight.clone()
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, Ordering::Relaxed);
    }
}

type Shard<K> = Mutex<HashMap<K, Arc<Entry>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn hash_of<K: Hash>(key: &K) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// A bounded map of bucket entries, split over shards so unrelated keys do not
/// contend on one lock.
///
/// Every shard holds at most `max_entries / 16` entries in the steady state.
/// When a shard grows past that, it evicts its oldest idle entries. Entries
/// with a live [`Slot`] are skipped, so a map with nothing idle may temporarily
/// grow past `max_entries` instead of doubling a limit with a fresh bucket.
pub struct ShardedMap<K> {
    shards: Box<[Shard<K>]>,
    shard_capacity: usize,
}

impl<K: Hash + Eq + Clone> ShardedMap<K> {
    pub fn new(max_entries: usize) -> Self {
        Self::with_shards(max_entries, SHARDS)
    }

    fn with_shards(max_entries: usize, shards: usize) -> Self {
        Self {
            shards: (0..shards)
                .map(|_| Mutex::new(HashMap::new()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            shard_capacity: max_entries.div_ceil(SHARDS).max(1),
        }
    }

    fn shard(&self, key: &K) -> &Mutex<HashMap<K, Arc<Entry>>> {
        &self.shards[(hash_of(key) as usize) % self.shards.len()]
    }

    /// Claim the entry for `key`, taking one token. The returned [`Slot`] must
    /// be held until the request is done; the caller still has to acquire the
    /// returned semaphore to count the request as in flight.
    pub fn acquire(
        &self,
        key: K,
        spec: RateSpec,
        in_flight: usize,
        now: Instant,
    ) -> Result<(Slot, Arc<Semaphore>), BucketRejection> {
        let mut shard = lock(self.shard(&key));
        let entry = shard
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Entry::new(spec.burst, in_flight, now)))
            .clone();
        *lock(&entry.last_seen) = now;

        if shard.len() > self.shard_capacity {
            self.evict(&mut shard, &key);
        }

        let slot = Slot::new(entry);
        let in_flight = slot.in_flight();
        drop(shard);

        let taken = lock(&slot.0.bucket).try_take(spec.rate_per_second, now);
        taken.map(|()| (slot, in_flight))
    }

    /// Drop roughly the oldest tenth of a shard, but only idle entries and
    /// never the key being acquired.
    fn evict(&self, shard: &mut HashMap<K, Arc<Entry>>, current: &K) {
        let candidates = shard.len() / 10;
        let candidates = candidates.max(1);
        let mut idle: Vec<(K, Instant)> = shard
            .iter()
            .filter(|(key, entry)| *key != current && entry.busy() == 0)
            .map(|(key, entry)| (key.clone(), entry.last_seen()))
            .collect();
        if idle.is_empty() {
            return;
        }
        idle.sort_by_key(|(_, seen)| *seen);
        for (key, _) in idle.into_iter().take(candidates) {
            shard.remove(&key);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.shards.iter().map(|shard| lock(shard).len()).sum()
    }

    #[cfg(test)]
    fn contains(&self, key: &K) -> bool {
        lock(self.shard(key)).contains_key(key)
    }

    #[cfg(test)]
    fn busy(&self, key: &K) -> usize {
        lock(self.shard(key))
            .get(key)
            .map(|entry| entry.busy())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn spec(rate_per_second: f64, burst: f64) -> RateSpec {
        RateSpec {
            rate_per_second,
            burst,
        }
    }

    #[test]
    fn burst_is_capped_and_rejection_names_a_retry_after() {
        let map = ShardedMap::<u32>::new(100);
        let spec = spec(1.0, 2.0);
        let now = Instant::now();
        assert!(map.acquire(1, spec, 1, now).is_ok());
        assert!(map.acquire(1, spec, 1, now).is_ok());
        let rejection = map.acquire(1, spec, 1, now).err().unwrap();
        assert!(rejection.retry_after >= 1);
        assert!(map
            .acquire(1, spec, 1, now + Duration::from_secs(3))
            .is_ok());
    }

    #[test]
    fn tokens_refill_continuously_up_to_the_burst() {
        let map = ShardedMap::<u32>::new(100);
        let spec = spec(2.0, 2.0);
        let now = Instant::now();
        assert!(map.acquire(1, spec, 1, now).is_ok());
        assert!(map.acquire(1, spec, 1, now).is_ok());
        assert!(map.acquire(1, spec, 1, now).is_err());

        // 250 ms at two tokens per second is half a token.
        let half = map
            .acquire(1, spec, 1, now + Duration::from_millis(250))
            .err()
            .unwrap();
        assert_eq!(half.retry_after, 1);

        // Another 250 ms completes the token.
        assert!(map
            .acquire(1, spec, 1, now + Duration::from_millis(500))
            .is_ok());

        // Refilling far past the burst caps the bucket.
        let later = now + Duration::from_secs(60);
        assert!(map.acquire(1, spec, 1, later).is_ok());
        assert!(map.acquire(1, spec, 1, later).is_ok());
        assert!(map.acquire(1, spec, 1, later).is_err());
    }

    #[test]
    fn eviction_removes_the_oldest_idle_entry_and_keeps_a_busy_one() {
        let map = ShardedMap::<u32>::with_shards(16, 1);
        let spec = spec(100.0, 100.0);
        let now = Instant::now();

        let busy = map.acquire(1, spec, 1, now).unwrap().0;
        drop(map.acquire(2, spec, 1, now));
        assert_eq!(map.len(), 2);

        let _new = map.acquire(3, spec, 1, now).unwrap().0;
        assert!(map.contains(&1));
        assert!(!map.contains(&2));
        drop(busy);
    }

    #[test]
    fn a_map_full_of_busy_entries_grows_past_max_entries() {
        let map = ShardedMap::<u32>::with_shards(1, 1);
        let spec = spec(100.0, 100.0);
        let now = Instant::now();

        let _a = map.acquire(1, spec, 1, now).unwrap().0;
        let _b = map.acquire(2, spec, 1, now).unwrap().0;
        let _c = map.acquire(3, spec, 1, now).unwrap().0;

        assert_eq!(map.len(), 3);
        assert!(map.len() > 1);
    }

    #[test]
    fn dropping_a_slot_releases_the_busy_marker() {
        let map = ShardedMap::<u32>::with_shards(16, 1);
        let spec = spec(100.0, 100.0);
        let now = Instant::now();

        let slot = map.acquire(7, spec, 1, now).unwrap().0;
        assert_eq!(map.busy(&7), 1);
        drop(slot);
        assert_eq!(map.busy(&7), 0);
    }
}
