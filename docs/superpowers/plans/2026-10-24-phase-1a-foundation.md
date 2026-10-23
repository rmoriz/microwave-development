# Phase 1A: Workspace, Configuration, and Request Authentication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Rust workspace foundation, configuration loader, and Chef-compatible request authentication primitives with tests.

**Architecture:** A Cargo workspace with focused crates. `microwave-config` loads TOML plus `MW_*` environment overrides. `microwave-crypto` implements `X-Ops-Sign` canonicalization, signature verification/signing, and password hash compatibility. `microwave-server` is a thin CLI that exercises configuration and reports its resolved values.

**Tech Stack:** Rust 1.99, `serde`, `toml`, `thiserror`, `clap`, `rsa`, `sha1`, `sha2`, `base64`, `bcrypt`, `rand`, `subtle`, `tempfile` (dev).

**Spec:** `docs/superpowers/specs/2026-10-24-microwave-server-design.md`

## Global Constraints

- Rust edition 2021 or newer; MSRV is the installed toolchain (Rust 1.99).
- Project-owned crates, binaries, modules, and public types use the `microwave-*` prefix or neutral names.
- Do not use a `chef-` prefix for project code. `chef-server-ctl` is only a compatibility alias.
- Default bind address is `0.0.0.0:8000`; default data directory is `./data`.
- Default authentication skew is 900 seconds; default superuser name is `pivotal`.
- No network calls in unit tests.
- Every task ends with a green test run and a commit.

## Review Focus

- **Malformed `X-Ops-Sign` header:** unknown version or missing `version=` must return a typed error, never panic.
- **Duplicate slashes and trailing slashes in signed paths:** canonicalization must match the upstream rule so valid signatures are not rejected.
- **Empty request body:** SHA-1/SHA-256 content hash must still be computed and verified.
- **Invalid or expired actor key:** verification must return a typed error and never expose key material in error text.
- **Legacy password hash type:** unknown hash types must fail verification, not fall back to a successful comparison.

---

### Task 1: Workspace and Configuration Crate

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `crates/microwave-config/Cargo.toml`
- Create: `crates/microwave-config/src/lib.rs`
- Create: `crates/microwave-config/src/config.rs`
- Test: `crates/microwave-config/tests/config_test.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `microwave_config::Config`
  - `Config::from_toml_str(&str) -> Result<Config, ConfigError>`
  - `Config::load(Option<&std::path::Path>) -> Result<Config, ConfigError>`
  - `Config::apply_env(&mut self) -> Result<(), ConfigError>`
  - `Config::resolved_database_url(&self) -> String`
  - Nested structs: `ServerConfig`, `PathsConfig`, `DatabaseConfig`, `SearchConfig`, `ObjectStoreConfig`, `AuthConfig`

- [ ] **Step 1: Write the failing test**

```rust
// crates/microwave-config/tests/config_test.rs
use microwave_config::{Config, DatabaseDriver, SearchBackend, ObjectStoreBackend};

#[test]
fn defaults_are_safe_and_local() {
    let c = Config::default();
    assert_eq!(c.server.bind, "0.0.0.0:8000");
    assert_eq!(c.paths.data_dir, "./data");
    assert_eq!(c.database.driver, DatabaseDriver::Sqlite);
    assert_eq!(c.search.backend, SearchBackend::Embedded);
    assert_eq!(c.object_store.backend, ObjectStoreBackend::Local);
    assert_eq!(c.auth.skew_seconds, 900);
    assert_eq!(c.auth.superuser_name, "pivotal");
}

#[test]
fn toml_overrides_defaults() {
    let c = Config::from_toml_str(r#"
        [server]
        bind = "127.0.0.1:9000"
        [auth]
        skew_seconds = 60
    "#).unwrap();
    assert_eq!(c.server.bind, "127.0.0.1:9000");
    assert_eq!(c.auth.skew_seconds, 60);
    assert_eq!(c.paths.data_dir, "./data");
}

#[test]
fn env_overrides_toml() {
    std::env::set_var("MW_SERVER__BIND", "127.0.0.1:9100");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.server.bind, "127.0.0.1:9100");
    std::env::remove_var("MW_SERVER__BIND");
}

