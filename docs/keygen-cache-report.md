# Keygen cache implementation report

Branch: `keygen-cache`

## Initial implementation (`d74a201`)

- Added a bounded, thread-safe `KeygenCache` in `microwave-crypto` with
  background workers, `try_take()`, on-demand `take()`, optional `warm_up()`,
  and status counters.
- Added `keygen.size`, `keygen.workers`, and `keygen.warm_up` configuration
  with `MW_KEYGEN__*` overrides.
- Wired the cache into `AppState` and API key creation for users, clients,
  organizations, and keys.
- Extended `/_status` with cache/worker state and documented the feature.

## Review fix round

| # | Finding | Status |
|---|---|---|
| 1 | OAuth key regeneration bypassed the cache and blocked the runtime | **ADDRESSED** |
| 2 | No configuration validation or upper bounds; huge `VecDeque` allocation could abort startup | **ADDRESSED** |
| 3 | Lost-wakeup race during cache shutdown | **ADDRESSED** |
| 4 | Worker health was not reset; warm-up failures were invisible | **ADDRESSED** |
| 5 | `notify_all` could make every worker generate after one take | **ADDRESSED** |
| 6 | Test fixtures spawned real default keygen workers | **ADDRESSED** |
| 7 | Missing concurrent-taker, blocking-fallback, and client/organization coverage | **ADDRESSED** |

### Finding details

1. `OAuthState` now carries the shared `Arc<KeygenCache>`. It exposes the same
   `try_take()` + `tokio::task::spawn_blocking(take)` helper, and
   `providers profile::regen_key` uses it. `ProvidesOAuthState for AppState`
   passes the cache through.
2. `KeygenConfig` validates `size <= 1024`, `workers <= 8`, `workers <= size`,
   and rejects `size == 0 && warm_up`. Validation runs for TOML, environment
   overrides, and `microwave-ctl check-config`. `KeygenCache::new` defensively
   clamps direct callers and uses `VecDeque::try_reserve` instead of
   `with_capacity`.
3. Shutdown sets `shutting_down` while holding the queue mutex, then notifies.
   Workers are joined on drop so none can leak.
4. A successful background generation resets `worker_healthy`. Warm-up
   failures increment a new `warm_up_failures` counter and mark health
   unhealthy. `worker_failures` is documented as cumulative; health reflects
   the latest generation outcome. `/_status` publishes `warm_up_failures`.
5. Cache takes use `notify_one`; shutdown still uses `notify_all` because every
   worker must exit.
6. `support::disable_keygen` is used by the OAuth fixture and PostgreSQL test
   fixtures. Cache-specific tests opt back in explicitly.
7. Added cache tests for concurrent takers and oversized-capacity clamping,
   plus API tests for the blocking-pool fallback, cache-backed organization and
   client key creation, and OAuth key regeneration.

### Verification

- `cargo fmt --all`
- `cargo test -p microwave-crypto -p microwave-api -p microwave-config -p microwave-oauth`
- `cargo clippy -p microwave-api -p microwave-crypto -p microwave-oauth --all-targets -- -D warnings`
- Strict `chef-client` e2e: `MW_E2E_COMPAT=0 MW_E2E_SYNTH_V2=0`
