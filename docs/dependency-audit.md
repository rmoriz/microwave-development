# Dependency Audit — 2026-10-24

Scope: direct Rust dependencies, container base images, and the deployment
services in `docker-compose*.yml`. Vendored upstream checkouts (`chef/`,
`chef-server/`) and their Ruby gems are excluded.

## Rust direct dependencies

`cargo update --dry-run` reports no updates inside the current semver
constraints. The following direct dependencies are behind the latest published
version and need an explicit version bump.

| Crate | Locked | Latest | Recommendation |
| --- | ---: | ---: | --- |
| `base64` | 0.22.1 | 0.23.1 | Low risk; the `Engine` API used here is stable. |
| `bcrypt` | 0.17.1 | 0.19.3 | Low risk; `hash`/`verify` API unchanged. |
| `toml` | 0.8.23 | 1.1.8 | Major bump; `from_str` usage is simple, test before merging. |
| `ureq` | 2.12.1 | 3.5.0 | Major bump; client/error API changed. Defer until the S3 tests are updated. |
| `reqwest` | 0.12.28 | 0.13.5 | Major bump; used by OpenSearch and migration. Defer. |
| `rusqlite` | 0.32.1 | 0.41.0 | Major bump; several API changes since 0.32. Defer and test the store suite. |
| `sqlx` | 0.8.6 | 0.9.0 | Major bump; PostgreSQL backend only. Defer and run the Docker tests. |
| `rand` | 0.8.8 | 0.10.3 | Major bump; RNG traits changed. Used for RSA key generation. Defer. |
| `sha1` | 0.10.7 | 0.11.0 | Coupled to `rsa`/`sha2` digest versions. Keep until `rsa` moves. |
| `sha2` | 0.10.9 | 0.11.1 | Coupled to `rsa` 0.9's `sha2` feature. Keep. |
| `hmac` | 0.12.1 | 0.13.0 | Uses `digest` 0.11; must move with `sha2` and `rsa`. Keep. |
| `md-5` | 0.10.6 | 0.11.0 | Uses `digest` 0.11; must move with the hash stack. Keep. |
| `num-bigint-dig` | 0.8.6 | 0.9.1 | Must match the version used by `rsa` 0.9. Keep. |

Everything else in the direct set is current, including `axum`, `tokio`,
`serde`, `clap`, `tower`, `sqlx`'s 0.8 line, `rsa`, `chrono`, `uuid`,
`thiserror`, `tracing`, `tracing-subscriber`, `subtle`, `tempfile`, and
`assert_cmd`.

### Notes

- `Cargo.lock` contains multiple versions of some crates (for example
  `base64` 0.22 and 0.23, `rand` 0.8 and 0.10) because transitive dependencies
  pin different lines. That is normal and not itself a defect.
- The hash stack (`sha1`, `sha2`, `hmac`, `md-5`, `num-bigint-dig`) should be
  upgraded together with `rsa`, not independently. `rsa` 0.9 still depends on
  `digest` 0.10.
- Security tooling (`cargo-audit`, `cargo-deny`, `cargo-outdated`) is not
  installed in this environment. `cargo update --dry-run` shows the lockfile is
  current within declared constraints, but it does not check advisories.
  Recommended CI addition:

  ```sh
  cargo install cargo-audit --locked
  cargo audit
  cargo install cargo-deny --locked
  cargo deny check advisories bans sources
  ```

## Container images

| Image | Current | Status |
| --- | --- | --- |
| `dxflrs/garage` | `v2.5.0` | Current; rebuilt and validated with the HA Compose profile. |
| `opensearchproject/opensearch` | `2.19.7` | Latest 2.x; validated. OpenSearch 3.9.0 exists but is a major upgrade and has not been validated with Chef tooling/our adapter. |
| `rust` builder | `rust:1-bookworm` (resolves to 1.99.x) | Rust is current. Debian `bookworm` is oldstable; `1-trixie` exists. |
| Runtime base | `debian:bookworm-slim` | Debian 12 is oldstable but supported until 2028. `trixie-slim` exists. |
| PostgreSQL placeholder | `postgres:16-alpine` (commented out) | Not wired to the server yet. PostgreSQL 18 is current; update when the metadata backend is enabled. |

### Recommendations

1. **Low-risk Rust bumps first:** `base64` 0.23 and `bcrypt` 0.19, with the full
   workspace suite.
2. **Plan a coupled crypto-stack upgrade** (`rsa` + `sha2`/`sha1` + `hmac` +
   `md-5` + `num-bigint-dig`) rather than bumping individual hash crates.
3. **Evaluate Debian trixie** for both builder and runtime in a separate PR;
   verify `sqlite3`, `curl`, and the static Garage binary still work.
4. **Evaluate OpenSearch 3.9.0** separately; stay on the validated 2.19.7 until
   the adapter and Chef client paths are tested against 3.x.
5. **Add `cargo-audit`/`cargo-deny` to CI** and pin container images by digest
   for reproducible builds.