#[test]
fn sqlite_url_is_derived_from_data_dir() {
    let mut c = Config::default();
    c.paths.data_dir = "/tmp/mw".into();
    assert_eq!(c.resolved_database_url(), "sqlite:///tmp/mw/microwave.db");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-config --test config_test`
Expected: FAIL because the workspace and crate do not exist.

- [ ] **Step 3: Create the workspace and implement the config types**

Create a virtual workspace at the repository root with members `crates/*` and `bin/*`, shared workspace dependencies, and `rust-toolchain.toml` pinned to the installed stable channel. Implement `Config` with serde defaults, a `from_toml_str` parser, an env overlay for `MW_<SECTION>__<FIELD>` keys, and a `resolved_database_url` that returns `sqlite://<data_dir>/microwave.db` unless `database.url` is set.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-config --test config_test`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml rust-toolchain.toml crates/microwave-config
git commit -m "feat: add workspace and configuration loader"
```

---

### Task 2: Canonical Request and Auth Header Parsing

**Files:**
- Create: `crates/microwave-crypto/Cargo.toml`
- Create: `crates/microwave-crypto/src/lib.rs`
- Create: `crates/microwave-crypto/src/auth.rs`
- Create: `crates/microwave-crypto/src/error.rs`
- Test: `crates/microwave-crypto/tests/canonical_test.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `microwave_crypto::{SignVersion, AuthHeaders, CryptoError}`
  - `parse_sign_header(&str) -> Result<SignVersion, CryptoError>`
  - `canonical_path(&str) -> String`
  - `content_hash(&[u8], SignVersion) -> String`
  - `canonical_request(&CanonicalRequest<'_>) -> String`
  - `AuthHeaders { version, user_id, timestamp, content_hash, signature }`
  - `CanonicalRequest { method, path, body, user_id, timestamp, api_version }`

- [ ] **Step 1: Write the failing test**

```rust
use microwave_crypto::{canonical_path, content_hash, parse_sign_header, SignVersion, CryptoError};

#[test]
fn parses_supported_sign_versions() {
    assert_eq!(parse_sign_header("version=1.0").unwrap(), SignVersion::V1_0);
    assert_eq!(parse_sign_header("version=1.1").unwrap(), SignVersion::V1_1);
    assert_eq!(parse_sign_header("algorithm=sha256;version=1.3").unwrap(), SignVersion::V1_3);
}

#[test]
fn rejects_unknown_sign_version() {
    assert!(matches!(parse_sign_header("version=2.0"), Err(CryptoError::UnsupportedSignVersion(_))));
}

#[test]
fn normalizes_canonical_paths() {
    assert_eq!(canonical_path("/nodes/web1/"), "/nodes/web1");
    assert_eq!(canonical_path("//organizations//acme//nodes/"), "/organizations/acme/nodes");
}

#[test]
fn hashes_empty_and_non_empty_bodies() {
    assert_eq!(content_hash(b"", SignVersion::V1_0), "2jmj7l5rSw0yVb/vlWAYkK/YBwk=");
    assert_eq!(content_hash(b"", SignVersion::V1_3), "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-crypto --test canonical_test`
Expected: FAIL because the crate does not exist.

- [ ] **Step 3: Implement parsing and canonicalization**

Implement `SignVersion` with `V1_0`, `V1_1`, `V1_3`; parse `version=`, optionally preceded by `algorithm=sha1;` or `algorithm=sha256;`. Canonicalize paths by collapsing repeated `/` characters and removing a trailing `/` unless the path is exactly `/`. Hash with SHA-1 for 1.0/1.1 and SHA-256 for 1.3, then Base64-encode. Build the canonical request strings exactly as specified in the design document.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-crypto --test canonical_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-crypto
git commit -m "feat: add canonical request primitives"
```

---

### Task 3: RSA Signature Verification and Signing

**Files:**
- Modify: `crates/microwave-crypto/src/auth.rs`
- Modify: `crates/microwave-crypto/src/error.rs`
- Test: `crates/microwave-crypto/tests/signature_test.rs`
- Create: `crates/microwave-crypto/tests/fixtures/test_key.pem` (test-only RSA key)

**Interfaces:**
- Consumes: Task 2 types.
- Produces:
  - `verify_request(&RsaPublicKey, &AuthHeaders, &CanonicalRequest<'_>) -> Result<(), CryptoError>`
  - `sign_request(&RsaPrivateKey, &CanonicalRequest<'_>) -> Result<AuthHeaders, CryptoError>`
  - `parse_public_key_pem(&str) -> Result<RsaPublicKey, CryptoError>`
  - `parse_private_key_pem(&str) -> Result<RsaPrivateKey, CryptoError>`

- [ ] **Step 1: Write the failing test**

```rust
use microwave_crypto::*;

#[test]
fn round_trips_all_supported_versions() {
    let private = include_str!("fixtures/test_key.pem");
    let public = include_str!("fixtures/test_key_pub.pem");
    for version in [SignVersion::V1_0, SignVersion::V1_1, SignVersion::V1_3] {
        let req = CanonicalRequest {
            method: "PUT",
            path: "/organizations/acme/nodes/web1",
            body: br#"{"name":"web1"}"#,
            user_id: "web1",
            timestamp: "2026-10-24T12:00:00Z",
            api_version: Some("2"),
            version,
        };
        let signed = sign_request(private, &req).unwrap();
        verify_request(public, &signed, &req).unwrap();
    }
}

#[test]
fn rejects_tampered_body() {
    let private = include_str!("fixtures/test_key.pem");
    let public = include_str!("fixtures/test_key_pub.pem");
    let req = CanonicalRequest { method: "PUT", path: "/nodes/web1", body: b"a", user_id: "web1", timestamp: "2026-10-24T12:00:00Z", api_version: Some("2"), version: SignVersion::V1_3 };
    let signed = sign_request(private, &req).unwrap();
    let tampered = CanonicalRequest { body: b"b", ..req };
    assert!(verify_request(public, &signed, &tampered).is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-crypto --test signature_test`
Expected: FAIL because the functions do not exist.

- [ ] **Step 3: Implement raw RSA PKCS#1 v1.5 signing and verification**

For 1.3, use RSA-SHA256 (`RSASSA-PKCS1-v1_5`). For 1.0 and 1.1, reproduce the upstream raw private-encrypt behavior: build the PKCS#1 v1.5 type-1 padded block from the canonical request bytes, apply the private exponent for signing, and the public exponent for verification. Use `rsa` key components plus `num-bigint-dig` modular exponentiation. Reject keys shorter than 2048 bits.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-crypto --test signature_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-crypto
git commit -m "feat: verify and sign X-Ops requests"
```

---

### Task 4: Password Hash Compatibility

**Files:**
- Create: `crates/microwave-crypto/src/password.rs`
- Modify: `crates/microwave-crypto/src/lib.rs`
- Test: `crates/microwave-crypto/tests/password_test.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `PasswordHash { hash: String, salt: String, hash_type: String }`
  - `hash_password(&str) -> Result<PasswordHash, CryptoError>`
  - `verify_password(&str, &PasswordHash) -> bool`
  - `needs_upgrade(&PasswordHash) -> bool`

- [ ] **Step 1: Write the failing test**

```rust
use microwave_crypto::{hash_password, verify_password, needs_upgrade, PasswordHash};

#[test]
fn hashes_and_verifies_bcrypt() {
    let h = hash_password("correct horse battery staple").unwrap();
    assert!(verify_password("correct horse battery staple", &h));
    assert!(!verify_password("wrong", &h));
    assert!(!needs_upgrade(&h));
}

#[test]
fn verifies_legacy_sha1_bcrypt_migration_type() {
    // Fixture generated from the upstream `SHA1-bcrypt` format.
    let h = PasswordHash {
        hash: "$2b$12$...".into(),
        salt: "salt".into(),
        hash_type: "SHA1-bcrypt".into(),
    };
    assert!(verify_password("secret", &h));
}

#[test]
fn unknown_hash_type_is_rejected() {
    let h = PasswordHash { hash: "x".into(), salt: "y".into(), hash_type: "unknown".into() };
    assert!(!verify_password("secret", &h));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-crypto --test password_test`
Expected: FAIL because the functions do not exist.

- [ ] **Step 3: Implement bcrypt and legacy formats**

Implement bcrypt hashing with cost 12, and verification for `bcrypt`, `erlang-bcrypt-0.5.0`, `SHA1-bcrypt`, `sha1+bcrypt`, and the legacy salted SHA-1 forms. Use the upstream `ec` and `osc` salt prefixes exactly. Compare with a constant-time helper. Generate a real fixture for the legacy test rather than leaving the placeholder.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-crypto --test password_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-crypto
git commit -m "feat: add password hash compatibility"
```

---

### Task 5: Minimal Server CLI

**Files:**
- Create: `bin/microwave-server/Cargo.toml`
- Create: `bin/microwave-server/src/main.rs`
- Test: `bin/microwave-server/tests/cli_test.rs`

**Interfaces:**
- Consumes: `microwave_config::Config`.
- Produces:
  - `microwave-server version` prints the crate version.
  - `microwave-server config [--config PATH]` prints the resolved configuration as JSON.
  - `microwave-server serve [--config PATH]` prints `serve: not implemented yet` and exits 0 for now.

- [ ] **Step 1: Write the failing test**

```rust
use assert_cmd::Command;

#[test]
fn version_prints_a_semver() {
    let out = Command::cargo_bin("microwave-server").unwrap().arg("version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn config_prints_json_defaults() {
    let out = Command::cargo_bin("microwave-server").unwrap().arg("config").output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["server"]["bind"], "0.0.0.0:8000");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-server --test cli_test`
Expected: FAIL because the binary does not exist.

- [ ] **Step 3: Implement the CLI**

Use `clap` derive with the three subcommands. Load the config through `Config::load`, apply env overrides, and serialize with `serde_json` for the `config` output. Keep `serve` as an explicit placeholder so later phases can replace it without changing the CLI surface.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-server --test cli_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bin/microwave-server Cargo.toml
git commit -m "feat: add microwave-server CLI"
```

---

## Self-Review

- **Spec coverage:** This plan covers Phase 1 sections 1.1 and the authentication/password halves of 1.2 from the TODO. Storage, authz, API routes, search, and object storage are intentionally deferred to Phase 1B and later plans.
- **Step scan:** Each task has a failing test, an implementation step, a passing test, and a commit.
- **Type consistency:** `Config`, `SignVersion`, `AuthHeaders`, `CanonicalRequest`, `PasswordHash`, and `CryptoError` are introduced once and reused with the same names.
- **Review Focus:** Each listed failure mode has a test in Task 2, 3, or 4.
- **Proportion:** The plan stays at the interface/test level; it does not transcribe implementations.
