# TLS certificate reload report

## Scope

Rotate the TLS certificate and private key of a running `microwave-server`
without dropping the listener. Plain HTTP and all non-TLS behavior are
unchanged.

## Mechanism

- `microwave-tls` now exposes `load_paths(cert, key)`. Startup `load()` and
  reloading share the same PEM parsing, key/certificate pair verification,
  error text, and ALPN setup (`h2`, `http/1.1`).
- `axum-server`'s `RustlsConfig` already stores the rustls `ServerConfig` in an
  `ArcSwap` and reads it per accepted connection. The server keeps a clone of
  that handle next to the acceptor and calls `reload_from_config()` only after
  the replacement pair validates. No listener restart, socket close, or
  in-flight connection reset occurs.
- On Unix the watcher registers `SIGHUP` synchronously before serving starts
  and reloads on every signal. The spawn path is separate, so a signal sent
  immediately after startup cannot hit the default terminate action.
- `server.tls.reload_interval_seconds` (default `0`) starts a polling task.
  It compares length plus modification time of both files, so unchanged files
  are not re-parsed and do not log. The first tick is skipped because startup
  already installed the initial pair.
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

- systemd: the packaged unit now has `ExecReload=/bin/kill -HUP $MAINPID`;
  `systemctl reload microwave-server` rotates the pair.
- Docker: `docker kill --signal=HUP <container>`. PID 1 is the server because
  the image has no entrypoint wrapper. Bind mounts and projected volumes update
  in place; Docker secrets are snapshots and need container recreation.
- Kubernetes: update the projected/bind-mounted files and send SIGHUP, or
  enable polling. No restart is required.
- Replace both files atomically. A half-written pair is rejected and the old
  pair keeps serving.

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
