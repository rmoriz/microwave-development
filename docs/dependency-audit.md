# Dependency Audit — 2026-10-24

Scope: direct Rust dependencies, container base images, and the deployment
services in `docker-compose*.yml`. Vendored upstream checkouts (`chef/`,
`chef-server/`) and their Ruby gems are excluded.

## Applied upgrades

The following upgrades were applied and verified with the full workspace test
suite:

| Crate / image | Before | After | Notes |
| --- | ---: | ---: | --- |
| `base64` | 0.22.1 | 0.23.1 | No source changes required. |
| `bcrypt` | 0.17.1 | 0.19.3 | No source changes required. |
| `toml` | 0.8.23 | 1.1.8 | No source changes required. |
| `sqlx` | 0.8.6 | 0.9.0 | Dynamic SQL is wrapped in `sqlx::AssertSqlSafe`; PostgreSQL tests pass 7/7. |
| `reqwest` | 0.12.28 | 0.13.5 | Feature `rustls-tls` replaced by `rustls`. |
| `ureq` | 2.12.1 | 3.5.0 | Migrated to `Agent::config_builder`, `http::Response<Body>`, and status-as-value handling. |
| `rusqlite` | 0.32.1 | 0.39.0 | Newest version compatible with `sqlx`'s optional `sqlx-sqlite` (`libsqlite3-sys < 0.38`). |
| `rand` | 0.8.8 direct | removed direct | `microwave-crypto` now uses `rsa::rand_core::OsRng`; `rand` remains only as an `rsa` transitive dependency. |
| Rust builder | `rust:1-bookworm` | `rust:1-trixie` | Docker image builds successfully. |
| Runtime base | `debian:bookworm-slim` | `debian:trixie-slim` | Image smoke-tested with `microwave-ctl --version` and `microwave-server --version`. |
| Garage wrapper base | `debian:bookworm-slim` | `debian:trixie-slim` | Same Garage v2.5.0 binary. |

## Remaining version gaps

| Crate | Locked | Latest | Reason to stay |
| --- | ---: | ---: | --- |
| `rusqlite` | 0.39.0 | 0.41.0 | `rusqlite` 0.41 needs `libsqlite3-sys` 0.39, while `sqlx-sqlite` (optional, but part of Cargo's resolution) requires `< 0.38`. Revisit when `sqlx` relaxes that bound. |
| `sha1` | 0.10.7 | 0.11.0 | Coupled to `rsa`/`sha2` digest versions. |
| `sha2` | 0.10.9 | 0.11.1 | `rsa` 0.9.10 still uses `digest` 0.10. |
| `hmac` | 0.12.1 | 0.13.0 | Uses `digest` 0.11; must move with the hash stack. |
| `md-5` | 0.10.6 | 0.11.0 | Uses `digest` 0.11; must move with the hash stack. |
| `num-bigint-dig` | 0.8.6 | 0.9.1 | Must match the version used by `rsa` 0.9. |

`rsa` 0.9.10 is still the latest stable release. The whole crypto stack
(`rsa` + `sha1` + `sha2` + `hmac` + `md-5` + `num-bigint-dig`) should be
upgraded together once `rsa` supports `digest` 0.11.

Everything else in the direct set is current, including `axum`, `tokio`,
`serde`, `clap`, `tower`, `chrono`, `uuid`, `thiserror`, `tracing`,
`tracing-subscriber`, `subtle`, `tempfile`, and `assert_cmd`.

### Notes

- `Cargo.lock` may contain multiple versions of transitive crates (for example
  `base64` 0.22 and 0.23) because different dependencies pin different lines.
  That is normal.
- `sqlx` 0.9 requires dynamic SQL strings to be explicitly asserted safe. All
  call sites build SQL from internal table/column constants and use bind
  parameters for values, so `AssertSqlSafe` is appropriate.
- Security tooling (`cargo-audit`, `cargo-deny`, `cargo-outdated`) is not
  installed in this environment. Recommended CI addition:

  ```sh
  cargo install cargo-audit --locked
  cargo audit
  cargo install cargo-deny --locked
  cargo deny check advisories bans sources
  ```

## Container images

| Image | Current | Status |
| --- | --- | --- |
| `dxflrs/garage` | `v2.5.0` | Current; validated with the HA Compose profile. |
| `opensearchproject/opensearch` | `2.19.7` | Latest 2.x; validated. OpenSearch 3.9.0 exists but is a major upgrade and has not been validated with Chef tooling/our adapter. |
| `rust` builder | `rust:1-trixie` | Rust 1.99.x on Debian 13. |
| Runtime base | `debian:trixie-slim` | Debian 13; image builds and binaries start. |
| PostgreSQL test image | `postgres:18-alpine` | Used by `scripts/run-postgres-tests.sh`; 7/7 integration tests pass. The HA Compose service remains commented out until the API is wired to the PostgreSQL store. |

## Recommendations

1. **Keep the hash stack pinned** until `rsa` moves to `digest` 0.11; do not
   bump `sha2`/`hmac`/`md-5` independently.
2. **Retry `rusqlite` 0.41** when `sqlx` allows `libsqlite3-sys` 0.39 or when
   the optional `sqlx-sqlite` dependency no longer constrains resolution.
3. **Evaluate OpenSearch 3.9.0** separately; stay on the validated 2.19.7 until
   the adapter and Chef client paths are tested against 3.x.
4. **Add `cargo-audit`/`cargo-deny` to CI** and pin container images by digest
   for reproducible builds.
