# Server-admins parity with erchef — design

**Date:** 2026-11-11
**Status:** approved approach, awaiting implementation
**Related:** cinc-server-ng issue #259, `docs/superpowers/plans/2026-10-26-p0-security-and-data-exposure.md`

## Intent

Make a non-superuser member of the global `server-admins` group able to do
exactly what the Erlang `erchef` server lets them do — and nothing more. Today
Microwave models the group, seeds per-user ACEs for it, ships
`grant-server-admin-permissions`, and imports membership, but every user
endpoint is gated on the bootstrap superuser, so those ACEs are dead. An
operator who migrated from Chef Server keeps their server admins but those
users still get `403`.

## What erchef actually does

All references are to the vendored tree in `chef-server/`.

- Bootstrap (`src/oc_erchef/habitat/config/chef_server_data_bootstrap.rb:119-161`)
  creates two **global containers**, `users` and `organizations`, then:
  - grants `server-admins` `create read update delete` on the `users`
    container,
  - grants the superuser all permissions on both containers (as their creator),
  - grants `server-admins` `read` on the `server-admins` group itself.
- There is **no** grant of `server-admins` on the `organizations` container.
- Authorization (`src/oc_erchef/apps/oc_chef_wm/src/`):
  - `GET /users` → read on the `users` container (`oc_chef_wm_users.erl:76`)
  - `POST /users` → create on the `users` container (`:78`)
  - `GET/PUT/DELETE /users/{name}` → read/update/delete on that user's own
    ACL (`oc_chef_wm_named_user.erl:126-128,143-144`)
  - `PUT /users/{name}` becomes superuser-only when `external_authentication_uid`
    or `recovery_authentication_enabled` is present in the body (`:134-142`)
  - `GET/POST /organizations` → read/create on the `organizations` container
    (`oc_chef_wm_organizations.erl:81-84`)
  - `POST /organizations/{org}/users` stays superuser-only
    (`oc_chef_wm_org_associations.erl:117-126`), as ours already is.
- A new object's ACL is the container ACL merged with "the requestor gets all
  permissions" (`oc_chef_authz.erl:134-180`). The account service also gives the
  new account all permissions on itself, which we already seed.
- The superuser account is created by direct SQL, outside that object path
  (`chef_server_data_bootstrap.rb:180-233`), so its ACL is empty. That is the
  mechanism behind "superuser accounts may not be managed by server-admins"
  (`docs-chef-io/content/server/reusable/md/server_rbac_server_admins_superusers.md`).
- Denial body is `{"error":["missing <permission> permission"]}`, which our
  `require_permission` already produces.

Observable consequence: `GET /organizations` is **403** for a non-superuser
server-admin. Our P0 helper `is_server_admin` deliberately returned 200; that
deviation is removed by this design.

## Goals

- A member of `server-admins` (or a user whose `users.admin` flag mirrors that
  membership) can list, read, create, update and delete users over the signed
  HTTP API, including users in organizations they do not belong to.
- A server-admin can read and replace ACLs on a user they created, because the
  requestor-all grant includes `grant`.
- The superuser account cannot be read, updated or deleted by a server-admin.
- `GET /organizations` and `POST /organizations` authorize through the
  `organizations` container, so the superuser passes and server-admins do not.
- A normal user keeps exactly today's access: own profile, co-members of a
  shared organization (through `::<org>_read_access_group`), nothing else.
- The model survives export/import: our snapshot tooling, the goiardi importer
  and the Chef/knife backup importer all produce the same container rows and
  ACEs as a fresh bootstrap.

## Non-goals

- Changing the `chef-server-ctl`-shaped commands. They already write
  membership; `update_user_patch` keeps the flag and the group in sync.
- Changing who may create or edit *clients*, cookbooks, nodes, or any other
  organization-scoped object.
- Reworking the `users.admin` column. It stays as the mirror of group
  membership and keeps its existing JSON field.
- Matching erchef's `PUT /users/{name}` email-verification rules or the
  `?verbose=true` body shape. Only authorization changes here.

## Anti-goals

- A server-admin deleting or renaming the bootstrap superuser.
- Startup re-seeding that overwrites operator-edited ACLs on the global
  containers or on the superuser account.
- A second authorization path for MCP or the CLI that skips these checks.

## Approaches considered

1. **Real global containers (chosen).** Add a `global_containers` table next to
   `global_groups`, seed exactly what the bootstrap script seeds, make user
   ACLs inherit the `users` container, and authorize the six endpoints through
   those ACEs. Faithful data model, ACL responses and grant semantics; costs a
   schema addition in both backends plus snapshot/importer plumbing.
