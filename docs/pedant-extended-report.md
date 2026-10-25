# Extended oc-chef-pedant Compatibility Report

**Date:** 2026-10-24
**Server under test:** `microwave-server 0.1.0`, merged `main` at `ad0a865`
(`docs: correct remaining backend and solver status`)
**Pedant:** vendored `chef-server/oc-chef-pedant` (gemspec version `2.2.0`)
**Harness:** `scripts/run-pedant.sh` + `scripts/pedant-bootstrap.rb` +
`scripts/pedant-solr-stub.rb` (new)
**Branch:** `subagent/pedant-more`

> **Final run (2026-10-25, full `all` run after all feature merges):** **3,284 passed / 0 failed / 7 pending out of 3,291 extended examples. All twelve extended tags pass.** Core plus extended (`all`) reports only the known plain-HTTP `headers` artifact; every other tag is green. The counts below are the historical pre-fix baseline.
>
> | Tag | Examples | Passed | Failed | Pending |
> | --- | ---: | ---: | ---: | ---: |
> | `keys` | 312 | 312 | 0 | 0 |
> | `acls` | 1,640 | 1,640 | 0 | 0 |
> | `data_bags` | 89 | 89 | 0 | 0 |
> | `cookbooks` | 531 | 531 | 0 | 0 |
> | `sandboxes` | 14 | 14 | 0 | 0 |
> | `search` | 501 | 500 | 0 | 1 |
> | `policies` | 93 | 93 | 0 | 0 |
> | `oauth` | 13 | 8 | 0 | 5 |
> | `license` | 9 | 9 | 0 | 0 |
> | `depsolver` | 20 | 20 | 0 | 0 |
> | `versioned_behaviors` | 65 | 64 | 0 | 1 |
> | `compliance_proxy` | 4 | 4 | 0 | 0 |
> | **Total** | **3,291** | **3,284** | **0** | **7** |
>
> The suites were first captured on `41abe62`. `main` then gained the ctl,
> object-store-secret, and full-depsolver changes; the branch was rebased onto
> `ad0a865` and `depsolver` and `cookbooks` were re-run. The intervening
> commits do not touch the ACL, data bag, sandbox, policy, OAuth, search,
> license, key, or principal surfaces, so those counts are still valid for
> `ad0a865`.

## Result in one sentence

The harness now runs the eleven additional suites requested, and they expose
**1,963 passed / 1,310 failed / 14 pending out of 3,287 examples**: the ACL and
cookbook suites account for 1,059 of the failures, while data bags, sandboxes,
policies, depsolver, license, OAuth, principals, keys, and search account for
the rest. Only two route families are entirely missing
(`/environments/:env/cookbooks*` and `/cookbook_artifacts/*`); most of the
remaining work is response-shape, validation, and authorization compatibility.

## Environment

| Component | Version / value |
| --- | --- |
| OS | macOS 26.6.2 (25G83), arm64 |
| Ruby | `ruby 3.4.12 (2026-10-23 revision 366f5fdc00) +PRISM [arm64-darwin25]` |
| Bundler | `2.6.9` |
| Rust | `rustc 1.99.0 (b940084d7 2026-09-28)` |
| Cargo | `cargo 1.99.0 (5f94df478 2026-08-27)` |
| Server | `microwave-server 0.1.0`, `main` `ad0a865` |
| Pedant API version header | `X-Ops-Server-API-Version: 0` (`PEDANT_API_VERSION=0`) |
| Pedant `X-Chef-Version` | `18.10.17` (hard-coded in `lib/pedant/request.rb`) |

The server binary was built once with
`cargo build --locked --bin microwave-server --bin microwave-ctl` in the
worktree and reused with `MW_SKIP_BUILD=1`. Every tag ran against its own
fresh server, data directory, and seeded actors, as in the five-tag workflow.

## Commands

