# TLS certificate reload report

## Scope

Rotate the TLS certificate and private key of a running `microwave-server`
without dropping the listener. Plain-HTTP serving behavior is unchanged; a
SIGHUP on a plain listener is a logged no-op.

## Mechanism

- `microwave-tls` now exposes `load_paths(cert, key)`. Startup `load()` and
  reloading share the same PEM parsing, key/certificate pair verification,
  error text, and ALPN setup (`h2`, `http/1.1`).
- `axum-server`'s `RustlsConfig` already stores the rustls `ServerConfig` in an
  `ArcSwap` and reads it per accepted connection. The server keeps a clone of
  that handle next to the acceptor and calls `reload_from_config()` only after
  the replacement pair validates. No listener restart, socket close, or
  in-flight connection reset occurs.
- On Unix the SIGHUP handler is installed synchronously at the top of the
  `Serve` branch, before database open, bootstrap, and `TcpListener::bind`.
  An early signal cannot hit the default terminate action. The handler is
  installed for plain-HTTP listeners too; it logs `TLS is disabled; nothing to
  reload` and continues. SIGHUP and timer reloads share a `tokio::sync::Mutex`,
  so they are serialized rather than racing reads and the `ArcSwap` update.
- `server.tls.reload_interval_seconds` (default `0`) starts a polling task.
  Its immediate first tick performs one unconditional reload, covering a
  rotation that landed during startup or bootstrap. Later ticks compare length
  plus modification time of both files, so unchanged files are not re-parsed
  and do not log.
- A failed reload logs
  `TLS certificate reload failed; keeping the previous certificate` with the
  configured paths and the validation error. The old `ServerConfig` stays in
  the `ArcSwap`. Errors never contain key material.
- The polling task records the observed stamps even after a failure, so a
  persistently invalid pair logs once per file change instead of once per
  tick. SIGHUP always retries immediately.

## Configuration and validation

| TOML | Environment | Default | Validation |
|---|---|---|---|
| `server.tls.reload_interval_seconds` | `MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS` | `0` | `0` disables polling. Non-zero requires TLS to be enabled and must be at most `604800` (seven days). Non-numeric values are rejected. |

`Config::validate()` runs for file, environment, `serve --check`, and
`check-config` paths. `serve --check` still validates the initial certificate
pair before touching the database.

## Operations

- systemd: the packaged unit has `ExecReload=/bin/kill -HUP $MAINPID`;
  `systemctl reload microwave-server` rotates the pair and is safe on plain
  HTTP as well.
- Docker: `docker kill --signal=HUP <container>`. PID 1 is the server because
  the image has no entrypoint wrapper. Bind mounts and projected volumes update
  in place; Docker secrets are snapshots and need container recreation.
- Kubernetes: update the projected/bind-mounted files and send SIGHUP, or
  enable polling. No restart is required.
- Replace each file via its own temporary file and rename. The two renames are
  not atomic as a pair: a racing reload can see a new certificate with the old
  key, fail validation, and keep the previous pair. Retry or wait for polling;
  the completed rotation is picked up.

## Tests

- `microwave-config`: defaults, TOML/env parsing, too-large interval,
  non-numeric env value, and interval without TLS.
- `microwave-server` process tests:
  - Generate two rcgen self-signed pairs. Start the real binary over TLS,
    capture the served leaf certificate's SHA-256 fingerprint with a rustls
    client, rotate both files, send `SIGHUP`, and assert the new fingerprint is
    served.
  - Replace the certificate with garbage, send `SIGHUP`, assert the failure
    log appears, the process is still alive, and the previous fingerprint is
    still served.
  - Start with `MW_SERVER__TLS__RELOAD_INTERVAL_SECONDS=1`, rotate the files
    without a signal, and assert the new fingerprint appears and the success
    log is written.
  - Delay startup with a synchronous keygen warm-up, wait for the handler
    installation log before the listener exists, send `SIGHUP`, and assert the
    process survives, serves the original certificate, and that the handler log
    precedes the listening log.
  - Start a plain-HTTP listener, send `SIGHUP` twice, assert it logs
    `TLS is disabled; nothing to reload`, stays alive, and keeps serving
    `/_status`.
  - `serve --check` accepts a valid interval and rejects an oversized interval
    or an interval without TLS.
- Existing TLS tests still cover ALPN/h2, HTTPS serving, plaintext rejection,
  and HTTP/2 URL authority.

## Verification

- `cargo fmt --all -- --check`
- `cargo test -p microwave-server -p microwave-api -p microwave-config -p microwave-tls`
- `cargo clippy -p microwave-server -p microwave-tls -p microwave-config --all-targets -- -D warnings`
- strict `chef-client` e2e: `MW_E2E_COMPAT=0 MW_E2E_SYNTH_V2=0 MW_SKIP_BUILD=1` — PASS
  (including validator registration)

## Remaining gaps

- SIGHUP handling is Unix-only; on other platforms use the polling interval.
- The interval watcher uses length plus modification time. Replacing file
  contents while preserving both is not detected; SIGHUP still reloads.
- Reload covers only the certificate/key pair. The rest of the configuration
  still requires a restart.
- No preflight expiry-window validation and no chef-client-over-HTTPS e2e.

## Review fix round

| # | Finding | Status | Change |
|---|---|---|---|
| 1 | SIGHUP handler registered after bootstrap/bind | ADDRESSED | The rustls handle, reload state, and SIGHUP handler are now created at the top of the `Serve` branch, immediately after TLS config loading and before database open, bootstrap, or `TcpListener::bind`. New process test signals after the handler-install log and before the listener exists, then verifies the handler log precedes the listening log. |
| 2 | Plain-HTTP deployments have no HUP handler but systemd advertises reload | ADDRESSED | The handler is installed for every listener. Without TLS it logs `TLS is disabled; nothing to reload` and continues. New test sends SIGHUP twice to a plain-HTTP listener and verifies it stays alive and keeps serving `/_status`. |
| 3 | Periodic reload can miss a rotation during startup | ADDRESSED | The polling task no longer skips its immediate first tick: it performs one unconditional `startup-check` reload, with file stamps captured before the priming read so any racing write triggers another reload. |
| 4 | Docs overclaim atomicity/startup safety | ADDRESSED | `docs/deployment.md` and this report now state that the two renames are not atomic as a pair, that a racing reload fails validation and keeps the old pair, that retries pick up the completed rotation, and that the handler is installed before bootstrap/bind and on plain HTTP. |
| 5 | Concurrent SIGHUP and interval reloads not serialized | ADDRESSED | SIGHUP and timer triggers share one `TlsReloader` with a `tokio::sync::Mutex`, so reload attempts execute one at a time. |
| 6 | Add startup/plain-listener tests | ADDRESSED | Added `sighup_during_startup_does_not_terminate` and `sighup_on_a_plain_http_listener_is_safe`; fingerprint rotation and invalid-replacement tests remain. |

### Verification (fix round)

- `cargo fmt --all -- --check`
- `cargo test -p microwave-server -p microwave-config -p microwave-tls` — 53 tests pass
- `cargo clippy -p microwave-server -p microwave-tls -p microwave-config --all-targets -- -D warnings`
- strict `chef-client` e2e: `MW_E2E_COMPAT=0 MW_E2E_SYNTH_V2=0 MW_SKIP_BUILD=1` — PASS
