//! Retry policy for the converge requests.
//!
//! `chef-client` retries a converge five times with an exponential backoff.
//! The same budget applies here: `200ms * 2^attempt` plus ±20 % jitter, on
//! `503`, `502`, `504`, `429` and transport errors. Every other `4xx` is
//! final, because a `403` or `412` will not turn green by waiting.
//!
//! A `429` `retry-after` is deliberately ignored in favour of the fixed
//! backoff: the load test compares runs against each other, and a server that
//! answers with wildly different header values would make the retry timing a
//! second variable. The fixed schedule keeps every run comparable.

use crate::LoadError;
use reqwest::StatusCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Retries after the first attempt, like `http_retry_count 5`.
pub const MAX_RETRIES: u32 = 5;

/// Base of the exponential backoff.
const BASE_DELAY: Duration = Duration::from_millis(200);

/// Body the server writes when the dependency solver is saturated.
const SOLVER_OVERLOADED: &[u8] = b"Dependency solver overloaded";

/// How many retries one request needed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetryCount(pub u32);

impl std::fmt::Display for RetryCount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// How many `503` responses reported an overloaded dependency solver.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SolverOverloaded(pub u32);

impl std::fmt::Display for SolverOverloaded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A finished attempt: status and body, retried by [`execute`] or returned.
#[derive(Debug)]
pub struct Attempt {
    pub status: StatusCode,
    pub body: Vec<u8>,
}

/// Whether `status` is worth another attempt.
pub fn should_retry(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// Delay before retry number `attempt` (zero based).
pub fn backoff(attempt: u32) -> Duration {
    let nominal = BASE_DELAY.saturating_mul(1u32 << attempt.min(8));
    let nanos = nominal.as_nanos();
    let spread = nanos / 5;
    if spread == 0 {
        return nominal;
    }
    // Clock nanos are enough entropy for a ±20 % spread and keep the crate
    // free of a random-number dependency.
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let offset = u128::from(seed) % (spread * 2 + 1);
    Duration::from_nanos((nanos - spread + offset) as u64)
}

/// Whether a `503` body names the solver.
pub fn is_solver_overload(body: &[u8]) -> bool {
    body.windows(SOLVER_OVERLOADED.len())
        .any(|window| window == SOLVER_OVERLOADED)
}

/// Run `attempt` until it yields a final status, retrying the policy statuses
/// and transport errors.
///
/// Returns the last attempt together with the number of retries and solver
/// overloads seen. A transport error that survives the whole budget is
/// returned as an error that carries both counters.
pub async fn execute<F, Fut>(
    mut attempt: F,
) -> Result<(Attempt, RetryCount, SolverOverloaded), LoadError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Attempt, LoadError>>,
{
    let mut retries = RetryCount::default();
    let mut solver = SolverOverloaded::default();

    for index in 0..=MAX_RETRIES {
        let last = index == MAX_RETRIES;
        match attempt().await {
            Ok(result) => {
                if result.status == StatusCode::SERVICE_UNAVAILABLE
                    && is_solver_overload(&result.body)
                {
                    solver.0 += 1;
                }
                if last || !should_retry(result.status) {
                    return Ok((result, retries, solver));
                }
            }
            Err(error) => {
                if last {
                    return Err(error.counted(retries, solver));
                }
            }
        }
        tokio::time::sleep(backoff(index)).await;
        retries.0 += 1;
    }

    unreachable!("the final iteration returns")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_transient_statuses_only() {
        for status in [429, 502, 503, 504] {
            assert!(
                should_retry(StatusCode::from_u16(status).unwrap()),
                "{status}"
            );
        }
        for status in [200, 403, 412, 500] {
            assert!(
                !should_retry(StatusCode::from_u16(status).unwrap()),
                "{status}"
            );
        }
    }

    #[test]
    fn backoff_doubles_within_twenty_percent() {
        for (attempt, nominal) in [200u64, 400, 800, 1_600, 3_200].into_iter().enumerate() {
            let delay = backoff(attempt as u32).as_millis() as u64;
            assert!(
                (nominal * 4 / 5..=nominal * 6 / 5).contains(&delay),
                "{delay}ms is not within ±20 % of {nominal}ms"
            );
        }
    }

    #[test]
    fn solver_overload_needs_the_marker_in_the_body() {
        assert!(is_solver_overload(b"Dependency solver overloaded"));
        assert!(is_solver_overload(
            b"{\"error\":\"Dependency solver overloaded\"}"
        ));
        assert!(!is_solver_overload(b"busy"));
    }
}
