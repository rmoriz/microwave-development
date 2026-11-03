//! Bounded latency histograms, one series map per instrument family.
//!
//! Every family keeps its series in a `RwLock<HashMap<..>>`. The hot path is
//! one read lock plus one bucket increment; a first insert re-checks under the
//! write lock. Once an instrument holds [`DEFAULT_SERIES_LIMIT`] (or
//! [`crate::REQUEST_SERIES_LIMIT`] for the request histogram) series, unknown
//! keys collapse onto an `_other` series instead of growing without bound.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock};
use std::time::Duration;

/// Upper histogram bounds in seconds.
///
/// These are the OpenTelemetry default boundaries, converted to seconds. The
/// final boundary is 10 s; observations above it land in the `+Inf` slot.
pub const LATENCY_BUCKETS_SECONDS: [f64; 15] = [
    0.0, 0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// Series cap for every instrument but the request histogram.
pub const DEFAULT_SERIES_LIMIT: usize = 2_000;

/// Series cap for the request histogram, which multiplies route by org.
pub const REQUEST_SERIES_LIMIT: usize = 10_000;

/// Label value that receives series beyond the cap.
pub const OVERFLOW_LABEL: &str = "_other";

/// The counters of one series.
///
/// The bucket counts are cumulative: slot `i` counts every observation at or
/// below `LATENCY_BUCKETS_SECONDS[i]`, and the last slot is `+Inf`.
///
/// Every counter is atomic, so a recording only needs the map's read lock and
/// can update the series in place.
#[derive(Debug)]
pub(crate) struct Buckets {
    sum_bits: AtomicU64,
    count: AtomicU64,
    buckets: Box<[AtomicU64]>,
}

/// A point-in-time copy of one [`Buckets`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BucketSnapshot {
    pub(crate) sum: f64,
    pub(crate) count: u64,
    pub(crate) buckets: Vec<u64>,
}

impl Buckets {
    fn new() -> Self {
        Self {
            sum_bits: AtomicU64::new(0.0_f64.to_bits()),
            count: AtomicU64::new(0),
            buckets: (0..=LATENCY_BUCKETS_SECONDS.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
        }
    }

    fn record(&self, seconds: f64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        let mut current = self.sum_bits.load(Ordering::Relaxed);
        loop {
            let next = (f64::from_bits(current) + seconds).to_bits();
            match self.sum_bits.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        let index = LATENCY_BUCKETS_SECONDS
            .iter()
            .position(|bound| seconds <= *bound)
            .unwrap_or(LATENCY_BUCKETS_SECONDS.len());
        for slot in &self.buckets[index..] {
            slot.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> BucketSnapshot {
        BucketSnapshot {
            sum: f64::from_bits(self.sum_bits.load(Ordering::Relaxed)),
            count: self.count.load(Ordering::Relaxed),
            buckets: self
                .buckets
                .iter()
                .map(|slot| slot.load(Ordering::Relaxed))
                .collect(),
        }
    }
}

/// A series struct doubles as the map key; the overflow form replaces its
/// unbounded label with [`OVERFLOW_LABEL`].
pub(crate) trait HistogramKey: Clone + Eq + Hash {
    fn collapsed(&self) -> Self;
}

/// Read access shared by every histogram series, used for rendering.
pub trait HistogramSeries {
    /// Label name/value pairs, at most one pair per series field.
    fn label_pairs(&self) -> Vec<(&'static str, String)>;
    fn count(&self) -> u64;
    fn sum(&self) -> f64;
    /// Cumulative bucket counts with the `+Inf` slot last.
    fn bucket_counts(&self) -> &[u64];
}

/// A capped histogram family.
pub(crate) struct Histograms<K> {
    limit: usize,
    series: RwLock<HashMap<K, Buckets>>,
}

impl<K: HistogramKey> Histograms<K> {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            series: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) fn record(&self, key: K, duration: Duration) {
        let seconds = duration.as_secs_f64();
        {
            let series = self.series.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(buckets) = series.get(&key) {
                buckets.record(seconds);
                return;
            }
        }
        let mut series = self.series.write().unwrap_or_else(PoisonError::into_inner);
        // Another thread may have inserted the key while we waited.
        if let Some(buckets) = series.get(&key) {
            buckets.record(seconds);
            return;
        }
        let key = if series.len() >= self.limit {
            key.collapsed()
        } else {
            key
        };
        series
            .entry(key)
            .or_insert_with(Buckets::new)
            .record(seconds);
    }

    pub(crate) fn snapshot(&self) -> Vec<(K, BucketSnapshot)> {
        self.series
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(key, buckets)| (key.clone(), buckets.snapshot()))
            .collect()
    }
}

/// A capped counter family. Counters carry no sum or buckets.
pub(crate) struct Counters<K> {
    limit: usize,
    series: RwLock<HashMap<K, AtomicU64>>,
}

impl<K: HistogramKey> Counters<K> {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            series: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) fn increment(&self, key: K) {
        {
            let series = self.series.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(count) = series.get(&key) {
                count.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        let mut series = self.series.write().unwrap_or_else(PoisonError::into_inner);
        // Another thread may have inserted the key while we waited.
        if let Some(count) = series.get(&key) {
            count.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let key = if series.len() >= self.limit {
            key.collapsed()
        } else {
            key
        };
        series
            .entry(key)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> Vec<(K, u64)> {
        self.series
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(key, count)| (key.clone(), count.load(Ordering::Relaxed)))
            .collect()
    }
}

/// Sort a rendered snapshot by its label values so output is stable.
pub(crate) fn sorted<S: HistogramSeries>(mut series: Vec<S>) -> Vec<S> {
    series.sort_unstable_by_key(|entry| {
        entry
            .label_pairs()
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>()
    });
    series
}
