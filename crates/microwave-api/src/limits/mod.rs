//! In-memory request limits.
//!
//! The limiter is deliberately process-local: replicas share nothing, so every
//! instance enforces its own admission, org and principal quotas.

pub mod bucket;
pub mod layer;
pub mod source;

pub use layer::{admission, AdmissionApplied};

use crate::Metrics;
use bucket::{BucketRejection, RateSpec, ShardedMap, Slot};
use microwave_config::LimitsConfig;
use microwave_store::{Org, Principal};
use source::IpKey;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio::time::timeout;

/// The limit that rejected a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Source,
    Org,
    Principal,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Source => "source",
            Self::Org => "org",
            Self::Principal => "principal",
        }
    }
}

/// A rejected acquisition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub scope: Scope,
    pub retry_after: u64,
}

/// A held request slot. Dropping it releases the in-flight capacity and
/// re-allows eviction of the bucket entry.
pub struct LimitPermit {
    _permit: Option<OwnedSemaphorePermit>,
    _slot: Option<Slot>,
    metrics: Option<Arc<Metrics>>,
    scope: Scope,
}

impl Drop for LimitPermit {
    fn drop(&mut self) {
        if let Some(metrics) = &self.metrics {
            metrics.adjust_limits_in_flight(self.scope.as_str(), -1);
        }
    }
}

impl LimitPermit {
    fn empty() -> Self {
        Self {
            _permit: None,
            _slot: None,
            metrics: None,
            scope: Scope::Global,
        }
    }
}

impl std::fmt::Debug for LimitPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LimitPermit")
            .field("scope", &self.scope)
            .finish()
    }
}

/// Process-local admission, source, org and principal limits.
pub struct Limiter {
    enabled: bool,
    queue_timeout: Duration,
    metrics: Arc<Metrics>,
    global: Arc<Semaphore>,
    source: ShardedMap<IpKey>,
    org: ShardedMap<String>,
    principal: ShardedMap<(String, String)>,
    org_defaults: microwave_config::LimitsOrgConfig,
    principal_defaults: microwave_config::LimitsPrincipalConfig,
    organizations: std::collections::BTreeMap<String, microwave_config::LimitsOrgOverride>,
    source_rate: RateSpec,
}