2. **`is_server_admin` gate only.** Gate `GET/POST /users` on the existing
   helper and keep `PUT/DELETE` on the per-user ACLs. No schema change, but no
   container rows, no creator grant on created users, no way to carry a
   container ACE from a backup, and `GET /organizations` stays more permissive
   than upstream. Rejected: it reproduces statuses, not behaviour.

## Design

### Data model

New table in both `crates/microwave-store/migrations/0001_initial.sql` and
`crates/microwave-store-postgres/migrations/0001_initial.sql`:

```sql
CREATE TABLE IF NOT EXISTS global_containers (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

`GlobalContainer { id, authz_id, name }` mirrors `Group`. Trait methods on
`AuthzRepository`: `ensure_global_container`, `find_global_container`,
`list_global_containers`, and `revoke_group_acl(target, group)` (used to remove
the inherited `server-admins` ACEs from the superuser account). The orphan
predicate used by `cleanup-bifrost` gains a `global_containers` clause in both
backends, otherwise cleanup would delete the containers' authz entities.

### Seeding

`ensure_global_container(name)` creates the row and its `authz_entities` entry
(race-safe, like `ensure_global_group`) and, for `name = "users"`, grants
`::server-admins` create/read/update/delete on it. `organizations` gets no
group grant, matching the bootstrap script.

Bootstrap (`bin/microwave-server/src/bootstrap.rs::ensure_superuser`), on every
start, idempotently:

1. ensures both containers and the `server-admins` group,
2. grants the configured superuser every permission on both containers,
3. revokes the `server-admins` group from the superuser's own user ACL, so the
   account matches erchef's empty-ACL superuser.

Step 3 is also the upgrade path for existing installs; it only removes that
one group, never operator-made actor or group grants.

`microwave_authz::create_organization` keeps its existing
`ensure_global_group` calls and also ensures both containers, so a store whose
bootstrap predates this change still gets them.

### User creation

`create_user_with_profile` (SQLite and PostgreSQL) replaces the hard-coded
`server-admins` inserts with a copy of the `users` container ACL — every actor
and group ACE, all permissions — followed by the existing self-`grant_actor_all`.
The API layer keeps granting the requestor all permissions after creation,
which matches "the requestor gets all permissions on the new object".

### Endpoint authorization

`helpers.rs` gains:

```rust
pub async fn require_global_container_permission(
    state: &AppState,
    auth: &AuthContext,
    container: &str,
    permission: Permission,
) -> Result<(), ApiError>;
```

`is_server_admin` is removed. `users.rs`:

| Endpoint | Check |
|---|---|
| `GET /users` | read on the `users` container |
| `POST /users` | create on the `users` container |
| `GET /users/{name}` | read on the target user's ACL (replaces `authorize_read`) |
| `PUT /users/{name}` | update on the target, unless the body has `external_authentication_uid` or `recovery_authentication_enabled` → superuser only |
| `DELETE /users/{name}` | delete on the target |

`PUT` and `DELETE` also call `require_superuser` when the target is the
configured superuser. With the revoked ACL this changes no observable status;
it is defence in depth against a drifted ACL (see Review Focus).

`organizations.rs`: `GET /organizations` → read on the `organizations`
container, `POST /organizations` → create on it. Named-organization
`GET/PUT/DELETE` keep their current organization-ACL checks, which already
match erchef.

### Snapshots and importers

`StoreSnapshot` gains `global_containers` (`GlobalContainerRow`), collected and
re-inserted like `global_groups`. `microwave-migrate` reconciles them by name
so importing a snapshot into a target that already bootstrapped reuses the
target's rows instead of failing on the unique name. The goiardi importer
emits the two standard containers with the bootstrap ACEs; the Chef/knife
importer ensures they exist when it rebuilds the standard graph.

### Testing strategy

The new `crates/microwave-api/tests/server_admin_test.rs` reproduces the
endpoint matrix from a server-admin, a normal user and the superuser, including
the superuser-account denial, the co-organization read, and the
sensitive-field-on-`PUT` gate. Store-level tests cover container idempotency,
ACL inheritance, the orphan predicate and snapshot round-trips on both
backends. `organization_auth_test.rs` flips its server-admin listing assertion
to 403.

## Decisions made here

- **`GET /organizations` becomes 403 for server-admins.** Exact parity wins over
  the P0 shortcut; the P0 plan stays as history.
- **The superuser's inherited `server-admins` ACEs are revoked on every start**
  rather than only at creation, so upgrades converge. Only that group is
  removed.
- **`PUT`/`DELETE` on the configured superuser require the superuser**, in
  addition to the ACL. This is the one intentional hardening beyond erchef; it
  produces no status difference in the default state.
- **`ensure_global_container` seeds the `users` policy itself** instead of
  relying on a separate policy call, so every fixture and every store created
  through `create_user` gets the same graph. This mirrors how
  `ensure_global_group` seeds its own ACL today.
