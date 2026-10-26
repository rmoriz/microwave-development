# SMTP Mail-Out Design

**Date:** 2026-10-26
**Status:** approved
**Scope:** first slice: remote SMTP transport with authentication/TLS and
password-reset email. Email-change verification and invitation emails are
explicit follow-ups.

## Intent

Microwave currently has no outbound email. The `/id/password-reset` endpoint
creates a single-use token but never delivers it, so a user cannot complete the
flow unless `auth.password_reset_token_in_response` is enabled for development.
The container/remote deployment story also assumes no local MTA.

This change adds a real mail-out path that works with a remote SMTP relay,
including SMTP authentication, STARTTLS or implicit TLS, and an optional
private CA. Local SMTP remains supported simply as `host = "localhost"`.

## Goals

- Send the password-reset link through a configured remote SMTP relay.
- Support SMTP AUTH with PLAIN and LOGIN credentials.
- Support STARTTLS (port 587), implicit TLS/SMTPS (port 465), and explicit
  plaintext for trusted local relays.
- Keep credentials out of logs and error messages.
- Preserve the current generic response by default so account existence is not
  disclosed. The explicit `auth.password_reset_token_in_response` development
  flag keeps working independently of mail delivery.
- Keep mail disabled by default; existing installs and tests are unchanged.

## Non-goals

- Email-change verification for `/id/profile`.
- Organization invitation emails.
- Inbound email, bounce processing, DKIM signing, or a local sendmail binary.
- HTML templates, localization, attachments, or a durable outbox with retries.
- General-purpose notification mail.
- Built-in abuse throttling. The endpoint should be protected at the reverse
  proxy; a cooldown/rate limit is a follow-up.

## Approaches considered

1. **`lettre` async SMTP transport (chosen).**
   Mature Rust mail library with Tokio, rustls, SMTP AUTH (PLAIN/LOGIN/XOAUTH2),
   STARTTLS, implicit TLS, custom root certificates, connection pooling and
   timeouts. Fits the existing async stack.
2. **Hand-written SMTP client.**
   Avoids one dependency but reimplements TLS upgrade, authentication
   mechanisms, dot-stuffing, response parsing and error handling. Not worth the
   security and maintenance risk.
3. **Invoke `sendmail`/`mail` as a child process.**
   Cannot satisfy the requested remote/authenticated relay requirement without
   a local MTA, adds an external binary dependency and shell/argument-injection
   surface. Rejected.

## Configuration

Add a `[mail]` section to `Config`, with `MW_MAIL__*` overrides:

```toml
[mail]
enabled = true
host = "smtp.example.com"
port = 587
security = "starttls"        # starttls | tls | none
username = "chef-mailer"
password = "secret"          # preferably from MW_MAIL__PASSWORD
auth = "auto"                # auto | plain | login | none
from = "Chef Infra Server <chef@example.com>"
reply_to = "ops@example.com" # optional
timeout_ms = 10000
hello_name = "chef.example.com" # optional EHLO name
tls_ca_path = "/etc/ssl/private-ca.pem" # optional private CA
```

Add `server.public_url` (for example `https://chef.example.com`) and require it
when mail is enabled. Password-reset links must not be built from an
unauthenticated `Host` header.

`server.public_url` must be an absolute `http`/`https` origin with no path,
query or fragment; trailing slashes are stripped. `https` is required except
for loopback development addresses (`localhost`, `127.0.0.1`, `[::1]`), where
`http` is accepted with a startup warning.

The SMTP password comes from `mail.password` or `MW_MAIL__PASSWORD` in this
slice. Secret-store integration (`microwave-ctl set-secret mail password ...`)
is a follow-up.

Validation rules:

- `enabled = false` by default; all other fields are ignored while disabled.
- When enabled, `host`, `from`, `server.public_url`, and a non-zero timeout are
  required.
- `username` requires `password`; `auth != "none"` requires credentials;
  credentials with `auth = "none"` are rejected.
- `security = "none"` is allowed only when explicitly configured and logs a
  startup warning. It is never selected implicitly.
- SMTP AUTH is rejected with `security = "none"` for non-loopback hosts:
  credentials are never sent over plaintext to a remote relay. Loopback hosts
  (`localhost`, `127.0.0.1`, `::1`) may use AUTH with plaintext for local
  relays and integration tests. Remote authenticated relays must use STARTTLS
  or TLS.
- `tls_ca_path`, when set, must be a readable PEM file.
- Unknown/invalid enum values fail configuration loading.
- Credentials are redacted from `Debug`, errors, logs, and `check-config`
  output.

Default port by security mode when `port` is omitted: `587` for STARTTLS,
`465` for implicit TLS, `25` for plaintext.

## Architecture

Add a small `microwave-mail` crate:

- `MailMessage { from, to, reply_to, subject, text_body }`
- `trait Mailer: Send + Sync { fn is_enabled(&self) -> bool; async fn send(&self, message: &MailMessage) -> Result<(), MailError>; }`
- `SmtpMailer` backed by `lettre::AsyncSmtpTransport<Tokio1Executor>`
- `DisabledMailer` for the default configuration
- `build_mailer(&MailConfig) -> Result<Arc<dyn Mailer>, MailError>`
- `MailDispatcher`: a bounded in-process channel with one worker, following the
  existing `ActionDispatcher` pattern. It implements `Mailer`; `send` only
  enqueues. The password-reset request enqueues the
  message and returns without waiting for the relay; the worker sends it and
  logs failures. Capacity is small (for example 64); overflow is logged and
  dropped. There are no retries and no durable outbox in this slice.