impl Limiter {
    pub fn new(config: &LimitsConfig, metrics: Arc<Metrics>) -> Self {
        Self {
            enabled: config.enabled,
            queue_timeout: Duration::from_millis(config.queue_timeout_ms),
            metrics,
            global: Arc::new(Semaphore::new(config.global.in_flight)),
            source: ShardedMap::new(config.max_entries),
            org: ShardedMap::new(config.max_entries),
            principal: ShardedMap::new(config.max_entries),
            org_defaults: config.org,
            principal_defaults: config.principal,
            organizations: config.organizations.clone(),
            source_rate: RateSpec {
                rate_per_second: config.source.rate_per_second as f64,
                burst: config.source.burst as f64,
            },
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Take one global in-flight slot, waiting up to the queue timeout.
    pub async fn global(&self) -> Result<LimitPermit, Rejection> {
        if !self.enabled {
            return Ok(LimitPermit::empty());
        }
        let permit = match self.global.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(TryAcquireError::Closed) => {
                return Err(Rejection {
                    scope: Scope::Global,
                    retry_after: 1,
                })
            }
            Err(TryAcquireError::NoPermits) => {
                self.wait(self.global.clone()).await?.ok_or(Rejection {
                    scope: Scope::Global,
                    retry_after: 1,
                })?
            }
        };
        Ok(self.held(None, Scope::Global, permit))
    }

    /// Take one token for a source address. The source bucket has no separate
    /// in-flight cap, so its semaphore is effectively unlimited.
    pub async fn source(&self, address: IpAddr) -> Result<LimitPermit, Rejection> {
        if !self.enabled {
            return Ok(LimitPermit::empty());
        }
        let slot = self
            .take(self.source.acquire(
                IpKey::from(address),
                self.source_rate,
                Semaphore::MAX_PERMITS,
                std::time::Instant::now(),
            ))
            .map_err(|retry_after| Rejection {
                scope: Scope::Source,
                retry_after,
            })?;
        self.enter(slot, Scope::Source).await
    }

    pub async fn org(&self, org: &Org) -> Result<LimitPermit, Rejection> {
        self.org_by_key(&org.id, Some(&org.name)).await
    }

    /// Org limit addressed by its internal key. `name` resolves an optional
    /// per-org override; it is `None` when the caller has no name at hand.
    pub async fn org_by_key(
        &self,
        key: &str,
        name: Option<&str>,
    ) -> Result<LimitPermit, Rejection> {
        if !self.enabled {
            return Ok(LimitPermit::empty());
        }
        let (spec, in_flight) = self.org_quota(name);
        let slot = self
            .take(
                self.org
                    .acquire(key.to_string(), spec, in_flight, std::time::Instant::now()),
            )
            .map_err(|retry_after| Rejection {
                scope: Scope::Org,
                retry_after,
            })?;
        self.enter(slot, Scope::Org).await
    }

    pub async fn principal(
        &self,
        org: &Org,
        principal: &Principal,
    ) -> Result<LimitPermit, Rejection> {
        if !self.enabled {
            return Ok(LimitPermit::empty());
        }
        let spec = RateSpec {
            rate_per_second: self.principal_defaults.rate_per_second as f64,
            burst: self.principal_defaults.burst as f64,
        };
        let slot = self
            .take(self.principal.acquire(
                (org.id.clone(), principal.actor_id.clone()),
                spec,
                self.principal_defaults.in_flight,
                std::time::Instant::now(),
            ))
            .map_err(|retry_after| Rejection {
                scope: Scope::Principal,
                retry_after,
            })?;
        self.enter(slot, Scope::Principal).await
    }

    fn org_quota(&self, name: Option<&str>) -> (RateSpec, usize) {
        let mut rate_per_second = self.org_defaults.rate_per_second;
        let mut burst = self.org_defaults.burst;
        let mut in_flight = self.org_defaults.in_flight;
        if let Some(over) = name.and_then(|name| self.organizations.get(name)) {
            if let Some(value) = over.rate_per_second {
                rate_per_second = value;
            }
            if let Some(value) = over.burst {
                burst = value;
            }
            if let Some(value) = over.in_flight {
                in_flight = value;
            }
        }
        (
            RateSpec {
                rate_per_second: rate_per_second as f64,
                burst: burst as f64,
            },
            in_flight,
        )
    }

    fn take(&self, result: Result<(Slot, Arc<Semaphore>), BucketRejection>) -> Result<Slot, u64> {
        match result {
            Ok((slot, _)) => Ok(slot),
            Err(rejection) => Err(rejection.retry_after),
        }
    }

    /// Wait for in-flight capacity of a bucket. A timed-out wait drops the
    /// slot, releasing the bucket for eviction.
    async fn enter(&self, slot: Slot, scope: Scope) -> Result<LimitPermit, Rejection> {
        let permit = match self.wait(slot.in_flight()).await? {
            Some(permit) => permit,
            None => {
                return Err(Rejection {
                    scope,
                    retry_after: 1,
                })
            }
        };
        Ok(self.held(Some(slot), scope, permit))
    }

    /// `Ok(None)` means the queue timeout elapsed, `Ok(Some)` is a permit.
    async fn wait(
        &self,
        semaphore: Arc<Semaphore>,
    ) -> Result<Option<OwnedSemaphorePermit>, Rejection> {
        match semaphore.clone().try_acquire_owned() {
            Ok(permit) => Ok(Some(permit)),
            Err(TryAcquireError::Closed) => Ok(None),
            Err(TryAcquireError::NoPermits) if self.queue_timeout.is_zero() => Ok(None),
            Err(TryAcquireError::NoPermits) => {
                Ok(timeout(self.queue_timeout, semaphore.acquire_owned())
                    .await
                    .ok()
                    .and_then(Result::ok))
            }
        }
    }

    fn held(&self, slot: Option<Slot>, scope: Scope, permit: OwnedSemaphorePermit) -> LimitPermit {
        self.metrics.adjust_limits_in_flight(scope.as_str(), 1);
        LimitPermit {
            _permit: Some(permit),
            _slot: slot,
            metrics: Some(self.metrics.clone()),
            scope,
        }
    }
}
