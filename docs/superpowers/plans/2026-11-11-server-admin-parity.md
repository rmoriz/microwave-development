# Server-admins Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a non-superuser member of the global `server-admins` group able to list, read, create, update and delete users exactly as the Erlang `erchef` server allows, through the same ACL graph.

**Architecture:** Add global `users` and `organizations` containers to both store backends, seed them as `chef_server_data_bootstrap.rb` does, inherit the `users` container ACL when creating a user, and replace the `require_superuser` gates on the user and organization endpoints with container/object ACL checks. Carry the containers through snapshots and importers so every layout ends up with the same graph.

**Tech Stack:** Rust workspace, `rusqlite` (SQLite) and `sqlx` (PostgreSQL) backends behind `MetadataStore`, axum API, TOML-driven tests.

**Spec:** `docs/superpowers/specs/2026-11-11-server-admin-parity-design.md`

## Global Constraints

- Every store change lands in **both** `crates/microwave-store` (SQLite) and `crates/microwave-store-postgres`; the PostgreSQL half of `crates/microwave-store/tests/dual_backend.rs` runs when `MICROWAVE_TEST_POSTGRES_URL` is set.
- The only new schema is two `CREATE TABLE IF NOT EXISTS` blocks in each backend's `migrations/0001_initial.sql`; there are no numbered follow-up migrations in this repo.
- Denial bodies stay `{"error":["missing <permission> permission"]}`; keep using `require_permission`.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets` before every commit.
- One focused commit per task, imperative subject with a scope (`feat(store):`, `fix(api):`).
- Documentation changes are asserted by `docs/claims.toml`; run `cargo test --workspace` after touching docs.
- Do not touch `crates/microwave-api/src/clients.rs` or `tests/clients_test.rs`; they carry unrelated working-tree edits.

## Review Focus

- **Upgrading an existing install.** `ensure_superuser` runs on every start and must be idempotent: re-seeding must not duplicate ACEs, and the superuser revoke must only drop the `server-admins` group.
- **A server-admin creating a user.** The creator must end up with all five permissions on the new account, so they can later replace its ACL.
- **Protecting the bootstrap superuser.** A drifted ACL that still names `server-admins` must not let a server-admin read, update or delete the configured superuser.
- **Co-organization reads.** A normal user who shares an organization with another user must still read that profile through `::<org>_read_access_group`; the ACL switch must not regress this.
- **Importing into a bootstrapped target.** A snapshot that contains its own `users`/`organizations` containers must reuse the target's rows instead of failing on the unique name.

---

### Task 1: Global containers in both store backends

**Files:**
- Modify: `crates/microwave-store/migrations/0001_initial.sql` (after `global_groups`, around line 291)
- Modify: `crates/microwave-store-postgres/migrations/0001_initial.sql` (after `global_groups`, around line 232)
- Modify: `crates/microwave-store/src/authz.rs` (`Group` at line 13, predicate at line 282, global-group methods at 386-432)
- Modify: `crates/microwave-store/src/backend.rs` (after `delete_global_group`, line 348)
- Modify: `crates/microwave-store/src/backend_sqlite.rs` (after line 500)
- Modify: `crates/microwave-store/src/lib.rs:26`
- Modify: `crates/microwave-store-postgres/src/authz.rs` (predicate at line 18, `ensure_global_group` at 1018)
- Modify: `crates/microwave-authz/src/org.rs:77-82`
- Test: `crates/microwave-store/tests/dual_backend.rs`

**Interfaces:**
- Produces: `microwave_store::GlobalContainer { id: String, authz_id: String, name: String }`
- Produces: `AuthzRepository::ensure_global_container(&self, name: &str) -> Result<GlobalContainer, StoreError>`, `find_global_container(&self, name: &str) -> Result<Option<GlobalContainer>, StoreError>`, `list_global_containers(&self) -> Result<Vec<GlobalContainer>, StoreError>`, `revoke_group_acl(&self, target_authz_id: &str, group_authz_id: &str) -> Result<(), StoreError>`
- Produces: `ensure_global_container("users")` seeds `::server-admins` `create/read/update/delete` on that container; `"organizations"` seeds nothing.

- [ ] **Step 1: Write the failing store test**

Add this scenario to the `suite` module of `crates/microwave-store/tests/dual_backend.rs` and list `global_containers` in both `matrix!` blocks.

```rust
pub async fn global_containers(store: &Arc<dyn MetadataStore>) -> Vec<String> {
    let users = store.ensure_global_container("users").await.expect("ensure users");
    let again = store
        .ensure_global_container("users")
        .await
        .expect("ensure users again");
    assert_eq!(users.authz_id, again.authz_id, "ensure must be idempotent");

    let entries = store
        .get_acl_entries(&users.authz_id)
        .await
        .expect("users container acl");
    for permission in [Permission::Create, Permission::Read, Permission::Update, Permission::Delete] {
        let entry = entries
            .iter()
            .find(|entry| entry.permission == permission)
            .unwrap_or_else(|| panic!("missing {permission:?} ACE"));
        assert!(
            entry.groups.contains(&"::server-admins".to_string()),
            "server-admins missing from {permission:?}: {entry:?}"
        );
    }
    let grant = entries
        .iter()
        .find(|entry| entry.permission == Permission::Grant)
        .expect("grant ACE row");
    assert!(grant.groups.is_empty(), "server-admins must not hold grant");

    let organizations = store
        .ensure_global_container("organizations")
        .await
        .expect("ensure organizations");
    assert!(
        store
            .get_acl_entries(&organizations.authz_id)
            .await
            .expect("organizations container acl")
            .is_empty(),
        "the organizations container has no group grants upstream"
    );

    let names: Vec<String> = store
        .list_global_containers()
        .await
        .expect("list global containers")
        .into_iter()
        .map(|container| container.name)
        .collect();
    assert!(names.contains(&"users".to_string()));
    assert!(names.contains(&"organizations".to_string()));

    store
        .delete_orphan_authz_entities()
        .await
        .expect("cleanup keeps referenced entities");
    assert!(store.find_global_container("users").await.unwrap().is_some());
    Vec::new()
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p microwave-store --test dual_backend global_containers 2>&1 | tail -20`
Expected: compile error, `no method named ensure_global_container`.

- [ ] **Step 3: Add the table to both schemas**

Append after the `global_groups` block in each `0001_initial.sql`:

```sql
-- Containers outside any organization. Upstream bootstraps the global
-- `users` and `organizations` containers and authorizes account and
-- organization creation through their ACLs.
CREATE TABLE IF NOT EXISTS global_containers (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

- [ ] **Step 4: Add the type, trait methods and both implementations**

`crates/microwave-store/src/authz.rs`, next to `Group`:

```rust
/// A container that is not scoped to an organization (`users`,
/// `organizations`). Upstream keeps them in `containers` under the global
/// placeholder org id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalContainer {
    pub id: String,
    pub authz_id: String,
    pub name: String,
}
```

Implement `ensure_global_container` by copying `ensure_global_group`'s race-safe transaction: insert the `authz_entities` row with `entity_type = 'container'`, insert the `global_containers` row with `INSERT OR IGNORE`, roll back on a lost race and re-read, then for `name == "users"` call `self.ensure_global_group("server-admins")` **before** opening the transaction and insert the four `authz_acl_group` rows (`INSERT OR IGNORE`). `find_global_container` and `list_global_containers` mirror the group queries. `revoke_group_acl` deletes every `authz_acl_group` row for `(target_authz_id, group_authz_id)` and returns `Ok(())` when nothing matched.

Add `and NOT EXISTS (SELECT 1 FROM global_containers WHERE global_containers.authz_id = authz_entities.authz_id)` to `ORPHAN_AUTHZ_ENTITY_PREDICATE` in **both** `crates/microwave-store/src/authz.rs` and `crates/microwave-store-postgres/src/authz.rs`.

In `crates/microwave-store-postgres/src/authz.rs`, implement the four trait methods with `sqlx` following `ensure_global_group` (`ON CONFLICT (name) DO NOTHING`), and export the type from `lib.rs`.

In `crates/microwave-authz/src/org.rs::apply_default_policy`, next to the existing
`ensure_global_group("server-admins")` call, add
`store.ensure_global_container("users").await?` and
`store.ensure_global_container("organizations").await?`, so a store that
predates this change gets the containers on its next organization creation.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p microwave-store --test dual_backend global_containers`
Expected: `2 passed` (sqlite and postgres) — or `1 passed` when `MICROWAVE_TEST_POSTGRES_URL` is unset, with the skip notice printed.

Then run: `scripts/run-postgres-tests.sh` if Docker is available; expected: both halves pass.

- [ ] **Step 6: Commit**

```bash
git add crates/microwave-store crates/microwave-store-postgres crates/microwave-authz
git commit -m "feat(store): add global containers and server-admin grants"
```

---

### Task 2: New users inherit the `users` container ACL

**Files:**
- Modify: `crates/microwave-store/src/identity.rs:265-348`
- Modify: `crates/microwave-store-postgres/src/identity.rs:213-330`
- Test: `crates/microwave-store/tests/dual_backend.rs`
- Test: `crates/microwave-store/tests/identity_test.rs`

**Interfaces:**
- Consumes: `ensure_global_container("users")` and `GlobalContainer` from Task 1.
- Produces: `create_user_with_profile` copies every actor and group ACE of the `users` container onto the new user's `authz_id`, for all five permissions, then keeps the existing self `grant_actor_all`.

- [ ] **Step 1: Write the failing tests**

Add `Permission` to the `use microwave_store::{...}` line in
`crates/microwave-store/tests/identity_test.rs`. The failing test is
`identity_test.rs` (in-memory SQLite, safe to mutate the canonical container) —
it proves the copy is generic rather than hard-coded:

```rust
#[test]
fn create_user_copies_custom_container_aces() {
    let store = SqliteStore::open_in_memory().unwrap();
    let container = store.ensure_global_container("users").unwrap();
    let reader = store
        .create_user("reader", "reader@example.com", "bcrypt", "h", "s", false)
        .unwrap();
    store
        .set_acl(&container.authz_id, Permission::Read, Some(&reader.authz_id), None)
        .unwrap();
    let created = store
        .create_user("copied", "copied@example.com", "bcrypt", "h", "s", false)
        .unwrap();
    let entries = store.get_acl_entries(&created.authz_id).unwrap();
    let read = entries
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .expect("read ACE");
    assert!(read.actors.contains(&"reader".to_string()), "{read:?}");
}
```

Also add this regression guard to `dual_backend.rs` (and both `matrix!` lists).
It passes before and after the change; run it first to record the baseline and
again after Step 3 so the backend-neutral default stays pinned:

```rust
pub async fn users_inherit_container_acl(store: &Arc<dyn MetadataStore>) -> Vec<String> {
    let name = unique("inherited-user");
    let user = store
        .create_user(&name, &format!("{name}@example.com"), "bcrypt", "h", "s", false)
        .await
        .expect("create user");
    let entries = store.get_acl_entries(&user.authz_id).await.expect("user acl");
    for permission in [Permission::Create, Permission::Read, Permission::Update, Permission::Delete] {
        let entry = entries
            .iter()
            .find(|entry| entry.permission == permission)
            .unwrap_or_else(|| panic!("missing {permission:?} ACE"));
        assert!(
            entry.groups.contains(&"::server-admins".to_string()),
            "server-admins missing from {permission:?}: {entry:?}"
        );
        assert!(
            entry.actors.contains(&name),
            "user missing from its own {permission:?} ACE: {entry:?}"
        );
    }
    Vec::new()
}
```

- [ ] **Step 1b: Record the dual-backend baseline**

Run: `cargo test -p microwave-store --test dual_backend users_inherit_container_acl`
Expected: PASS both backends today. It must still pass after Step 3.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p microwave-store --test identity_test create_user_copies_custom_container_aces`
Expected: FAIL — `reader` is absent from `copied`'s ACL.

- [ ] **Step 3: Replace the hard-coded inserts with a container copy**

In both `create_user_with_profile` implementations, keep the existing
`server_admins` lookup (it still drives the `admin` membership insert) and
replace only the hard-coded four-permission ACE loop. Next to the group lookup:

```rust
let users_container = self.ensure_global_container("users")?; // + .await in PostgreSQL
```

and inside the transaction, after inserting the user and `authz_entities` row:

```sql
INSERT OR IGNORE INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
SELECT ?1, permission, actor_authz_id
FROM authz_acl_actor WHERE target_authz_id = ?2;

INSERT OR IGNORE INTO authz_acl_group (target_authz_id, permission, group_authz_id)
SELECT ?1, permission, group_authz_id
FROM authz_acl_group WHERE target_authz_id = ?2;
```

(PostgreSQL: `INSERT ... SELECT ... ON CONFLICT DO NOTHING`.) Keep the following `grant_actor_all(&authz_id, &authz_id)` and the `admin` group-membership insert unchanged.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-store --test dual_backend users_inherit_container_acl && cargo test -p microwave-store --test identity_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-store crates/microwave-store-postgres
git commit -m "feat(store): inherit the users container ACL on user creation"
```

---

### Task 3: Bootstrap seeds the containers and protects the superuser

**Files:**
- Modify: `bin/microwave-server/src/bootstrap.rs:25-90` (`ensure_superuser`)
- Test: `bin/microwave-server/src/bootstrap.rs` (existing `#[cfg(test)]` module near line 600)

**Interfaces:**
- Consumes: `ensure_global_container`, `revoke_group_acl`, `grant_actor_all`, `find_global_group` from Tasks 1-2.
- Produces: `async fn ensure_platform_acl(store: &dyn MetadataStore, superuser: &microwave_store::User) -> Result<(), Box<dyn std::error::Error>>` in `bootstrap.rs`, called at the end of `ensure_superuser`.

- [ ] **Step 1: Write the failing tests**

Add to `mod superuser_tests` in `bin/microwave-server/src/bootstrap.rs`. That
module uses a concrete `SqliteStore`, so the store calls are the sync inherent
ones; add `Permission` to its imports.

```rust
#[tokio::test]
async fn ensure_superuser_seeds_platform_containers() {
    let data_dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.paths.data_dir = data_dir.path().to_string_lossy().into_owned();
    config.auth.superuser_name = "root".into();
    let store = SqliteStore::open_in_memory().unwrap();

    ensure_superuser(&store, &config).await.unwrap();
    let user = store.find_user("root").unwrap().unwrap();

    let users = store.find_global_container("users").unwrap().expect("users container");
    let organizations = store
        .find_global_container("organizations")
        .unwrap()
        .expect("organizations container");
    for container in [&users, &organizations] {
        let entries = store.get_acl_entries(&container.authz_id).unwrap();
        for permission in Permission::ALL {
            let entry = entries.iter().find(|entry| entry.permission == permission)
                .unwrap_or_else(|| panic!("missing {permission:?} on {}", container.name));
            assert!(entry.actors.contains(&user.username), "{:?}", container.name);
        }
    }
    let entries = store.get_acl_entries(&user.authz_id).unwrap();
    assert!(
        entries.iter().all(|entry| !entry.groups.contains(&"::server-admins".to_string())),
        "the bootstrap superuser must not be manageable by server-admins: {entries:?}"
    );
}

#[tokio::test]
async fn ensure_superuser_is_idempotent_and_keeps_operator_grants() {
    let data_dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.paths.data_dir = data_dir.path().to_string_lossy().into_owned();
    config.auth.superuser_name = "root".into();
    let store = SqliteStore::open_in_memory().unwrap();

    ensure_superuser(&store, &config).await.unwrap();
    let user = store.find_user("root").unwrap().unwrap();
    let group = store.ensure_global_group("operator-probe").unwrap();
    store
        .set_acl(&user.authz_id, Permission::Read, None, Some(&group.authz_id))
        .unwrap();

    ensure_superuser(&store, &config).await.unwrap();

    let entries = store.get_acl_entries(&user.authz_id).unwrap();
    let read = entries.iter().find(|entry| entry.permission == Permission::Read).unwrap();
    assert!(read.groups.contains(&"::operator-probe".to_string()), "{read:?}");
    assert!(!read.groups.contains(&"::server-admins".to_string()), "{read:?}");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p microwave-server ensure_superuser_`
Expected: FAIL — `find_global_container` missing or the containers absent.

- [ ] **Step 3: Implement `ensure_platform_acl` and call it**

At the end of `ensure_superuser`, after the key block:

```rust
ensure_platform_acl(store, &user).await?;
```

Implement:

```rust
/// Reproduce the erchef bootstrap: the global `users` and `organizations`
/// containers exist, their creator (the configured superuser) holds every
/// permission on them, and the superuser account itself is not manageable by
/// the `server-admins` group. Idempotent; safe on every start.
async fn ensure_platform_acl(
    store: &dyn MetadataStore,
    superuser: &microwave_store::User,
) -> Result<(), Box<dyn std::error::Error>> {
    let users = store.ensure_global_container("users").await?;
    let organizations = store.ensure_global_container("organizations").await?;
    for container in [&users, &organizations] {
        store.grant_actor_all(&container.authz_id, &superuser.authz_id).await?;
    }
    let server_admins = store.ensure_global_group("server-admins").await?;
    store
        .revoke_group_acl(&superuser.authz_id, &server_admins.authz_id)
        .await?;
    Ok(())
}
```

Add the missing `use microwave_store::{GlobalContainer, Permission};` if the test module needs them (imports belong in the test module, not the binary root).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-server bootstrap`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bin/microwave-server/src/bootstrap.rs
git commit -m "feat(server): seed platform containers and protect the superuser account"
```

---

### Task 4: User endpoints authorize through ACLs

**Files:**
- Modify: `crates/microwave-api/src/helpers.rs:91-159`
- Modify: `crates/microwave-api/src/users.rs:1-60, 77-90, 202-240, 346-360, 438-451`
- Create: `crates/microwave-api/tests/server_admin_test.rs`

**Interfaces:**
- Consumes: `find_global_container` from Task 1; the container ACEs from Tasks 1-3.
- Produces: `pub async fn require_global_container_permission(state: &AppState, auth: &AuthContext, container: &str, permission: Permission) -> Result<(), ApiError>` and `pub fn is_configured_superuser(state: &AppState, username: &str) -> bool` in `helpers.rs`. `is_server_admin` is deleted.

- [ ] **Step 1: Write the failing endpoint matrix**

```rust
mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use microwave_store::Permission;
use tower::ServiceExt;

async fn send(app: &axum::Router, method: &str, path: &str, body: &[u8], user: &str) -> StatusCode {
    app.clone()
        .oneshot(support::signed_request(method, path, body, SignVersion::V1_3, user))
        .await
        .unwrap()
        .status()
}

const CREATE_BODY: &[u8] = br#"{"name":"dave","email":"dave@example.com","display_name":"Dave","password":"secret1"}"#;

#[tokio::test]
async fn server_admin_can_manage_users() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "bob", "bob@example.com", true);
    support::add_user(&state, "carol", "carol@example.com", false);
    let app = support::app(state);

    assert_eq!(send(&app, "GET", "/users", b"", "bob").await, StatusCode::OK);
    assert_eq!(send(&app, "POST", "/users", CREATE_BODY, "bob").await, StatusCode::CREATED);
    assert_eq!(send(&app, "GET", "/users/carol", b"", "bob").await, StatusCode::OK);
    assert_eq!(
        send(&app, "PUT", "/users/carol", br#"{"name":"carol","display_name":"Carol"}"#, "bob").await,
        StatusCode::OK
    );
    assert_eq!(send(&app, "DELETE", "/users/carol", b"", "bob").await, StatusCode::OK);
}

#[tokio::test]
async fn normal_user_cannot_manage_users() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "eve", "eve@example.com", false);
    support::add_user(&state, "carol", "carol@example.com", false);
    let app = support::app(state);

    assert_eq!(send(&app, "GET", "/users", b"", "eve").await, StatusCode::FORBIDDEN);
    assert_eq!(send(&app, "POST", "/users", CREATE_BODY, "eve").await, StatusCode::FORBIDDEN);
    assert_eq!(send(&app, "GET", "/users/carol", b"", "eve").await, StatusCode::FORBIDDEN);
    assert_eq!(
        send(&app, "PUT", "/users/carol", br#"{"name":"carol","display_name":"C"}"#, "eve").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(send(&app, "DELETE", "/users/carol", b"", "eve").await, StatusCode::FORBIDDEN);
}

/// Mirror what `ensure_superuser` does on a real boot: the bootstrap
/// superuser inherits no `server-admins` ACEs on its own account.
fn revoke_superuser_server_admin_aces(state: &microwave_api::AppState) {
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    let group = state.sqlite().ensure_global_group("server-admins").unwrap();
    state
        .sqlite()
        .revoke_group_acl(&pivotal.authz_id, &group.authz_id)
        .unwrap();
}

#[tokio::test]
async fn server_admin_cannot_manage_the_superuser() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "bob", "bob@example.com", true);
    revoke_superuser_server_admin_aces(&state);
    let app = support::app(state.clone());

    assert_eq!(send(&app, "GET", "/users/pivotal", b"", "bob").await, StatusCode::FORBIDDEN);
    assert_eq!(
        send(&app, "PUT", "/users/pivotal", br#"{"name":"pivotal","display_name":"P"}"#, "bob").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(send(&app, "DELETE", "/users/pivotal", b"", "bob").await, StatusCode::FORBIDDEN);

    // Defence in depth: even a drifted ACL that names server-admins must not
    // let a server-admin update the configured superuser.
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    let group = state.sqlite().ensure_global_group("server-admins").unwrap();
    state
        .sqlite()
        .set_acl(&pivotal.authz_id, Permission::Update, None, Some(&group.authz_id))
        .unwrap();
    assert_eq!(
        send(&app, "PUT", "/users/pivotal", br#"{"name":"pivotal","display_name":"P"}"#, "bob").await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn sensitive_profile_fields_are_superuser_only() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "bob", "bob@example.com", true);
    support::add_user(&state, "carol", "carol@example.com", false);
    let app = support::app(state);

    let body = br#"{"name":"carol","display_name":"Carol","recovery_authentication_enabled":true}"#;
    assert_eq!(send(&app, "PUT", "/users/carol", body, "bob").await, StatusCode::FORBIDDEN);
    assert_eq!(send(&app, "PUT", "/users/carol", body, "pivotal").await, StatusCode::OK);
}

#[tokio::test]
async fn organization_members_can_read_each_others_profiles() {
    let state = support::state_with_pivotal();
    let org = support::add_org(&state, "acme").await;
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    let bob = support::add_user(&state, "bob", "bob@example.com", false);
    support::associate(&state, &org, &alice);
    support::associate(&state, &org, &bob);
    let app = support::app(state);

    assert_eq!(send(&app, "GET", "/users/bob", b"", "alice").await, StatusCode::OK);
}

#[tokio::test]
async fn server_admin_can_grant_on_a_user_it_created() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "bob", "bob@example.com", true);
    let app = support::app(state);

    assert_eq!(send(&app, "POST", "/users", CREATE_BODY, "bob").await, StatusCode::CREATED);
    assert_eq!(
        send(&app, "PUT", "/users/dave/_acl/read", br#"{"actors":["dave"],"groups":[]}"#, "bob").await,
        StatusCode::OK
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p microwave-api --test server_admin_test`
Expected: FAIL — `server_admin_can_manage_users` returns `403`.

- [ ] **Step 3: Add the helpers and rewire the handlers**

In `helpers.rs`, add:

```rust
pub fn is_configured_superuser(state: &AppState, username: &str) -> bool {
    username.eq_ignore_ascii_case(&state.config.auth.superuser_name)
}

pub async fn require_global_container_permission(
    state: &AppState,
    auth: &AuthContext,
    container: &str,
    permission: Permission,
) -> Result<(), ApiError> {
    if is_superuser(auth) {
        return Ok(());
    }
    let container = state
        .store
        .find_global_container(container)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(ApiError::internal)?;
    require_permission(state, auth, &container.authz_id, permission).await
}
```

Delete `is_server_admin` and its comment block.

In `users.rs`:

- `list_users`: replace `require_superuser(&auth)?` with `require_global_container_permission(&state, &auth, "users", Permission::Read).await?`.
- `create_user`: same call with `Permission::Create`.
- `get_user`: replace `authorize_read(...)` with `require_permission(&state, &auth, &user.authz_id, Permission::Read).await?` and delete the now-unused `authorize_read`.
- `update_user`: keep the `current` lookup (404 first) and replace the
  existing `require_superuser(&auth)?` line that currently sits before body
  parsing with the block below, placed *after* the existing
  `let body = body.as_object().ok_or_else(...)?` conversion:

```rust
let sensitive = body.contains_key("external_authentication_uid")
    || body.contains_key("recovery_authentication_enabled");
if sensitive || is_configured_superuser(&state, &current.username) {
    require_superuser(&auth)?;
} else {
    require_permission(&state, &auth, &current.authz_id, Permission::Update).await?;
}
```

Upstream parses the body before authorizing, so the shape check stays ahead of
the ACL decision.

`body` is the `&Map<String, Value>`; move this after the existing `.ok_or_else(...)` conversion so the borrow exists.

- `delete_user`: after the existing `current` lookup:

```rust
if is_configured_superuser(&state, &user.username) {
    require_superuser(&auth)?;
} else {
    require_permission(&state, &auth, &user.authz_id, Permission::Delete).await?;
}
```

Adjust imports: `use crate::helpers::{base_url, is_configured_superuser, is_superuser, location_headers, require_global_container_permission, require_permission, require_superuser};` and add `Permission` to the `microwave_store` import.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-api --test server_admin_test`
Expected: PASS (6 tests).

Run: `cargo test -p microwave-api`
Expected: PASS; if a legacy test asserted the removed `is_server_admin` behaviour, update it to the new ACL outcome rather than reintroducing the helper.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src crates/microwave-api/tests/server_admin_test.rs
git commit -m "fix(api): authorize user management through the users container ACL"
```

---

### Task 5: Organization endpoints use the `organizations` container

**Files:**
- Modify: `crates/microwave-api/src/organizations.rs:23-50`
- Modify: `crates/microwave-api/tests/organization_auth_test.rs:85-128`

**Interfaces:**
- Consumes: `require_global_container_permission` from Task 4.
- Produces: `GET /organizations` → read on `organizations`; `POST /organizations` → create on `organizations`; named-organization routes unchanged.

- [ ] **Step 1: Flip the test expectation**

In `organization_routes_enforce_authorization`, replace the server-admin assertion:

```rust
let (status, _) = send(&app, "GET", "/organizations", b"", "bob").await;
assert_eq!(
    status,
    StatusCode::FORBIDDEN,
    "upstream grants server-admins nothing on the organizations container"
);
```

Keep the normal-user 403 and add a superuser 200 for `GET /organizations` and a superuser 201 for `POST /organizations` (the existing test already creates an org through `pivotal` elsewhere).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p microwave-api --test organization_auth_test organization_routes_enforce_authorization`
Expected: FAIL — server-admin still gets 200.

- [ ] **Step 3: Rewire the two handlers**

In `list_orgs`, replace the `is_server_admin` block with:

```rust
require_global_container_permission(&state, &auth, "organizations", Permission::Read).await?;
```

In `create_org`, replace `require_superuser(&auth)?` with:

```rust
require_global_container_permission(&state, &auth, "organizations", Permission::Create).await?;
```

Update the `helpers` import list (drop `is_server_admin`, add `require_global_container_permission`).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p microwave-api --test organization_auth_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/organizations.rs crates/microwave-api/tests/organization_auth_test.rs
git commit -m "fix(api): authorize organizations through the organizations container ACL"
```

---

### Task 6: Carry global containers through snapshots and importers

**Files:**
- Modify: `crates/microwave-store/src/migration.rs:44, 244-250, 761-772, 1528-1545`
- Modify: `crates/microwave-migrate/src/lib.rs:328-334, 432-436, 479-537`
- Modify: `crates/microwave-migrate/src/goiardi.rs:1137-1180`
- Modify: `crates/microwave-migrate/src/knife_backup.rs:2360-2400`
- Test: `crates/microwave-store/tests/compression_test.rs`
- Test: `crates/microwave-migrate/tests/import_preflight_test.rs`
- Test: `crates/microwave-migrate/tests/goiardi_test.rs`

**Interfaces:**
- Consumes: `ensure_global_container`, `list_global_containers`, `GlobalContainer` from Task 1.
- Produces: `GlobalContainerRow { id, authz_id, name, created_at, updated_at }` and `StoreSnapshot::global_containers`; `reconcile_global_containers(data: &mut StoreSnapshot, existing: &[(String, String)])` in `microwave-migrate`.

- [ ] **Step 1: Write the failing tests**

`crates/microwave-store/tests/compression_test.rs` — extend the existing round-trip test (`source.export_snapshot()` → fresh store → `import_snapshot`):

```rust
let container = source.ensure_global_container("users").unwrap();
let exported = source.export_snapshot().unwrap();

assert!(
    exported.global_containers.iter().any(|row| row.name == "users"),
    "{:?}",
    exported.global_containers
);

let target_dir = TempDir::new().unwrap();
let target = SqliteStore::open(&database(&target_dir)).unwrap();
target.import_snapshot(&exported).unwrap();
assert_eq!(
    target.find_global_container("users").unwrap().unwrap().authz_id,
    container.authz_id
);
```

`crates/microwave-migrate/tests/import_preflight_test.rs` — reuse the bootstrap shape from `accepts_a_target_with_only_the_bootstrap_superuser` (add `GlobalContainerRow` to the `use microwave_store::{...}` list):

```rust
#[test]
fn reuses_the_bootstrap_global_containers() {
    let target = tempfile::tempdir().unwrap();
    let database = target.path().join("microwave.db");
    let bootstrap = microwave_store::SqliteStore::open(&database).unwrap();
    bootstrap
        .create_user("pivotal", "pivotal@example.com", "bcrypt", "h", "s", true)
        .unwrap();
    let bootstrap_users = bootstrap
        .find_global_container("users")
        .unwrap()
        .expect("create_user bootstraps the users container");
    drop(bootstrap);

    let mut snapshot = snapshot(Some("goiardi"));
    snapshot.data.global_containers = vec![GlobalContainerRow {
        id: "global-users".to_string(),
        authz_id: "authz-global-users".to_string(),
        name: "users".to_string(),
        ..GlobalContainerRow::default()
    }];
    snapshot.data.authz_entities.push(AuthzEntityRow {
        authz_id: "authz-global-users".to_string(),
        entity_type: "container".to_string(),
        ..AuthzEntityRow::default()
    });
    let input = write(target.path(), &snapshot);

    import(target.path(), &input).unwrap();

    let store = microwave_store::SqliteStore::open(&database).unwrap();
    let containers: Vec<_> = store
        .list_global_containers()
        .unwrap()
        .into_iter()
        .filter(|container| container.name == "users")
        .collect();
    assert_eq!(containers.len(), 1, "{containers:?}");
    assert_eq!(containers[0].authz_id, bootstrap_users.authz_id);
}
```

`crates/microwave-migrate/tests/goiardi_test.rs` — extend
`crawls_objects_raw_and_skips_default_environment` after the environment
assertions:

```rust
let names: Vec<&str> = snapshot.data.global_containers.iter().map(|row| row.name.as_str()).collect();
assert!(names.contains(&"users"), "{names:?}");
assert!(names.contains(&"organizations"), "{names:?}");
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p microwave-store --test compression_test && cargo test -p microwave-migrate`
Expected: FAIL — `global_containers` field does not exist.

- [ ] **Step 3: Add the snapshot row, collection and insert**

In `migration.rs`, next to `GlobalGroupRow`, and add the field next to `global_groups` in `StoreSnapshot`, the `collect(...)` query (`FROM global_containers ORDER BY id`), and the `INSERT INTO global_containers ... ON CONFLICT(id) DO UPDATE` loop after the `global_groups` loop.

- [ ] **Step 4: Reconcile containers in the importer**

Add alongside `existing_global_groups` / `reconcile_global_groups`:

```rust
fn existing_global_containers(containers: Vec<microwave_store::GlobalContainer>) -> Vec<(String, String)> {
    containers.into_iter().map(|container| (container.name, container.authz_id)).collect()
}
```

`reconcile_global_containers` is a copy of `reconcile_global_groups` that retains by `global_containers` name, drops the snapshot's entity rows for replaced authz ids, and rewrites `target_authz_id` in `authz_acl_actor` / `authz_acl_group`. Call it in both places `reconcile_global_groups` is called, immediately after it, using `store.list_global_containers()`.

- [ ] **Step 5: Emit the containers from the goiardi importer and ensure them in the knife importer**

Add `push_global_container(data, name, creator_authz_id, generated_at)` in `goiardi.rs` (model it on `push_global_group`, but with `entity_type = "container"`): for `users`, grant `::server-admins` create/read/update/delete and the creator all five; for `organizations`, grant only the creator all five. Call it for both names where the standard graph is built near line 1137.

In `knife_backup.rs`, where the standard graph is rebuilt (`resolve_global_group("server-admins")`, around line 2380), call `store.ensure_global_container("users").await?` and `store.ensure_global_container("organizations").await?`.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p microwave-store --test compression_test && cargo test -p microwave-migrate`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/microwave-store/src/migration.rs crates/microwave-migrate
git commit -m "feat(migrate): carry global containers through snapshots and importers"
```

---

### Task 7: Documentation and full verification

**Files:**
- Modify: `docs/deployment.md` (server-admin section around line 2520, import notes around 1940 and 2178)
- Modify: `docs/http-parity.md:40-45`
- Modify: `TODO.md:59, 273`
- Modify: `docs/superpowers/specs/2026-10-24-microwave-server-design.md:266`
- Check: `docs/claims.toml:661-666`

- [ ] **Step 1: Update the prose**

- `docs/deployment.md`: state that `grant-server-admin-permissions` writes the global `server-admins` group (the `users.admin` flag mirrors it), that members may list/read/create/update/delete user accounts but not superuser accounts, and that server-admins may not list or create organizations. Keep the literal needle `` `grant-server-admin-permissions` `` intact for `readme-cli-server-admins`.
- `docs/http-parity.md`: add the global `users`/`organizations` containers to the authorization bullet.
- `TODO.md:273`: append "including the global `users`/`organizations` containers and the server-admin user-management grants".
- The 2026-10-24 design: after the superuser bullets, add one line that `server-admins` authorizes user management through the `users` container.
- If any `claims.toml` needle no longer matches, update the needle to the new wording rather than the text.

- [ ] **Step 2: Verify the claims**

Run: `python3 scripts/check-claims.py`
Expected: PASS (no missing needles or stale claims).

- [ ] **Step 3: Run the full Rust gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all pass. If Docker is available, also run `scripts/run-postgres-tests.sh` and `MICROWAVE_REQUIRE_POSTGRES=1 cargo test -p microwave-store --test dual_backend`.

- [ ] **Step 4: Commit**

```bash
git add docs TODO.md
git commit -m "docs(server-admin): document the global container authorization model"
```

---

## Self-review notes

- Spec coverage: data model → Task 1; inheritance → Task 2; bootstrap and superuser protection → Task 3; users endpoints and sensitive fields → Task 4; organizations and the P0 reversal → Task 5; snapshots/importers → Task 6; docs → Task 7.
- `is_server_admin` has no remaining caller after Task 5; `grep -rn is_server_admin crates` must be empty before Task 5's commit.
- `Permission` is already exported from `microwave_store`; `set_acl` and `grant_actor_all` signatures used in tests exist on `AuthzRepository`.
- The `admin` flag is untouched; every existing flag/membership sync test keeps passing.