`AppState` owns `Arc<dyn Mailer>` (the dispatcher in production) and passes it
into `OAuthState`. Mail construction happens once at startup; an invalid
enabled configuration fails startup instead of silently degrading to no mail.
This keeps request latency flat and avoids an unauthenticated caller holding a
connection open for the SMTP timeout.

## Password-reset flow

On `POST /id/password-reset`:

1. Look up the user as today. Unknown users keep the same generic response.
2. If mail is enabled and the user has an email address:
   - create the single-use token;
   - build `{server.public_url}/id/password-reset?token=...`;
   - send a plain-text email from `mail.from`.
3. Always return the existing generic success message.
4. `auth.password_reset_token_in_response` remains effective regardless of
   mail delivery. When it is `true`, the response still includes `reset_url`
   even if the email was sent; when it is `false` (the default), the response
   stays generic and no token is exposed. This is intentionally a development
   escape hatch and weakens the email-verification boundary: anyone who can
   call the endpoint receives a usable reset link for an existing account.
5. If the user has no email address, log and return the generic response.
6. Enqueue the message; enqueue failures (worker stopped/queue full) are logged
   and still return the generic response. SMTP failures happen in the worker
   and are logged there.
7. Add a reset-token TTL (`auth.password_reset_ttl_seconds`, default `3600`)
   and enforce it on `find` and `consume`. Expired tokens are treated as
   invalid. The email text derives its expiry from this value.

Email content:

```text
Subject: Reset your Chef Infra Server password

A password reset was requested for your account.

Open this link to choose a new password:
{reset_url}

This link expires in {ttl_minutes} minutes. If you did not request a reset,
ignore this message.
```

## Error handling and observability

- `MailError` distinguishes configuration, TLS, authentication, and transport
  failures internally; the HTTP response never exposes them.
- Log only the relay host, port, security mode, sender and recipient domain.
  Never log the password or the reset token.
- Optionally increment in-process counters later; not required for this slice.
- `microwave-ctl check-config` validates the mail section, including the CA
  path, without sending mail.

## Tests

- Config parsing/defaults, env overrides, and each validation failure.
- Transport builder selects STARTTLS, implicit TLS and plaintext correctly;
  private CA loading is exercised. Unit tests assert the chosen `lettre` TLS
  mode and credentials; a real TLS handshake is covered only by an optional
  env-gated integration test, not the default unit suite.
- A local plaintext fake SMTP server captures a password-reset message and
  verifies:
  - `MAIL FROM`, `RCPT TO`, subject and reset URL;
  - SMTP AUTH PLAIN and LOGIN succeed with configured credentials;
  - wrong credentials produce a logged failure and no panic.
- Dispatcher tests cover enqueue success, queue overflow/drop, and worker
  failure logging without panicking the request path.
- Password-reset route tests with a recording `Mailer`:
  - existing user with email sends exactly one message;
  - unknown user and user without email send none but return the same response;
  - mail-enabled responses include `reset_url` when
    `password_reset_token_in_response = true`, and omit it when `false`;
  - mail-disabled behavior and existing `password_reset_token_in_response`
    tests remain unchanged;
  - expired tokens are rejected.
- `cargo test --workspace`, `cargo fmt --all --check`, clippy with
  `-D warnings`, and the OAuth/`users` Pedant tags remain green.

## Documentation

- Document the `[mail]` section and `server.public_url` in `README.md`,
  `docs/deployment.md`, and `packaging/microwave.toml.example`.
- Document that remote SMTP with authentication is the supported production
  path; localhost SMTP and plaintext are development/trusted-relay options.
- Note the explicit non-goals (email verification, invitations) so the next
  slice can build on the same `Mailer` trait.

## Rollout and compatibility

- Default `mail.enabled = false`: no new startup failure, no behavior change.
- Existing development flow with `password_reset_token_in_response` keeps
  working while mail is disabled.
- Enabling mail requires `server.public_url` and validates the relay
  configuration at startup.

## Builder decisions

These were settled during the design review and are not open questions:

- Use an asynchronous bounded dispatcher, not an inline SMTP await.
- Never send SMTP credentials over plaintext to a remote host; the loopback
  exception exists only for local relays/tests.
- Accept `http://` public URLs only for loopback; require `https` otherwise.
- Derive the email's expiry text from `auth.password_reset_ttl_seconds`.
- Config/env password only in this slice; secret-store lookup is a follow-up.
- No built-in rate limiter in this slice; protect the endpoint at the proxy.

## Follow-ups

- Email-change verification for `/id/profile` using a signed token and the same
  mailer.
- Organization invitation emails.
- Secret-store lookup for `mail.password`.
- Per-IP/per-account cooldown for password-reset requests.
- Optional delivery metrics and a bounded retry/outbox if operators need
  stronger delivery guarantees.
