# Clients Pedant Completion

**Date:** 2026-10-24
**Branch:** `subagent/clients2`
**Base:** `main` after `7fe95ab` (`merge: complete user pedant compatibility`); the branch started at `3fa1895` and was rebased.
**Baseline:** `docs/pedant-org-client-followup.md` (clients 129 passed / 10 failed / 3 pending)

## Result

| Run | Examples | Passed | Failed | Pending |
| --- | ---: | ---: | ---: | ---: |
| Merged baseline (`clients`, global API v1) | 142 | 129 | 10 | 3 |
| After server fixes only (config forced to global API v1) | 142 | 107 | 35 | 3 |
| **After server + harness API-version fix** | **142** | **142** | **0** | **3** |

The final run exits `0`:

```sh
PEDANT_BUNDLE_PATH=<bundle> PEDANT_ARTIFACTS=<artifacts> \
  scripts/run-pedant.sh clients
```

Artifacts: `mw-clients2-artifacts-final/` (outside the repository).

The middle row is why the harness change below is part of the fix: the five
`[v1+]` examples pass, but 31 additional `complete_endpoint_spec.rb` examples
assert v0 response shapes (top-level key material on create/update,
`public_key` on client reads) and the 4 pedant `NameError` examples reappear.
Those 31 are not server defects; they are the same file being run under API
version 1 globally. See [the harness note below](#harness-scriptspedant-bootstraprb).

## What changed

### Server behavior (`crates/microwave-api/src/clients.rs`)

`POST /organizations/:org/clients` is now version-aware:

- **v1+**
  - neither `create_key` nor `public_key` → the client is created with no
    default key; the response is only `{ "uri": ... }` and
    `GET .../keys/default` returns 404.
  - `create_key: true` → a key pair is generated and returned under
    `chef_key` (with `private_key`).
  - `public_key` → the key is stored and the response carries its location:
    `{ "uri", "chef_key": { "uri": "<client>/keys/default", "name": "default",
    "public_key", "expiration_date": "infinity" } }`. There is no top-level
    `public_key`/`private_key`.
  - `private_key` (any value), or `create_key` together with `public_key` →
    400.
- **v0** keeps the legacy contract: a default key pair is always provisioned
  when no key is supplied, and responses carry top-level `public_key` /
  `private_key`.

`PUT /organizations/:org/clients/:name`:

- **v1+** rejects `public_key`, `private_key` and `create_key`; key management
  belongs to the `/keys` endpoints. The key rotation that pedant exercises with
  `private_key: true` is v0-only.
- A normal client may still update itself, but can no longer set
  `validator: true` on itself. Admins may promote/demote; validators continue
  to be unable to update anything, including themselves.

`GET`/`DELETE /organizations/:org/clients/:name`:

- **v1+** omits `public_key`; **v0** still returns it.

Validator client creation continues to use upstream's system-privilege bypass
rather than granting the validator CREATE/READ on the `clients` container.
Adding it to the container ACL would leak the validator into the ACL template
of every subsequently created client (upstream calls this out in
`oc_chef_wm_base.erl`). A regression test now covers the functional
requirement: a validator created through the API can itself create ordinary
clients and still cannot create another validator.

### Harness (`scripts/pedant-bootstrap.rb`)

- **API version baseline.** Pedant's clients suite, apart from the explicit
  `[v1+]` examples, asserts v0 response shapes (top-level key material on
  create/update, `public_key` on client reads). Running the whole tag with
  `server_api_version 1` therefore contradicts itself. The generated config now
  uses `server_api_version 0` — the value in chef-server's own
  `chef-server-ctl/habitat/config/pedant_config.rb` — and the `[v1+]` examples
  opt in with `platform.use_max_server_api_version`. (The users work on `main`
  landed the same `PEDANT_API_VERSION` default change for the bootstrap
  requests; this branch is rebased on it.)
- **Admin user group.** Pedant's `user_from_config` drops `admin`/`associate`
  for requestors with `create_me: false`, so a pre-seeded admin user produced an
  expectation of `["pivotal"]` while the server correctly returned
  `["pivotal", "pedant_admin_user"]`. The admin user is now created by pedant
  (`create_me: true`), which preserves that metadata; the bootstrap no longer
  pre-adds it to `admins`/`billing-admins`/`users`, because pedant's
  `make_owner` performs the association itself. No server change was needed —
  the group membership was correct.

## Tests added

`crates/microwave-api/tests/clients_test.rs`:

- `client_creation_key_semantics_are_version_aware` — v1 no-key create, explicit
  `create_key: false`, v0 implicit key pair, v1 rejection of `private_key` and
  conflicting `create_key`/`public_key`.
- `client_public_keys_are_validated_and_returned` — exact v1 `chef_key`
  location response, no `public_key` on v1 reads, key still readable through
  `/keys/default`.
- `client_update_key_rotation_and_self_service` — v1 key-field rejection, v0
  rotation/`public_key` updates, self-promotion forbidden, admin promotion,
  validator self-service forbidden.
- `newly_created_validator_can_create_clients` — validator created via the API
  can create normal clients and cannot create validators.

## Remaining pedant items

- 3 pending examples are pedant's own `skip`s. The validator container-ACL
  example is skipped upstream for the template-pollution reason above; the
  other two document known upstream oddities around admin updates and
  self-promotion.
- The 4 `NameError: undefined local variable 'chef_key'` failures
  (`client_util.rb:95`) only occur when the suite is forced to global API v1.
  They are a pedant bug (`chef_key =>` instead of `"chef_key" =>`) and disappear
  at the upstream v0 baseline. They are not server behavior and were not
  patched in the vendored pedant.
