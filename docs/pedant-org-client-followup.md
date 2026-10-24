# Organization / Client Pedant Follow-up

**Date:** 2026-10-24
**Branch:** `subagent/orgfix`
**Baseline:** `docs/pedant-report.md` (commit `0c2c11c`)

## Result

| Tag | Before (passed / failed / pending) | After (passed / failed / pending) |
| --- | ---: | ---: |
| `organizations` | 5 / 10 / 0 (of 15) | **15 / 0 / 0** (of 15) |
| `clients` | 9 / 130 / 3 (of 142) | **129 / 10 / 3** (of 142) |

Run with:

```sh
PEDANT_BUNDLE_PATH=<bundle> PEDANT_ARTIFACTS=<artifacts> \
  scripts/run-pedant.sh organizations clients
```

Artifacts from the final run: `pedant-orgfix-final/` (organizations) and
`pedant-orgfix-final2/` (clients), outside the repository.

## What changed

- Organization creation validates `name`/`full_name`, provisions
  `<org>-validator` with a generated default key, adds it to the `clients`
  group and returns `clientname`, `private_key`, and (API v1+) `chef_key`.
  The returned key authenticates immediately.
- `PUT /organizations/:org` updates `full_name`, rejects renames and
  `private_key`, and echoes the posted representation.
- Added the groups and containers APIs, including normalized group payloads,
  membership replacement, renames, default-object deletion protection, and
  ACL-based authorization.
- Clients now enforce validator/normal-client creation and management rules,
  validate PEM public keys on create/update, return upstream-shaped client
  documents, and support default-key creation/rotation with versioned response
  shapes.
- The pedant bootstrap now uses the returned validator material, manages
  groups through the public API instead of SQLite, and writes an integer
  `server_api_version`.

## Remaining client failures (10)

- 4 × pedant's own `NameError` (`chef_key` local variable) in
  `complete_endpoint_spec.rb` valid-name examples.
- 1 × admin-group expectation caused by pre-created pedant users losing their
  `admin`/`associate` metadata in the harness.
- 1 × explicit `public_key` create response shape: this server returns the
  top-level `public_key` requested by the compatibility work, while the v1 spec
  expects only `chef_key`.
- 4 × server-api-v1 tests that conflict with the compatibility surface requested
  here: key generation when `create_key` is omitted, `public_key` on client
  GET, and key rotation via `PUT /clients/:name`.
- The `organizations` tag passes completely.
