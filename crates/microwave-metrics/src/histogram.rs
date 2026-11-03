//! Bounded latency histograms, one series map per instrument family.
//!
//! Every family keeps its series in a `RwLock<HashMap<..>>`. The hot path is
//! one read lock plus one bucket increment; a first insert re-checks under the
//! write lock. Once an instrument holds [`DEFAULT_SERIES_LIMIT`] (or
//! [`crate::REQUEST_SERIES_LIMIT`] for the request histogram) series, unknown
//! keys collapse onto an `_other` series instead of growing without bound.

use std::collections::HashMap;
use std::hash::Hash;
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

/// The fixed bucket array of one series.
///
/// `buckets` is cumulative: slot `i` counts every observation at or below
/// `LATENCY_BUCKETS_SECONDS[i]`, and the last slot is `+Inf`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Buckets {
    pub(crate) sum: f64,
    pub(crate) count: u64,
    pub(crate) buckets: Box<[u64]>,
}

impl Buckets {
    fn new() -> Self {
        Self {
            sum: 0.0,
            count: 0,
            buckets: vec![0; LATENCY_BUCKETS_SECONDS.len() + 1].into_boxed_slice(),
        }
    }

    fn record(&mut self, seconds: f64) {
        self.count += 1;
        self.sum += seconds;
        let index = LATENCY_BUCKETS_SECONDS
            .iter()
            .position(|bound| seconds <= *bound)
            .unwrap_or(LATENCY_BUCKETS_SECONDS.len());
        for slot in &mut self.buckets[index..] {
            *slot += 1;
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
        let mut series = self.series.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(buckets) = series.get_mut(&key) {
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

    pub(crate) fn snapshot(&self) -> Vec<(K, Buckets)> {
        self.series
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(key, buckets)| (key.clone(), buckets.clone()))
            .collect()
    }
}

/// A capped counter family. Counters carry no sum or buckets.
pub(crate) struct Counters<K> {
    limit: usize,
    series: RwLock<HashMap<K, u64>>,
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
            let mut series = self.series.write().unwrap_or_else(PoisonError::into_inner);
            if let Some(count) = series.get_mut(&key) {
                *count += 1;
                return;
            }
            let key = if series.len() >= self.limit {
                key.collapsed()
            } else {
                key
            };
            *series.entry(key).or_insert(0) += 1;
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<(K, u64)> {
        self.series
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(key, count)| (key.clone(), *count))
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
