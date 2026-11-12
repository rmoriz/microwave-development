# Implicit state machines — rotation enforcement and OAuth atomicity

**Date:** 2026-11-12
**Status:** implemented on `main` after `v0.6.2` (commits `debf3a7`…`ce80639`)
**Related:** the state-machine audit of 2026-11-12

## Intent

Close the two state-machine gaps found in the audit: the write-only
`credential_rotation_required` marker, and the non-atomic consumption of OAuth
authorization codes and refresh tokens.

## Goals

- While `<data_dir>/credential_rotation_required` exists, the server answers
  every non-health API request with `503` and a message naming
  `rotate-shared-secrets`, and the CLI refuses every command except that one.
- An authorization code can be exchanged at most once, even under concurrent
  requests.
- A refresh token can be rotated at most once, and a crash cannot consume it
  without issuing its successor.
- Both guarantees hold on SQLite and PostgreSQL.

## Non-goals

- A cross-node marker. The marker is a per-node file, as it is today; the
  rotation warning already tells the operator to copy secrets to every node.
- Cleaning up revoked `oauth_access_grants` rows (an operational follow-up).
- Changing the RFC 7009 revoke endpoint's behaviour, or adding refresh-token
  expiry.
- Stopping systemd, or refusing to start.

## Decisions and approaches

1. **Enforcement shape: health-only 503 (chosen).** The server stays up so
   health probes remain observable and containers do not crash-loop. The
   middleware gate sits next to the maintenance gate and reuses the same
   exempt-path set (`/_status`, `/_stats`). Refusing startup was rejected: it
   turns the marker into a restart loop with no health endpoint.
2. **CLI gate placement.** `dispatch` loads its `Context` after the
   configuration-free commands (`version`, `pedant`, `service`) have already
   returned. The gate goes immediately after `Context::load`: every command
   except `RotateSharedSecrets` fails with the rotation instruction. Keeping
   `service`, `pedant` and `version` runnable on a broken install is
   deliberate and documented.
3. **Marker source of truth.** The existing
   `<data_dir>/credential_rotation_required` file. The filename becomes a
   shared constant in `microwave-config` so the server, the CLI and the
   existing tests cannot drift. The server stats it per request: one `stat` is
   cheaper than the maintenance flag's database read, and it makes setting and
   clearing take effect without a restart. Loading the rotated secrets into a
   running server still requires a restart, which the rotation command already
   says.
4. **Gate order.** Rotation before maintenance: both return `503`, but the
   rotation message is the actionable one when both are set.
5. **OAuth authorization codes: single conditional consume (chosen).**
   `consume_oauth_access_grant(token) -> Option<OAuthAccessGrant>` performs
   `UPDATE … SET revoked_at = COALESCE(revoked_at, now)
   WHERE token = ?1 AND revoked_at IS NULL RETURNING <columns>`. Only one
   caller gets the row. The handler keeps its cheap pre-checks (application
   and `redirect_uri` match, which are immutable columns) so a wrong client
   cannot burn a valid code; freshness is checked on the returned row, so an
   expired code is burned and rejected.
6. **OAuth refresh tokens: transactional rotate (chosen).**
   `rotate_oauth_access_token(refresh_token, expires_in) -> Option<OAuthAccessToken>`
   runs one transaction per backend: conditionally revoke the old row
   (`WHERE revoked_at IS NULL`, returning owner, application, scopes), then
   insert the successor with a fresh access and refresh token. `None` means
   the token was unknown or already rotated. The handler validates the client
   application against a read first, so another client's token is not burned.
7. **No schema change.** `revoked_at`, `refresh_token` and the existing
   columns carry all the state; SQLite is bundled at a version that supports
   `UPDATE … RETURNING` (the repository already relies on `DELETE … RETURNING`
   for password-reset tokens).

## Design

### Credential-rotation gate

- `microwave-config` exposes `pub const CREDENTIAL_ROTATION_MARKER: &str =
  "credential_rotation_required";`.
- `microwave-ctl` uses it for the marker path (`Context::data_dir`), the
  writer, the clearer and the new pre-dispatch check.
- `microwave-api` middleware, before the maintenance check:
  `if !is_maintenance_exempt(path) && state.config.paths.data_dir joined with
  the marker exists` → `503` with
  `"Server credentials must be rotated. Run 'microwave-ctl rotate-shared-secrets'."`
  The response keeps the existing version-header behaviour.
- `rotate-shared-secrets` still clears the marker after a successful rotation;
  the server resumes normal traffic immediately, and the operator restarts to
  load the new secrets as before.

### OAuth atomic transitions

Two new `OAuthRepository` methods on both backends:

```rust
async fn consume_oauth_access_grant(&self, token: &str)
    -> Result<Option<OAuthAccessGrant>, StoreError>;

async fn rotate_oauth_access_token(&self, refresh_token: &str, expires_in: Option<i64>)
    -> Result<Option<OAuthAccessToken>, StoreError>;
```

`microwave-oauth/src/oauth.rs`:

- `authorization_code`: read the grant for the client/redirect checks, then
  consume it atomically. `None` → `invalid_grant` ("expired or already used");
  a returned row that fails `timestamp_is_fresh` → the same error, with the
  code already burned; otherwise mint the token from the returned row.
- `refresh_token`: read the token for the application check, then rotate.
  `None` → `invalid_grant` ("refresh token was revoked"); otherwise return the
  successor's `token_response`.

### Testing

- `crates/microwave-api/tests/ops_test.rs`: a temporary data directory holding
  the marker → a normal API path returns `503` with the message while
  `/_status` does not; removing the marker restores normal responses.
- `bin/microwave-ctl/tests/p1c_compat_test.rs`: with the marker set,
  `user-create` fails with the rotation instruction; `rotate-shared-secrets`
  succeeds and clears it; `service status` still runs.
- `crates/microwave-store/tests/dual_backend.rs`: a new `oauth_atomicity`
  suite in both matrices. Eight concurrent `consume_oauth_access_grant` calls
  for one code → exactly one `Some`; eight concurrent
  `rotate_oauth_access_token` calls for one refresh token → exactly one
  `Some`, and exactly one active token for the owner afterwards.
- `crates/microwave-api/tests/oauth_test.rs` (or the token-exchange section of
  `http_parity_test.rs`): replaying a code after a successful exchange, or
  reusing the old refresh token after rotation, returns `invalid_grant`;
  an expired code returns `invalid_grant` and cannot be exchanged later.

## What is left to the builder

Exact line anchors, test names and step order are in the implementation plan.
