# TLS listener report

## Initial implementation (`12dfa08`)

- Added `[server.tls]` (`enabled`, `cert_path`, `key_path`) and
  `MW_SERVER__TLS__*` overrides; plain HTTP remains the default.
- Added the `microwave-tls` crate: PEM loading, readability and pair
  validation via rustls, with errors that never include key material.
- `microwave-server serve` binds HTTPS with `axum-server` + rustls when TLS is
  configured; `serve --check` and `microwave-ctl check-config` validate first.
- Docker healthcheck accepts `MW_HEALTHCHECK_SCHEME`,
  `MW_HEALTHCHECK_HOST`, and `MW_HEALTHCHECK_INSECURE`.
- Tests cover config/env parsing, missing/mismatched pairs, and a real HTTPS
  smoke test that also proves plain HTTP is rejected on the TLS port.

## Review fix round

| # | Finding | Status | Change |
|---|---|---|---|
| 1 | API-generated URLs still use `http://` under native TLS | ADDRESSED | `helpers::base_url` falls back to `https` when `server.tls.is_enabled()` and no `X-Forwarded-Proto` is present. Proxies still override the scheme. Unit tests cover native TLS, plain HTTP, and forwarded-proto precedence. An API integration test covers `/users` and local `/bookshelf` sandbox URLs. |
| 2 | ALPN not configured | ADDRESSED | `microwave-tls` sets `alpn_protocols = [h2, http/1.1]` on the rustls server config. Unit test asserts the list. Manual probe: `curl --http2` negotiated HTTP/2 and `openssl s_client -alpn h2` reported `ALPN protocol: h2`. |
| 3 | `MW_HEALTHCHECK_INSECURE` treats any non-empty value as true | ADDRESSED | Docker healthcheck only adds `-k` for `1`, `true`, or `TRUE`; `0`, `false`, `yes`, and empty keep verification enabled. Documented. Shell expansion checked for every value. |
| 4 | Broken docs table row and inconsistent auto-enable wording | ADDRESSED | Split the merged troubleshooting row. Docs and the example config now say that setting either path enables TLS, the other is then required and validated, and `enabled = false` forces plain HTTP. |
| 5 | Test the plain HTTP listener after the TLS refactor | ADDRESSED | New process-level test starts `serve` without TLS, verifies `GET /_status` over HTTP, and verifies a TLS handshake to the same port fails. |

### Verification

- `cargo fmt --all -- --check`
- `cargo test -p microwave-api -p microwave-server -p microwave-config -p microwave-tls`
- `cargo clippy -p microwave-api -p microwave-server -p microwave-config -p microwave-tls --all-targets -- -D warnings`
- strict chef-client e2e: `MW_E2E_COMPAT=0 MW_E2E_SYNTH_V2=0` — PASS
- `docker build --check .` — no warnings
- ALPN probe: HTTP/2 negotiated; `openssl s_client -alpn h2` reports `h2`

### Remaining gaps

- Certificates loaded at startup can now be rotated via SIGHUP or polling; see
  [`tls-reload-report.md`](tls-reload-report.md).
- No preflight expiry-window validation (an expired pair loads and clients
  reject it at handshake).
- No chef-client-over-HTTPS e2e; clients must trust the issuing CA.
- No client-certificate (mTLS) support.

## Review fix round 2

| # | Finding | Status | Change |
|---|---|---|---|
| 1 | HTTP/2 requests carry `:authority` but no `Host`, so `base_url()` fell back to `server.bind` | ADDRESSED | New `middleware::ensure_host_header` runs before handlers and inserts `Host` from the request URI authority only when neither `Host` nor `X-Forwarded-Host` is present. HTTP/1.1 and proxy precedence are unchanged. |
| 2 | `tls_urls_test` only asserted the bind fallback | ADDRESSED | Tests now send absolute-form URIs with an authority distinct from the bind address and assert that authority is used. Added explicit `X-Forwarded-Host` precedence and HTTP/1.1 `Host` cases. |
| 3 | No real HTTP/2-over-TLS test | ADDRESSED | Process test starts `serve` with TLS, negotiates HTTP/2 with reqwest over TLS (asserts `Version::HTTP_2`), uses `chef.example.com:<port>` resolved to loopback, and asserts generated URLs use that authority. A second request asserts `X-Forwarded-Host` wins. |
| 4 | Listener tests did not clear `MW_CONFIG` | ADDRESSED | All three listener/process tests remove `MW_CONFIG` (and relevant `MW_SERVER__TLS__*` variables) from the child environment. |
| 5 | `PORT_LOCK` could stay poisoned after a panicking test | ADDRESSED | Both plain and TLS listener tests and the HTTP/2 test recover the guard with `unwrap_or_else(PoisonError::into_inner)`. |

### Verification (round 2)

- `cargo fmt --all -- --check`
- `cargo test -p microwave-api -p microwave-server -p microwave-tls` — passed
- `cargo clippy -p microwave-api -p microwave-server -p microwave-tls --all-targets -- -D warnings` — passed
- strict chef-client e2e: `MW_E2E_COMPAT=0 MW_E2E_SYNTH_V2=0` — PASS
- HTTP/2 process test reports `HTTP_2` and `https://chef.example.com:<port>/...`