```sh
cd /private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/mw-pedant-more

export MW_SKIP_BUILD=1
export PEDANT_BUNDLE_PATH=/private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/pedant-bundle

PEDANT_ARTIFACTS=/private/var/folders/.../pedant-ext-keys \
  scripts/run-pedant.sh keys

PEDANT_ARTIFACTS=/private/var/folders/.../pedant-ext-1 \
  scripts/run-pedant.sh data_bags license depsolver sandboxes

PEDANT_ARTIFACTS=/private/var/folders/.../pedant-ext-2 \
  scripts/run-pedant.sh acl policies oauth versioned_behaviors

PEDANT_ARTIFACTS=/private/var/folders/.../pedant-ext-4 \
  scripts/run-pedant.sh cookbooks

PEDANT_ARTIFACTS=/private/var/folders/.../pedant-ext-search3 \
  scripts/run-pedant.sh search

# After rebasing onto ad0a865:
PEDANT_ARTIFACTS=/private/var/folders/.../pedant-main-depsolver \
  scripts/run-pedant.sh depsolver
PEDANT_ARTIFACTS=/private/var/folders/.../pedant-main-cookbooks \
  scripts/run-pedant.sh cookbooks
```

The committed harness also accepts the named sets:

```sh
scripts/run-pedant.sh core        # headers users organizations clients nodes (default)
scripts/run-pedant.sh extended    # the eleven suites in this report
scripts/run-pedant.sh all         # core + extended
PEDANT_TAGS=extended scripts/run-pedant.sh
```

Friendly aliases map to Pedant's RSpec metadata:
`acls` → `acl`, `oauth` → `oc_id`, `versioned_behaviors` → `api_v1`.
The five-tag default is unchanged.

Artifacts (logs, HTTP traffic, generated keys/config) are in the per-tag
`PEDANT_ARTIFACTS` directories printed by the script. The final search run is
`/private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/pedant-ext-search3`.

### Harness changes required to make these suites meaningful

1. **Readiness timeout.** A fresh SQLite database can take more than ten
   seconds to migrate on a cold page cache. The readiness loop now allows 60
   seconds. Without this, `data_bags` exited before the server was up.
2. **Tag sets and aliases.** `core`, `extended`, and `all` expansions plus the
   aliases above.
3. **No-op Solr commit stub (`scripts/pedant-solr-stub.rb`).** Pedant's
   `word_break_spec.rb` calls `force_solr_commit` unconditionally, and
   `search_util` uses the configured `search_server` for that POST. With no
   config value the URL became `http:///update?commit=true`, which accounted
   for **376 of the 388 search failures** in the first run. The harness now
   starts a tiny 200-OK HTTP stub, writes its URL as `search_server`, and sets
   `direct_solr_query_sleep_time 0`. The real search API is still what is
   tested; the stub only absorbs the external Solr commit call.
