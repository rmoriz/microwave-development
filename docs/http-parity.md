# HTTP API parity and tests

The signed HTTP API is the compatibility contract. This page lists what it
covers and the suites that hold it in place for both metadata backends.

## API surface

`/_status`, `/_stats`, `/server_api_version`, `/license`, users, organizations,
clients, principals, key management, organization memberships and invitations,
nodes, node `_identifiers`, environments, environment recipes, roles (including
environment run lists), data bags, sandboxes, bookshelf uploads, cookbooks,
cookbook resolution, policyfiles and policy groups, ACL endpoints,
signed-request validation (`/validate/*`), search, data-collector forwarding
and opt-in mutation action notifications, the compliance profile proxy, the
removed analytics `controls` endpoint (`410 Gone`), the optional
server-enforced `required_recipe` endpoint, and the `/id` identity, OAuth, and
password-reset routes, including `/id/v1/me`, `/id/v1/me/organizations`, and
`/id/v1/status`.

`/validate/*` and `_identifiers` are internal upstream APIs that Chef Infra
Server only routes on its internal listener. Microwave exposes them on the main
listener to signed organization members, so restrict them at the reverse proxy
if the upstream network split is required.

Authorization uses recursive ACL/group checks with cycle rejection, the default
organization policy, and ACL read/write endpoints with granular `users` and
`clients` responses.

## Dual-backend store matrix

[`crates/microwave-store/tests/dual_backend.rs`](../crates/microwave-store/tests/dual_backend.rs)
contains one async suite for every repository domain and runs it against both
backends through `MetadataStore`:

- identity: orgs, users (profiles/patches), clients, actor keys, principals
- authorization: groups/containers, nested and global groups, ACL replacement,
  recursive permission checks, org read-access ACEs
- objects: nodes, roles, environments, data bags and bag items
- cookbooks: sandboxes, checksum commit/purge, version create/find/update/delete
- policies: revisions, policy groups, associations
- artifacts: artifact/version create/find/delete and checksum purge
- membership: add/remove, invitations accept/reject/delete, `users`/`admins`
- OAuth: applications, grants, access/refresh tokens, ID sessions, password resets
- KV/secrets: raw KV, maintenance flag, secret bootstrap

SQLite always runs in-memory. The PostgreSQL half runs when
`MICROWAVE_TEST_POSTGRES_URL` is set and is skipped only when the variable is
unset; a configured database that cannot connect or migrate fails the test.
`scripts/run-postgres-tests.sh` exports the URL and runs the matrix as part of
the PostgreSQL gate. Names and checksums are unique per run, assertions that
could see other rows (such as node counts) are scoped to the fixture
organization, and org-scoped fixtures plus test-created global groups are
deleted afterwards. The shared `server-admins` group and the global
maintenance KV flag are intentionally left in place. The maintenance test is
serialized within the test binary and restores the previous flag even when an
assertion panics; separate processes sharing one database must not run their
maintenance assertions concurrently.

## HTTP route parity suite

[`crates/microwave-api/tests/http_parity_test.rs`](../crates/microwave-api/tests/http_parity_test.rs)
runs the same signed HTTP scenarios through SQLite and PostgreSQL and compares
status, `Location`/content-type headers, and normalized bodies for users
(including deletion), organizations, clients, keys, nodes, roles, environments,
data bags, groups, containers, sandboxes, cookbooks, cookbook artifacts,
policies and policy groups, search, user/org/object/data-bag/artifact ACLs,
membership and invitations, OAuth application/token and v1 identity/user
routes, credential authentication, system recovery, organization deletion
(including dependent checksum, client-key, sandbox, ACL and global
read-access-group rows, with atomic rollback and `server-admins` preservation),
license, status/stats, internal node identifiers, signed-request validation,
analytics `controls`, environment recipes, role environment run lists, and
depsolver.

The PostgreSQL half skips only when `MICROWAVE_TEST_POSTGRES_URL` is unset; set
`MICROWAVE_REQUIRE_POSTGRES=1` or `true` to make an unset URL, missing Docker,
or missing client tools a hard failure. Cleanup asserts every deletion, checks
captured client keys and sandbox rows, verifies no run-named organizations,
users, OAuth applications, or global groups survive, and keeps a panic-safe
PostgreSQL drop guard.

Remaining parity gaps are OAuth HTML session/profile pages and remote
OpenSearch/Elasticsearch search, which rely on their dedicated suites.
