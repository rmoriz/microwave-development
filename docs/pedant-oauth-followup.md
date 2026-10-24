# OAuth (oc_id) Pedant Completion

**Date:** 2026-10-24
**Branch:** `subagent/oauthfix`
**Base:** `b707c80` (`merge: extended pedant coverage`)
**Baseline:** `docs/pedant-extended-report.md` §14 — oauth 13 examples,
1 passed, 7 failed, 5 pending.

## Result

| Run | Examples | Passed | Failed | Pending |
| --- | ---: | ---: | ---: | ---: |
| Baseline (`oauth` = `oc_id`) | 13 | 1 | 7 | 5 |
| **After fixes** | **13** | **8** | **0** | **5** |

The final run exits `0`:

```sh
PEDANT_BUNDLE_PATH=<bundle> PEDANT_ARTIFACTS=<artifacts> \
  scripts/run-pedant.sh oauth
```

Artifacts: `pedant-oauthfix-verify3/` (outside the repository).

The five pending examples are Pedant's own skips (`/v1/users`, `/v1/me`,
`/v1/me/organizations`, `/zendesk`, `/password-reset`), which need a configured
oc-id application, Zendesk, or mail delivery.

## What changed

### Sessions and pages (`crates/microwave-oauth/src/{sessions,util}.rs`)

- Every rendered page now carries
  `<meta name="csrf-token" content="..." />`. The self-closing ` />` matters:
  Pedant scrapes with `%r{<meta name="csrf-token" content="(.*)" />}`.
- `GET /id/signin` embeds the same token as a hidden `authenticity_token`,
  posts to `/id/auth/chef/callback` (the route Pedant uses) instead of
  `/id/sessions`, and submits `commit=Sign+In`.
- `GET /id/signin` sets the `_oc_id_session` pre-session cookie only when the
  request has no session cookie, and redirects an already signed-in user to
  `/id/oauth/authorized_applications`, matching `SessionsController#new`.
- `/id/sessions` remains routed as an alias for the callback, and the
  sign-out flow keeps its `Max-Age=0` cookie deletion and `/id/signin`
  redirect.

### Profile (`crates/microwave-oauth/src/profile.rs`)

- `GET /id/profile` renders `user[first_name]`, `user[middle_name]`,
  `user[last_name]` and `user[email]` as
  `value="..." ... id="user_first_name"` (value before id), which is the
  order Pedant's regexes require, and keeps the `Signed in as: <name>` banner.
- `PUT /id/profile` (also `POST` with `_method=PUT`) updates the three name
  fields and deliberately leaves the stored email alone: upstream oc-id sends
  a verification mail for address changes, and Pedant asserts the address is
  unchanged.
- After a profile update the default public key gains one trailing newline,
  mirroring upstream oc-id's `PUT /users` round-trip. Pedant asserts
  `oc_id_user.signing_key.public_key.to_s + "\n"` byte-for-byte on the
  following `GET /users/:name`; the normalization is idempotent.
- Password changes and key regeneration keep their existing behaviour and are
  now covered by the Pedant-style regression test.

### Tests (`crates/microwave-api/tests/oauth_test.rs`)

The new integration tests replay Pedant's exact requests: scrape the cookie
and CSRF token from `GET /id/signin`, post the credentials to
`/id/auth/chef/callback`, then assert the profile form shape, the name-only
update, the key whitespace, the password change, key regeneration, and
sign-out.

## Remaining Rails-visual gaps

Pedant does not assert on these, but they still differ from upstream oc-id:

- CSRF tokens are per-render UUIDs. They are not persisted with the session and
  never rejected, so they satisfy scraping and round-tripping but not Rails'
  `protect_from_forgery` semantics.
- `/id/signin`, `/id/profile`, `/id/oauth/applications*`,
  `/id/oauth/authorize`, and `/id/password-reset` are hand-written HTML rather
  than Devise/Doorkeeper views: no layout, flash messages, field-error
  rendering, `utf8=✓`, or `form_with` markup. Only the sign-in and profile
  forms embed `authenticity_token` inputs.
- Error responses from the profile, application, and OAuth endpoints are JSON
  or bare HTML; upstream re-renders the Rails form with 422 and inline errors.
- `/id/oauth/authorized_applications` does not list or revoke authorized
  applications, and the application pages have no delete/`_method` link.
- The email address cannot be changed through the profile form (no
  verification-mail flow), and empty name fields are treated as "unchanged".
- `/id/profile/regen_key` streams the PEM directly; upstream uses the same
  `application/pem-keys` attachment but goes through Rails' `send_data`.
