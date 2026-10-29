# oc-chef-pedant Compatibility Report

**Date:** 2026-10-24
**Server under test:** `microwave-server 0.1.0` at commit `0c2c11c` (`docs: record oauth identity surface`)
**Pedant:** vendored `chef-server/oc-chef-pedant` (gemspec version `2.2.0`)
**Harness:** `scripts/run-pedant.sh` + `scripts/pedant-bootstrap.rb`

> **Current status (2026-10-29):** the body of this report is the historical record of the pre-fix baseline and the fix rounds; the tables below are dated snapshots, not live counts. The one remaining core failure, the plain-HTTP `headers` artifact, is fixed at the harness (see [Harness notes](#harness-notes-and-known-limitations)); `scripts/run-pedant.sh headers` reports 7 examples, 0 failures. Users, organizations, clients, and nodes pass with zero failures, and the extra `environments` (610 examples / 0 failures / 23 pending) and `roles` (78 / 0) suites are green. With the patch, the five-tag core set is **652 passed / 0 failed / 8 pending out of 660 examples**; the 8 pending examples are pedant's own skips.

## Result in one sentence

Pedant runs end-to-end against a freshly built local server, but all five selected tags fail:
**254 passed, 398 failed, 8 pending out of 660 examples**. The largest gaps are the missing
`/authenticate_user` and `/system_recovery` endpoints, missing `/groups` and `/containers` APIs,
missing default key generation/return on user creation, and incomplete organization, client, and
node validation/response semantics.

## Environment

| Component | Version / value |
| --- | --- |
| OS | macOS 26.6.2 (25G83), arm64 |
| Ruby | `ruby 3.4.12 (2026-10-23 revision 366f5fdc00) +PRISM [arm64-darwin25]` |
| Bundler | `2.6.9` |
| Rust | `rustc 1.99.0 (b940084d7 2026-09-28)` |
| Cargo | `cargo 1.99.0 (5f94df478 2026-08-27)` |
| Server | `microwave-server 0.1.0`, main commit `0c2c11c` |
| Pedant API version header | `X-Ops-Server-API-Version: 1` |
| Pedant `X-Chef-Version` | `18.10.17` (hard-coded in `lib/pedant/request.rb`) |

Ruby 4.0.9 is installed on the host but was **not** used: pedant's dependency set and its
`rest-client`/`mixlib-authentication` stack are exercised with Ruby 3.4.12. `bundle install`
succeeded against the vendored lockfile; no gem or Bundler compatibility workaround was required.
The script pins Ruby 3.4.12 via asdf when available.

## Command line

The final run used one fresh server/data directory per tag (pedant deletes the requestor clients
in its after-suite hook even when they were pre-existing, so sharing an instance between tags
produces bogus 401s after the first tag):

```sh
cd /private/var/folders/.../opencode/mw-pedant

PEDANT_BUNDLE_PATH=/private/var/folders/.../opencode/pedant-bundle \
PEDANT_ARTIFACTS=/private/var/folders/.../opencode/pedant-run-final2 \
  scripts/run-pedant.sh
```

For each tag the script starts `target/debug/microwave-server serve` on a loopback ephemeral port
with a temporary SQLite data directory, waits for `/_status`, seeds actors, writes a pedant config,
then runs:

```sh
cd chef-server/oc-chef-pedant
SUPERUSER_KEY=<data-dir>/pivotal.pem \
  bundle exec ./bin/oc-chef-pedant \
    -c <artifacts>/<tag>/pedant_config.rb \
    -L <artifacts>/<tag>/http-traffic.log \
    --focus <tag>
```

Exit status of the final run was **1** (one or more tags failed), as expected for an unmodified
server. Logs, generated keys, config, and HTTP traffic captures are in the artifacts directory
printed by the script.

## Counts per tag

RSpec's own summary lines, after stripping ANSI color codes:

| Tag | Examples | Passed | Failed | Pending | Suite aborted | Pedant exit |
| --- | ---: | ---: | ---: | ---: | :---: | ---: |
| `headers` | 7 | 5 | 2 | 0 | no | 1 |
| `users` | 165 | 21 | 144 | 0 | no | 1 |
| `organizations` | 15 | 5 | 10 | 0 | no | 1 |
| `clients` | 142 | 9 | 130 | 3 | no | 1 |
| `nodes` | 331 | 214 | 112 | 5 | no | 1 |
| **Total** | **660** | **254** | **398** | **8** | **0** | **1** |

Raw summary lines:

```text
headers        7 examples, 2 failures
users          165 examples, 144 failures
organizations  15 examples, 10 failures
clients        142 examples, 130 failures, 3 pending
nodes          331 examples, 112 failures, 5 pending
```

RSpec reports errors inside examples as failures; there were no aborted suites or load errors.
The `users` failure count is inflated by one root cause: `platform.create_user` expects a key in
the `POST /users` response, and the server returns none unless the caller asks for one. Many
dependent tests then fail with `undefined method '[]' for nil`. The `nodes` count likewise
includes repeated validation-message mismatches for the same missing checks.

## First concrete failures

These are representative failures from the start of each tag's failure list. Long keys and
repeated bodies are truncated.

### 1. `headers` — low `X-Chef-Version` is accepted instead of rejected

Request: `GET /organizations/:org/users` with `X-Chef-Version: 9.0.1`; expected `400`, got `200`.

```text
Response should have HTTP status code 400 ('Bad Request'), but it was actually 200 ('OK')
# ./spec/api/header_spec.rb:28
```

### 2. `headers` — nginx default-error check cannot sign an `http://` URL

Pedant rewrites the URL with `request_url.sub!("https", "http")`. Because the harness targets a
plain-HTTP local server, `sub!` returns `nil`, and pedant raises before making the request:

```text
URI::InvalidURIError: bad URI (is not URI?): nil
# ./spec/api/header_spec.rb:45
```

This is an environment/pedant assumption, not evidence of a server defect. It should be excluded
or the server should gain TLS support before treating it as a compatibility failure.

### 3. `users` — `/authenticate_user` is missing

`POST /authenticate_user` with valid `pivotal` credentials expected `200`; got `404`:

```text
Response should have HTTP status code 200 ('OK'), but it was actually 404 ('Not Found')
  Reponse Body: {"error":["not found"]}
# ./spec/api/authenticate_user_spec.rb:118
```

All 51 `authenticate_user` examples in this tag fail, including the expected `405` responses for
`GET`/`PUT`/`DELETE`, because no route exists at all. `/system_recovery` is also absent and
accounts for another 7 failures.

### 4. `users` — `GET /users?email=` ignores the filter

Request: `GET /users?email=somenonexistingemail@somewhere.com`; expected `{}`, got all users:

```text
Expected a full match of the result
  {"pedant_admin_user" => "http://127.0.0.1:57717/users/pedant_admin_user",
   "pedant_nobody" => "http://127.0.0.1:57717/users/pedant_nobody",
   "pedant_user" => "http://127.0.0.1:57717/users/pedant_user",
   "pivotal" => "http://127.0.0.1:57717/users/pivotal"}
to the spec
  {}
# ./spec/api/user_spec.rb:50
```

The single-user email filter, verbose list, and `external_authentication_uid` filter all fail for
the same reason: `GET /users` unconditionally returns the org/user map.

### 5. `users` — `POST /users` does not generate or return a default key

Request body (as sent by pedant's `platform.create_user`):

```json
{"username":"test-2410","email":"test-2410@chef.io","first_name":"test-2410",
 "last_name":"test-2410","display_name":"test-2410","password":"foobar"}
```

Expected `201` with `private_key`; actual body:

```json
{"uri":"http://127.0.0.1:60253/users/test-2410"}
```

Real Chef Server generates a default key for a newly created user and returns it. The server only
does so when `create_key: true` is supplied, and it only returns `chef_key`, never a top-level
`private_key` for API v0. This is the dominant cascade in `users` and also affects client tests
that create users as fixtures.

### 6. `users` — verbose `GET /users` returns strings, not user objects

Expected each entry to be an object containing `display_name`; actual list entries are plain URL
strings:

```text
Failure/Error: data.key?("display_name").should be true
NoMethodError: undefined method 'key?' for an instance of String
# ./spec/api/user_spec.rb:117
```

### 7. `users` — `GET /users/:name` omits required profile fields

Expected body includes `first_name`, `last_name`, and `display_name`; actual body has only
`admin`, `email`, `public_key`, `recovery_authentication_enabled`, and `username`:

```text
Expected a full match of the result
  {"admin"=>false, "email"=>"pedant_user@example.test",
   "public_key"=>"-----BEGIN PUBLIC KEY-----...", 
   "recovery_authentication_enabled"=>false, "username"=>"pedant_user"}
to the spec
  {"first_name"=>"pedant_user", "last_name"=>"pedant_user",
   "display_name"=>"pedant_user", "email"=>"pedant_user@chef.io",
   "username"=>"pedant_user", "public_key"=>/^-----BEGIN (RSA )?PUBLIC KEY-----/}
# ./spec/api/user_spec.rb:823
```

### 8. `users` — a user outside the organization can read another user

Request: `GET /users/pedant_user` signed by `pedant_nobody` (not associated with the org); expected
`403`, got `200` with the full user body.

```text
Response should have HTTP status code 403 ('Forbidden'), but it was actually 200 ('OK')
# ./spec/api/user_spec.rb:865
```

### 9. `organizations` — org creation response lacks validator material

Request: `POST /organizations` with a valid name/full name; expected `clientname` and
`private_key`, got only `guid`, `name`, and `uri`:

```json
{"guid":"b62b1dcdf8e04b1cb6621325bf09a9ca",
 "name":"test-1792807762-3435",
 "uri":"http://127.0.0.1:60648/organizations/test-1792807762-3435"}
```

Expected partial body:

```json
{"clientname":"test-1792807762-3435-validator",
 "uri":"http://127.0.0.1:60648/organizations/test-1792807762-3435"}
```

This is why the harness creates the validator client explicitly; pedant's normal
`org(create_me: true)` path cannot work against this server as-is.

### 10. `organizations` — missing `full_name` is accepted

Request body `{"name":"test-1792807762-3435"}` (no `full_name`) expected `400`; got `201` with a
created org. An invalid name (`@!## !@#($@`) is also accepted with `201`.

```text
Response should have HTTP status code 400 ('Bad Request'), but it was actually 201 ('Created')
# ./spec/api/organization_spec.rb:120
```

### 11. `organizations` — `PUT /organizations/:id` is not implemented

Pedant sends a valid update; the server returns `405 Method Not Allowed` with
`allow: GET,HEAD,DELETE` and an empty body, so pedant fails while parsing JSON:

```text
-> "HTTP/1.1 405 Method Not Allowed"
-> "allow: GET,HEAD,DELETE"
-> "content-length: 0"

JSON::ParserError: unexpected token at ''
# ./lib/pedant/json.rb:21
```

All organization update tests fail for this reason.

### 12. `clients` — `/groups/:name` is not implemented

`GET /organizations/:org/groups/admins` expected `200` with group members; got `404`:

```text
Response should have HTTP status code 200 ('OK'), but it was actually 404 ('Not Found')
  Reponse Body: {"error":["not found"]}
# ./spec/api/clients/account_client_spec.rb:80
```

`/groups/clients` and group-retrieval tests fail identically. There is no `groups` module in the
API router, so pedant cannot perform its normal group setup either. The harness works around this
by inserting `authz_group_actors` rows directly into SQLite.

### 13. `clients` — `/containers/:name` is not implemented

`GET /organizations/:org/containers/clients` expected `200`; got `404`:

```text
Response should have HTTP status code 200 ('OK'), but it was actually 404 ('Not Found')
  Reponse Body: {"error":["not found"]}
# ./spec/api/clients/account_client_spec.rb:152
```

### 14. `clients` — client-creation authorization is inverted

Pedant expects a normal client to be forbidden from creating clients, and a validator to be
forbidden from creating another validator. Both requests actually return `201`:

```text
# non-validator client
Response should have HTTP status code 403 ('Forbidden'), but it was actually 201 ('Created')
# ./spec/api/clients/account_client_spec.rb:198

# validator creating a validator
Response should have HTTP status code 403 ('Forbidden'), but it was actually 201 ('Created')
# ./spec/api/clients/account_client_spec.rb:217
```

The clients container grants the `clients` group only `read`/`delete`, yet the handlers do not
enforce the container ACL for create, so any authenticated client in the org can create one.

### 15. `clients` — public keys are omitted from creation responses and not validated

With a supplied `public_key`, pedant expects `public_key` in the response; the server returns only
`uri` (or key material only when it generated the key itself):

```text
Failure/Error: parsed_response["public_key"].should_not be_nil
  expected: not nil
       got: nil
# ./lib/pedant/rspec/client_util.rb:294
```

Malformed, private-key, blank, `1`, `[]`, and `{}` values for `public_key` are all accepted with
`201`, where pedant expects `400` and no persisted client.

### 16. `nodes` — error messages do not match pedant's contract

`GET /nodes/nonexistent_pedant_node` expected:

```json
{"error":["node 'nonexistent_pedant_node' not found"]}
```

Actual:

```json
{"error":["node nonexistent_pedant_node"]}
```

This single formatting difference fails the not-found tests and several authorization/validation
tests that compare exact bodies.

### 17. `nodes` — environment-scoped node listing is missing

`GET /environments/_default/nodes` expected `200 {}`; got `404`:

```text
Response should have HTTP status code 200 ('OK'), but it was actually 404 ('Not Found')
  Reponse Body: {"error":["not found"]}
# ./spec/api/nodes/complete_endpoint_spec.rb:114
```

### 18. `nodes` — node validation accepts invalid names/environments

Invalid node name `node@127.0.0.1` is accepted:

```text
Response should have HTTP status code 400 ('Bad Request'), but it was actually 201 ('Created')
  Reponse Body: {"uri":"http://127.0.0.1:61113/organizations/pedant_testorg/nodes/node@127.0.0.1"}
```

Name `pedant:no_colon_in_environment_name` in `chef_environment`, `pedant@127.0.0.1`, unknown
`json_class`/`chef_type`, and invalid `policy_name`/`policy_group` values are all accepted with
`201`. The node handlers persist payloads without applying the upstream validators.

### 19. `nodes` — `DELETE /nodes/:name` returns `{}` instead of the deleted node

Pedant expects `200` with the full deleted node body (name, `json_class`, `chef_type`,
environment, attributes, run list). Actual body is `{}`:

```text
Expected a partial match of the result
  {}
to the spec
  {"name"=>"pedant_testing_node", "json_class"=>"Chef::Node", "chef_type"=>"node", ...}
# ./spec/api/auth_spec.rb:222
```

### 20. `nodes` — invalid top-level keys are silently accepted

`POST /nodes` with `something_random`, a UTF-8 key, or a random hex key returns `201` and creates
the node; pedant expects `400 Invalid key <name> in request body`:

```text
Response should have HTTP status code 400 ('Bad Request'), but it was actually 201 ('Created')
  Reponse Body: {"uri":"http://127.0.0.1:61113/organizations/pedant_testorg/nodes/pedant_testing_node_..."}
# ./lib/pedant/rspec/common.rb:78
```

The same is true for `PUT /nodes/:name` (failures 93–97 in the nodes log).

## Highest-impact missing behaviors

Ordered by how much pedant surface they unlock:

1. **`/authenticate_user` and `/system_recovery` do not exist.** Every signin/password example in
   the `users` tag fails with `404`, even where pedant expects a method-not-allowed `405`.
2. **User creation does not generate and return a default key.** `POST /users` without
   `create_key` returns only `uri`; pedant's shared setup then dereferences a nil `chef_key` and
   aborts many dependent examples. Return the v0 `private_key` and v1/v2 `chef_key` shapes by
   default (while still rejecting conflicting `create_key`/`public_key` combinations).
3. **Organization creation does not provision the validator client.** `POST /organizations` must
   return `clientname`/`private_key` (and a v1 `chef_key`) and create the `-validator` client with
   the correct ACLs. `PUT /organizations/:id` is also absent.
4. **`/groups` and `/containers` APIs are absent.** These are required for pedant's own setup
   (`admins`, `billing-admins`, `users`, `clients` groups; `clients` container) and for the ACL
   tests. The harness currently seeds group membership by writing SQLite rows, which is not a
   substitute for the API.
5. **Client authorization and key semantics are incomplete.** Validators and ordinary clients can
   create clients; requestor ACLs are not enforced; `public_key` is neither returned nor validated;
   `create_key`/`private_key` flags are inconsistently honored between v0 and v1.
6. **Node validation and response bodies do not match the upstream contract.** Error text,
   not-found text, missing environment-scoped listing, missing name/environment/policy/json_class/
   chef_type validation, unknown top-level key rejection, and DELETE returning `{}` instead of the
   resource account for most of the 112 node failures.
7. **Organization/user response shapes are missing fields.** `GET /users` needs filtering and a
   verbose object list; `GET /users/:name` needs `first_name`, `last_name`, and `display_name`;
   org validation must reject missing `full_name` and invalid names.
8. **`X-Chef-Version` has no lower-bound enforcement.** A request with `9.0.1` is accepted; the
   header tests expect `400` for versions below the configured minimum.
9. **User read authorization is too permissive.** An authenticated user who is not a member of the
   organization can read other users' profiles.

## Harness notes and known limitations

- `scripts/pedant-bootstrap.rb` creates the org, validator, two clients, and three users through
  the public signed API. It writes a generated `pedant_config.rb` with `create_me: false` for all
  requestors.
- Validator material is created explicitly because the server's org-creation response does not
  return it. That gap is itself reported above.
- Users and clients are pre-created because pedant's normal setup uses the org validator to create
  clients and relies on user creation returning a key; both paths are currently broken.
- Group membership is seeded with `sqlite3` because no `/groups` API exists. This is the only
  direct database write in the harness. Remove it once the groups API lands.
- Each tag runs against a separate server/data directory. Pedant deletes requestor clients during
  cleanup even when they were pre-existing, so a shared server makes every tag after the first
  fail with spurious 401s.
- The `headers` nginx-default-error example used `request_url.sub!("https", "http")`, and `sub!`
  returns `nil` when the URL is already plain HTTP, so the example signed and sent a nil URL. That is
  a harness bug, not a server-behavior signal. `header_spec.rb` now passes `request_url` unchanged,
  and the patch is listed under "Vendored harness-only patches" at the top of
  [`scripts/run-pedant.sh`](../scripts/run-pedant.sh) so it is re-applied on every re-vendor. The
  `headers` low-version failure is a real missing check.
- Pending examples are pedant's, not harness failures. The `clients` tag has 3 pending and the
  `nodes` tag has 5.
- The harness restores the vendored `Gemfile.lock` after each run. Bundler adds the local
  `arm64-darwin-25` platform and drops the `rack` floor because the omnibus `safe_versions.rb`
  file is not present in this checkout; committing that host-specific rewrite is undesirable.

---

## Post-fix rerun (merged main)

Run after the node, user/auth, and organization/client compatibility fixes were merged.

Command:

```sh
scripts/run-pedant.sh headers users organizations clients nodes
```

| Tag | Examples | Passed | Failed | Pending |
| --- | ---: | ---: | ---: | ---: |
| headers | 7 | 6 | 1 | 0 |
| users | 165 | 81 | 84 | 0 |
| organizations | 15 | 15 | 0 | 0 |
| clients | 142 | 129 | 10 | 3 |
| nodes | 331 | 326 | 0 | 5 |
| **Total** | **660** | **557** | **95** | **8** |

The `headers` failure in this table is the plain-HTTP pedant artifact (`"https"` → `"http"` `sub!` returns nil), fixed at the harness on 2026-10-29. Four `clients` failures are pedant's own `NameError: undefined local variable 'chef_key'`; several of the remaining client failures are v1 canonical-behavior trade-offs documented in `docs/pedant-org-client-followup.md`.

### Clients tag completion

`scripts/run-pedant.sh clients` now reports **142 examples, 0 failures, 3
pending** (exit 0). The five real `[v1+]` failures were fixed in the server;
the admin-group failure was a pedant config-metadata issue and the four
`NameError` examples only appear when the whole tag is forced to API v1. The
generated pedant config now uses `server_api_version 0` (matching
`chef-server/src/chef-server-ctl/habitat/config/pedant_config.rb`), with the
`[v1+]` examples opting into newer versions as designed. Details:
`docs/pedant-clients-followup.md`.

### Next highest-impact work

1. `users` tag: user create/update validation, verbose/list filters, `first_name`/`last_name`/`display_name`, v1 `public_key` omission, rename and outside-user authorization.
2. Broaden the harness beyond these five tags: keys, ACLs, data bags, cookbooks, sandboxes, search, policies, oauth, license, depsolver.

---

## Final merged run

Run on merged `main` after the users and clients fixes:

```sh
scripts/run-pedant.sh headers users organizations clients nodes
```

| Tag | Examples | Passed | Failed | Pending |
| --- | ---: | ---: | ---: | ---: |
| headers | 7 | 6 | 1 | 0 |
| users | 165 | 165 | 0 | 0 |
| organizations | 15 | 15 | 0 | 0 |
| clients | 142 | 139 | 0 | 3 |
| nodes | 331 | 326 | 0 | 5 |
| **Total** | **660** | **651** | **1** | **8** |

The single failure in this table is the plain-HTTP pedant artifact in `header_spec.rb` (`"https"` → `"http"` `sub!` returns nil), fixed at the harness on 2026-10-29. The 8 pending examples are pedant's own skips.

### Harness patch rerun (2026-10-29)

The `header_spec.rb` one-liner and the patch list in `scripts/run-pedant.sh` landed, and the tag was
re-run:

```sh
scripts/run-pedant.sh headers
```

Result: **7 examples, 0 failures**. With that, the five-tag core set is 652 passed, 0 failed, and 8
pending (pedant's own skips); the report no longer has a server-side core failure.

Artifacts: `/private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/pedant-final`.

### Remaining coverage

These five tags are green. The harness now also supports the extended suites:
`scripts/run-pedant.sh extended` covers keys, ACLs, data bags, cookbooks,
sandboxes, search, policies, OAuth, license, depsolver, and the versioned
behaviors. Baseline counts, root causes, and the prioritized gap list are in
[`docs/pedant-extended-report.md`](pedant-extended-report.md).