4. **No new seed data was required** (see [Seeding](#seeding)).

## Per-tag counts

RSpec summary lines, after stripping ANSI colour codes. "Passed" is
`examples - failures - pending`.

| Tag | Examples | Passed | Failed | Pending | Pedant exit | Runtime (approx.) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `keys` | 312 | 173 | 139 | 0 | 1 | 10m 32s |
| `acls` (`acl`) | 1,640 | 1,085 | 555 | 0 | 1 | 16m 34s |
| `data_bags` | 89 | 53 | 36 | 0 | 1 | ~40s |
| `cookbooks` | 531 | 27 | 504 | 0 | 1 | 52s |
| `sandboxes` | 14 | 8 | 6 | 0 | 1 | 6s |
| `search` | 501 | 487 | 6 | 8 | 1 | 4m 50s |
| `policies` | 93 | 59 | 34 | 0 | 1 | ~1m |
| `oauth` (`oc_id`) | 13 | 1 | 7 | 5 | 1 | 1m 4s |
| `license` | 9 | 3 | 6 | 0 | 1 | ~10s |
| `depsolver` | 20 | 15 | 5 | 0 | 1 | 3s |
| `versioned_behaviors` (`api_v1`) | 65 | 52 | 12 | 1 | 1 | ~2m |
| **Total** | **3,287** | **1,963** | **1,310** | **14** | **1** | **~40m** |

Raw summary lines:

```text
keys                 312 examples, 139 failures
acl                  1640 examples, 555 failures
data_bags            89 examples, 36 failures
cookbooks            531 examples, 504 failures
sandboxes            14 examples, 6 failures
search               501 examples, 6 failures, 8 pending
policies             93 examples, 34 failures
oauth                13 examples, 7 failures, 5 pending
license              9 examples, 6 failures
depsolver            20 examples, 5 failures
versioned_behaviors  65 examples, 12 failures, 1 pending
```

Notes:

- The `cookbooks` tag also pulls `environments/cookbooks_spec.rb` and
  `environments/single_cookbook_spec.rb` because those files carry the
  `:cookbooks` tag.
- The `versioned_behaviors` alias focuses `:api_v1`, which also matches
  `principal_spec.rb`. All 12 examples in
  `versioned_behaviors/server_api_v1_spec.rb` pass; all 12 failures in this tag
  are principal-spec failures.
- The 14 pending examples are Pedant's own skips: 5 in `oc_id`
  (unconfigured OAuth application / email), 8 in `search`, 1 in `api_v1`.

## Top failures grouped by root cause

Ordering is by number of examples affected. Counts are failures, not unique
defects; several groups include cascades from a single missing check.

### 1. ACL response bodies do not match Chef Server templates (acl: 348, keys: 19)

The generic `GET /organizations/:org/:type/:name/_acl` matrix fails for
clients, cookbooks, data bags, environments, groups, nodes, policies, policy
groups, roles, and containers. The server returns the stored ACEs, but the
default ACL template differs from upstream: the resource itself is not added
as an actor, and the expected built-in groups are not present. Example for a
new client:

```text
Expected a partial match of the result
  {"create" => {"actors" => ["pedant_admin_user", "pivotal"],
                "groups" => ["admins"]}, ...}
to the spec
  {"create" => {"actors" => ["pivotal", "new-object", "pedant_admin_user"],
                "groups" => ["admins"]}, ...}
```

`groups_acl_spec.rb` adds three security failures: a non-superuser can remove
the admin group from a grant ACE and gets `200`, where Pedant expects `403`.

### 2. `/users/:name/_acl` is empty and org admins are forbidden (acl: 52)

The endpoint exists but returns an all-empty ACL body:

```text
{"create"=>{"actors"=>[],"groups"=>[]}, "read"=>...}
```

The org admin then gets `403 {"error":["missing permission"]}` on every
`/users/:user/_acl/{permission}` PUT, so each ACL example fails twice (once
for the operation under test, once for the `reset_body` in the `after` hook).

### 3. `cookbook_artifacts` endpoints are missing (acl: 134)

Every `cookbook_artifacts` ACL example fails during setup:

```text
PUT /organizations/pedant_testorg/cookbook_artifacts/new-object/1111...
Response should have HTTP status code 201 ('Created'), but it was actually 404 ('Not Found')
  Reponse Body: {"error":["not found"]}
```

There is no `cookbook_artifacts` module or route in `microwave-api`. This is a
true endpoint gap, not an ACL defect.

### 4. Cookbook create responses contain only `uri` (cookbooks: ~42 direct, plus cascades)

`PUT /cookbooks/:name/:version` returns:

```json
{"uri":"http://127.0.0.1:54686/organizations/pedant_testorg/cookbooks/pedant_basic/1.0.0"}
```

Pedant expects the full `Chef::CookbookVersion` document (`name`,
`cookbook_name`, `version`, `json_class`, `chef_type`, `frozen?`, `recipes`,
`metadata`, and segment URLs). The same shape problem affects the API v0 and
API v2 contexts.

### 5. Cookbook validation gaps poison later examples (cookbooks: 26 direct + 50 × 409)

Invalid names (`first@second`), negative versions (`1.2.-42`), wrong
`json_class`, unknown top-level keys, UTF-8 keys, mismatched
`metadata.cookbook_name`, and missing sandbox checksums are all accepted with
`201`. Because `create_spec.rb` and `update_spec.rb` reuse fixed cookbook
names, each wrongly persisted object makes later examples return `409`:

```text
Response should have HTTP status code 400 ('Bad Request'), but it was actually 201 ('Created')
  Reponse Body: {"uri":".../cookbooks/first@second/1.2.3"}

Response should have HTTP status code 201 ('Created'), but it was actually 409 ('Conflict')
  Reponse Body: {"error":["cookbook version cookbook_name 1.2.3 already exists"]}
```

This amplification is why `cookbooks` has 504 failures from a much smaller
number of distinct bugs.

### 6. `/environments/:env/cookbooks` and `/:cookbook` routes are missing (cookbooks: 86)

`environments.rs` only routes `/environments` and `/environments/{env}`. The
environment-scoped cookbook listing and single-cookbook endpoints return
`404 {"error":["not found"]}`:

```text
GET /organizations/pedant_testorg/environments/_default/cookbooks
Response should have HTTP status code 200 ('OK'), but it was actually 404 ('Not Found')
```

This accounts for 27 failures in `environments/cookbooks_spec.rb` and 59 in
`environments/single_cookbook_spec.rb`.

### 7. Cookbook update semantics and response bodies (cookbooks: 221)

Most of the update matrix fails because the server treats a mismatched
`cookbook_name`/`version` as a create attempt (`409`) instead of a validation
error, and because successful PUTs return only `uri` rather than the updated
document. Some examples also fail because the fixture was poisoned by an
earlier accepted invalid create. Representative:

```text
Response should have HTTP status code 400 ('Bad Request'), but it was actually 409 ('Conflict')
  Reponse Body: {"error":["cookbook version cookbook-to-be-modified 11.2.3 already exists"]}
```

### 8. Sandbox commit response and checksum validation (sandboxes: 6, cookbooks: 20)

`POST /sandboxes` rejects a missing `checksums` with the wrong message
(`checksums is required` vs `Field 'checksums' missing`), accepts `{}` and
non-null checksum values that Pedant expects to be rejected, and does not
recognise an already-uploaded checksum (`needs_upload` stays `true`). The PUT
commit response is the biggest problem: it returns only `sandbox_id` and
`uri`, while Pedant expects `guid`, `name`, `checksums` (as an array),
`create_time`, and `is_completed`:

```text
Expected a partial match of the result
  {"sandbox_id"=>"a5c12...", "uri"=>"http://.../sandboxes/a5c12..."}
to the spec
  {"guid"=>"a5c12...", "name"=>"a5c12...",
   "checksums"=>["ec8f...", "3d97..."], "create_time"=>/2026-.../}
```

`commit_sandbox` returning no `checksums` is also why 20 cookbook checksum
update examples fail with `undefined method '[]' for nil`.

### 9. Data bag validation, messages, and response bodies (data_bags: 36)

The endpoints exist, but the contract is substantially different:

- Invalid data bag names (`pedant_badName!!$$$$_oh_very+bad`,
  `pedant-does-not-like-punctuation!!!!`) and invalid item IDs
  (`pedant_badId!!`, `^$@^*  pedant`) are accepted with `201`.
- `POST /data/:bag` success returns only `{"uri": ...}` instead of the item
  body with `id`, `chef_type: "data_bag_item"`, and `data_bag`.
- `DELETE /data/:bag` returns `{}` instead of the deleted bag document.
- `PUT /data/:bag/:item` for a missing item creates it (`200`) instead of
  returning `404`.
- Error bodies use different text, e.g. `data bag <name>` vs
  `Cannot load data bag <name>` and
  `Field 'name' missing` vs `data bag name is required`.
- `Allow` headers include `HEAD` (`GET,HEAD,POST` vs `GET, POST`).

### 10. Policy delete/create response bodies (policies: 6)

`POST /policies/:name/revisions` returns only `{"uri": ...}`; Pedant expects
the policy document that was posted. `DELETE` of a policy, policy group, or
revision returns `{}`; Pedant expects the revisions map, the policy group with
its `policies`, and the deleted revision respectively.

### 11. Policy-group association validation (policies: 28)

All 28 invalid-document examples are accepted with `201` where `400` is
expected: missing `name`, missing `run_list`, wrong `run_list` type, invalid
run-list entries (`123`, `recipe[`, `role[foo]`, `recipe[foo]`), missing or
malformed `cookbook_locks`, bad identifiers, and oversized identifiers. The
one message that is produced adds detail Pedant does not expect:

```text
expected: "Field 'name' invalid"
got:      "Field 'name' invalid : invalid+invalid does not match invalid invalid"
```

### 12. Depsolver body parsing and environment ACLs (depsolver: 5)

The full solver that landed on `main` resolves the run list, applies environment
constraints, expands dependencies, and returns `412` bodies correctly for the
cases that reach it. Five failures remain:

- Three malformed-body cases (`""`, non-JSON, malformed JSON) answer with the
  plain-text `Failed to parse the request body`, which Pedant cannot parse as
  JSON. The handler needs a JSON error body.
- The non-existent-cookbook case in `_default` and the
  `CHEF-3813` dependency-metadata case get `403 {"error":["missing permission"]}`.
  The requestor is an org admin; the solver now checks read access on the
  environment (`_default` included), and that environment's ACL is missing the
  admin grant. This is the same ACL-template issue as the `acl` tag, not a
  solver defect.

### 13. License response constants and node count (license: 6)

`GET /license` exists but returns `node_license: 1000`, `node_count: 0`, and
`upgrade_url: ""`. Pedant (whose `MAX_NODE_COUNT` is hard-coded to 25) expects
`node_license: 25`, a real `node_count` (0, 1, 25, 26 in the four contexts),
and an `https://` upgrade URL:

```text
actual: {"limit_exceeded"=>false,"node_count"=>0,"node_license"=>1000,"upgrade_url"=>""}
spec:   {"limit_exceeded"=>false,"node_license"=>25,"node_count"=>0,
         "upgrade_url"=>/^https\:\/\.*/}
```

### 14. OAuth sign-in page has no CSRF token (oauth: 7)

`GET /id/signin` returns an HTML form, but it has no
`<meta name="csrf-token" content="...">`. Pedant extracts that token before it
can POST the sign-in form, so all seven sign-in/profile/key-reset examples die
with:

```text
token = response.match(re)[1]
NoMethodError: undefined method '[]' for nil
```

The `/id/v1/status` example passes. The routes for callback, profile,
password, and key reset exist; this is an HTML response-shape gap (plus the
form currently posts to `/id/sessions` rather than the callback path).

### 15. Principals response shape and org membership (versioned_behaviors: 12)

Six failures are the not-found body: the server returns
`{"error":["principal not_a_number"]}` with an array value, while Pedant
expects `{"error":"Cannot find principal not_a_number","not_found":"principal"}`.
Four more are `org_member`: an outside user (`pedant_nobody`) is reported as
`org_member: true` when the spec expects `false`. Two are authentication:
requests signed by the unassociated `pedant_bad_user` get `401` where Pedant
expects the principal read to succeed.

### 16. Search client index row shape (search: 2)

`/search/client` rows omit `clientname`, `orgname`, and `public_key`:

```text
actual row: {"admin"=>false,"chef_type"=>"client","json_class"=>"Chef::ApiClient",
             "name"=>"pedant_admin_client","validator"=>false}
expected:   also clientname, orgname, public_key
```

### 17. Search tokenizer semantics (search: 1)

For data bag items named `foo` and `foo-bar`,
`id:foo* AND NOT bar` returns only `foo`; Pedant expects both. The embedded
search backend's tokenization/negation differs from the Solr/Elasticsearch
behaviour the suite encodes.

### 18. Client key response shapes (keys: part of 112 "managing keys" failures)

`POST .../keys` returns the full key object (`name`, `public_key`,
`expiration_date`, `uri`). Pedant's basic POST examples expect exactly
`{"uri": ...}` (plus `private_key` when a key is generated) and a matching
`Location` header. `PUT .../keys/:name` likewise returns the whole key, while
Pedant expects the patch payload echoed back. These examples therefore fail on
`body_exact` even though the key was created or updated correctly.

### 19. Client creation rejects the string `"admin": "true"` (keys: 25)

Pedant's key specs create clients with `"admin" => "true"` (a string, a legacy
API quirk). The server returns `400 {"error":["Field 'admin' is invalid"]}`
and creates nothing, so the subsequent 24 key lifecycle examples fail with
`404 client ...` or `401 Failed to authenticate`. This is a validation gap in
client creation, not a key endpoint defect.

### 20. Key reads ignore `public_key_read_access` (keys: 19)

Any org member or client should be able to read another actor's keys by
default (the `public_key_read_access` group contains the `users` and `clients`
groups). The server checks the target actor's ACL instead, so a second client
or a normal user gets `403 {"error":["missing permission"]}` where Pedant
expects `200`. Removing an actor from `public_key_read_access` also does not
revoke access because the group is not consulted at all.

### 21. Search index-deletion examples require a real Solr (search: 3, harness)

With `search_server` configured (needed for `word_break_spec.rb`), the three
`search_index_deletion` examples run `direct_solr_query` against the no-op
stub and fail in `get_response_count` because the stub has no Solr response
document. They are marked here as harness limitations, not server defects.

## Prioritized missing server behaviors

### P0 — highest failure reduction

1. **ACL templates and built-in groups** (≈ 420 ACL + 19 key failures).
   - Include the resource itself in its default ACL actors (clients,
     cookbooks, data bags, environments, groups, nodes, policies, policy
     groups, roles, containers).
   - Provision `::server-admins` (and the other `::` server groups) and
     include them where upstream does.
   - Make `GET /organizations/:org/:type/:name/_acl?detail=granular` return the
     `users`/`clients` split.
   - Fix `/users/:name/_acl` to return the real ACL and allow org admins to
     manage it.
   - Reject removal of the admin group's GRANT ACE by non-superusers (`403`).
   - **Type:** response-shape + authorization; no route gap.
2. **Implement `/cookbook_artifacts` CRUD/ACL routes** (134 failures).
   - **Type:** endpoint gap.
3. **Cookbook create/update contract** (≈ 300 failures).
   - Return the full cookbook document from `PUT /cookbooks/:name/:version`.
   - Validate name, version, `json_class`, `chef_type`, top-level keys, and
     `metadata` before persisting; reject `400` rather than create.
   - Distinguish create vs update by the URL/payload, and return `400` (not
     `409`) for a body/URL disagreement.
   - **Type:** response-shape + validation + behavior.
4. **Implement `/environments/:env/cookbooks` and
   `/environments/:env/cookbooks/:name`** (86 failures).
   - **Type:** endpoint gap.

### P1 — large, self-contained suites

5. **Sandbox commit contract** (6 sandbox + 20 cookbook failures).
   - Return `guid`/`name`, `checksums` as an array, `create_time`, and
     `is_completed` from `PUT /sandboxes/:id`.
   - Validate `checksums` (`{}` and non-string values → `400 Bad checksums!`),
     use the exact `Field 'checksums' missing` wording, and set
     `needs_upload: false` for checksums already in bookshelf.
   - **Type:** response-shape + validation.
6. **Data bag contract** (36 failures).
   - Validate bag/item names; return full item bodies on POST/PUT; return the
     deleted bag on DELETE; make PUT of a missing item `404`; align error text.
   - **Type:** validation + response-shape + behavior.
7. **Policy contract** (34 failures).
   - Return the posted policy document from revision POST and the deleted
     document/list from DELETE.
   - Validate policy-group association documents and return `400` with the
     exact `Field '...'` messages.
   - **Type:** response-shape + validation.
8. **Depsolver** (5 failures).
   - Return JSON for malformed request bodies instead of the plain-text
     `Failed to parse the request body`.
   - Grant org admins read on `_default`/environment ACLs so the solver's
     environment check succeeds (same ACL-template fix as P0).
   - **Type:** response-shape + ACL/authorization; the solver itself is
     implemented.

### P2 — narrow but visible

9. **Client creation `admin` boolean compatibility** (25 key failures).
   - Accept `"admin": "true"`/`"false"` strings as Pedant and older clients
     send them.
   - **Type:** validation.
10. **Key read authorization** (19 key failures).
    - Honour the `public_key_read_access` group for key list/get; create the
      group with `users` and `clients` by default.
    - **Type:** authorization.
11. **Key response shapes** (part of 112 key failures).
    - POST returns `{"uri": ...}` (+ `private_key` when generated); PUT echoes
      the patch payload. Keep the full object on GET.
    - **Type:** response-shape.
12. **License** (6 failures).
    - Return the configured node limit (25 for the Pedant contract), count
      nodes for `node_count`, and a real `upgrade_url`; `limit_exceeded` must
      reflect the count.
    - **Type:** response-shape/behavior.
13. **OAuth HTML** (7 failures).
    - Add `<meta name="csrf-token" content="...">` to `/id/signin`, accept the
      form post at `/id/auth/chef/callback`, and implement the profile/regen
      responses Pedant parses.
    - **Type:** response-shape; routes exist.
14. **Principals** (12 failures).
    - Not-found body must be `{"error": "Cannot find principal <name>",
      "not_found": "principal"}`; `org_member` must reflect actual org
      membership; unassociated users must still be able to read principals.
    - **Type:** response-shape + authorization.
15. **Search client rows** (2 failures) and **tokenizer** (1 failure).
    - Add `clientname`, `orgname`, and `public_key` to client index documents.
    - Match Chef's `foo* AND NOT bar` tokenization behaviour.
    - **Type:** response-shape + search semantics.

## Seeding

The extended suites need **no additional seed data beyond the existing
bootstrap**. This was verified by running them against a fresh org created by
`scripts/pedant-bootstrap.rb`:

- `cookbooks`, `depsolver`, and `sandboxes` upload their own cookbooks,
  sandboxes, and checksums through the API.
- `policies` creates its own policy names, revisions, and groups.
- `data_bags` creates and deletes its own bags/items, including the
  "with no data bags" setup.
- `keys` creates its own clients, users, orgs, and keys.
- `acl` creates its own objects (roles, environments, nodes, cookbooks, data
  bags, policies, containers) and fails on ACL shape/route gaps, not on missing
  fixtures.
- `search` creates its own nodes/roles/environments/data bags/clients and then
  searches the embedded index.
- `license`, `oauth`, `versioned_behaviors`, and `depsolver` use the seeded
  superuser/admin/user/client actors or objects they create themselves.

Pre-seeding a cookbook, data bag, or policy would actively break empty-state
examples (`GET /cookbooks without existing cookbooks`, `with no data bags`,
`when no policies exist`, and the `/search` built-in-index list). No direct
SQLite writes were added for the extended tags.

## Known Pedant bugs and harness limitations

1. **Search `force_solr_commit` assumes a Solr server.** 376 failures in the
   first search run were the harness, not the server. Fixed with
   `scripts/pedant-solr-stub.rb` + `search_server` config. The remaining three
   `search_index_deletion` examples genuinely need a real Solr and are only
   meaningful for an external-index deployment.
2. **`license_spec.rb` hard-codes `MAX_NODE_COUNT = 25`** and an `https://`
   `upgrade_url` regex, with a TODO to move it to config. If Microwave intends
   a 1,000-node default, this spec (or the server's contract) must be
   reconciled deliberately.
3. **Cookbook create/update validation failures cascade.** `create_spec.rb`
   and `update_spec.rb` reuse fixed names such as `cookbook_name`, so one
   missing validation produces a later `409` and inflates the count. The 504
   cookbook failures are not 504 independent defects.
4. **ACL `cookbook_artifacts` failures are setup cascades.** The 134 examples
   fail because the object cannot be created; they are counted under the
   missing endpoint, not as separate ACL defects.
5. **`--focus api_v1` also selects `principal_spec.rb`.** The
   `versioned_behaviors` tag count therefore mixes the (green)
   `server_api_v1_spec.rb` with principal access-control examples.
6. **Key specs use the string `"admin": "true"`.** This is a legacy Pedant
   quirk that upstream accepts; it is worth supporting for old clients, but it
   is also why 25 key failures are not key-endpoint failures.
7. **Exact `Allow` headers include `HEAD`.** Axum adds `HEAD` automatically,
   so `GET,HEAD,POST` differs from Pedant's expected `GET, POST`. This affects
   data bag method-not-allowed examples and is a framework-level shape gap.
8. **Plain-HTTP core artifact.** The five-tag `headers` failure
   (`request_url.sub!("https", "http")` returning `nil`) is unchanged and is
   not part of this extended run.
9. **ACL runtime.** `acls` is 1,640 examples and takes ~16 minutes; it is the
   long pole. `keys` takes ~10 minutes because it creates a fresh org and
   many keys.
10. **Harness edit safety.** `run-pedant.sh` reads itself while running; do not
    edit it during a run (one early search run was aborted at summary time
    after a mid-run edit; the RSpec results were still complete and the
    artifact was kept).
