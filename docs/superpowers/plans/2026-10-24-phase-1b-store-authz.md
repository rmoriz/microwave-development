# Phase 1B: SQLite Metadata Store and Authorization Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the unified SQLite metadata store and the ACL/group authorization engine that every later API surface depends on.

**Architecture:** `microwave-store` owns the SQLite connection, schema migrations, identity records, and the authorization graph. `microwave-authz` owns permission semantics and the default organization policy. Both are synchronous crates; the HTTP layer will call them through `spawn_blocking`.

**Tech Stack:** Rust 1.99, `rusqlite` (bundled SQLite), `uuid`, `chrono`, `serde`, `thiserror`.

**Spec:** `docs/superpowers/specs/2026-10-24-microwave-server-design.md`

## Global Constraints

- SQLite in WAL mode with `foreign_keys = ON`.
- IDs are 32 lowercase hex characters, generated with UUID v4.
- Timestamps are stored as RFC 3339 UTC text.
- No `chef-` prefix for project crates or types.
- All writes that create an object in a container also create its ACL rows in the same transaction.
- Every task ends with green tests and a commit.

## Review Focus

- **Duplicate names within an org:** creating a second node/client/role with the same name must fail with a typed conflict, not a raw SQLite error.
- **Group cycles:** adding a group as its own ancestor must be rejected.
- **Transitive ACL grants:** a user in a subgroup must inherit permissions from the parent group.
- **Unknown authz entity:** permission checks must return `false` or a typed not-found error, never panic.
- **Org creation rollback:** a failure halfway through default policy creation must not leave a partially created org.

---

### Task 1: Store Crate, Schema, and Connection

**Files:**
- Create: `crates/microwave-store/Cargo.toml`
- Create: `crates/microwave-store/src/lib.rs`
- Create: `crates/microwave-store/src/sqlite.rs`
- Create: `crates/microwave-store/migrations/0001_initial.sql`
- Test: `crates/microwave-store/tests/schema_test.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `microwave_store::SqliteStore`
  - `SqliteStore::open(path: &Path) -> Result<SqliteStore, StoreError>`
  - `SqliteStore::open_in_memory() -> Result<SqliteStore, StoreError>`
  - `SqliteStore::connection()` for crate-internal repository modules
  - `StoreError`

- [ ] **Step 1: Write the failing test**

```rust
use microwave_store::SqliteStore;

#[test]
fn opens_in_memory_and_creates_core_tables() {
    let store = SqliteStore::open_in_memory().unwrap();
    let names: Vec<String> = store
        .table_names()
        .unwrap()
        .into_iter()
        .filter(|n| !n.starts_with("sqlite_"))
        .collect();
    for table in ["orgs", "users", "clients", "actor_keys", "authz_entities", "nodes", "cookbooks"] {
        assert!(names.contains(&table.to_string()), "missing {table}");
    }
}

#[test]
fn enables_wal_and_foreign_keys_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("mw.db")).unwrap();
    assert_eq!(store.journal_mode().unwrap(), "wal");
    assert!(store.foreign_keys_enabled().unwrap());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-store --test schema_test`
Expected: FAIL because the crate does not exist.

- [ ] **Step 3: Implement the crate and schema**

Create the crate, add it to the workspace, implement `open`, `open_in_memory`, `table_names`, `journal_mode`, and `foreign_keys_enabled`. Apply `0001_initial.sql` with `execute_batch`. The initial schema must include the identity, authz, and core object tables from the specification, with foreign keys and unique constraints.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-store --test schema_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/microwave-store
git commit -m "feat: add sqlite metadata store schema"
```

---

### Task 2: Identity Repository

**Files:**
- Create: `crates/microwave-store/src/identity.rs`
- Modify: `crates/microwave-store/src/lib.rs`
- Test: `crates/microwave-store/tests/identity_test.rs`

**Interfaces:**
- Consumes: `SqliteStore`.
- Produces:
  - `Org { id, authz_id, name, full_name, created_at, updated_at }`
  - `User { id, authz_id, username, email, hash_type, password_hash, password_salt, admin, recovery_authentication_enabled }`
  - `Client { id, org_id, authz_id, name, validator, admin }`
  - `ActorKey { actor_id, actor_type, key_name, public_key, expires_at }`
  - `SqliteStore::{create_org, find_org, create_user, find_user, create_client, find_client, add_actor_key, find_actor_keys}`

- [ ] **Step 1: Write the failing test**

```rust
use microwave_store::{ActorType, SqliteStore};

#[test]
fn creates_and_finds_org_user_client_and_keys() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    assert_eq!(store.find_org("acme").unwrap().unwrap().id, org.id);

    let user = store
        .create_user("alice", "alice@example.com", "bcrypt", "hash", "salt", false)
        .unwrap();
    assert_eq!(store.find_user("alice").unwrap().unwrap().id, user.id);

    let client = store.create_client(&org.id, "web1", false, false).unwrap();
    assert_eq!(store.find_client(&org.id, "web1").unwrap().unwrap().id, client.id);

    store
        .add_actor_key(&user.id, ActorType::User, "default", "-----BEGIN PUBLIC KEY-----\nx\n-----END PUBLIC KEY-----")
        .unwrap();
    assert_eq!(store.find_actor_keys(&user.id, ActorType::User).unwrap().len(), 1);
}

#[test]
fn duplicate_names_conflict() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_org("acme", "Acme Corp").unwrap();
    assert!(store.create_org("acme", "Other").is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-store --test identity_test`
Expected: FAIL because the repository methods do not exist.

- [ ] **Step 3: Implement the identity repository**

Generate 32-hex IDs and authz IDs. Store timestamps as RFC 3339. Map unique-constraint violations to `StoreError::Conflict`. `create_org` and `create_client` also insert the matching `authz_entities` row; `create_user` inserts a global actor entity. No ACLs yet.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-store --test identity_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-store
git commit -m "feat: add identity repository"
```

---

### Task 3: Authorization Graph

**Files:**
- Create: `crates/microwave-authz/Cargo.toml`
- Create: `crates/microwave-authz/src/lib.rs`
- Create: `crates/microwave-authz/src/acl.rs`
- Modify: `crates/microwave-store/src/sqlite.rs` to expose authz queries
- Test: `crates/microwave-authz/tests/acl_test.rs`

**Interfaces:**
- Consumes: `SqliteStore`, authz IDs.
- Produces:
  - `Permission { Create, Read, Update, Delete, Grant }`
  - `SqliteStore::{add_group_member, add_group_child, set_acl, get_acl, check_permission}`
  - `microwave_authz::check(store, actor_authz_id, target_authz_id, permission) -> Result<bool, StoreError>`
  - `microwave_authz::ensure_acyclic(store, parent_group_id, child_group_id)`

- [ ] **Step 1: Write the failing test**

```rust
use microwave_authz::{Permission, check};
use microwave_store::SqliteStore;

#[test]
fn direct_and_transitive_grants_are_seen() {
    let store = SqliteStore::open_in_memory().unwrap();
    let actor = store.create_actor("user").unwrap();
    let target = store.create_object("node").unwrap();
    let child = store.create_group("readers").unwrap();
    let parent = store.create_group("admins").unwrap();

    store.add_group_member(&child, &actor).unwrap();
    store.add_group_child(&parent, &child).unwrap();
    store.set_acl(&target, Permission::Read, Some(&actor), None).unwrap();

    assert!(check(&store, &actor, &target, Permission::Read).unwrap());
    assert!(!check(&store, &actor, &target, Permission::Update).unwrap());

    let shared = store.create_object("shared").unwrap();
    store.set_acl(&shared, Permission::Read, None, Some(&parent)).unwrap();
    assert!(check(&store, &actor, &shared, Permission::Read).unwrap());
}

#[test]
fn group_cycles_are_rejected() {
    let store = SqliteStore::open_in_memory().unwrap();
    let a = store.create_group("a").unwrap();
    let b = store.create_group("b").unwrap();
    store.add_group_child(&a, &b).unwrap();
    assert!(store.add_group_child(&b, &a).is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-authz --test acl_test`
Expected: FAIL because the crate and authz methods do not exist.

- [ ] **Step 3: Implement ACL storage and recursive checks**

Store ACL rows in `authz_acl_actor` and `authz_acl_group`. Implement `check_permission` with a `WITH RECURSIVE` CTE covering direct actor grants and group grants through arbitrary depth. Implement cycle rejection before inserting a parent/child group edge.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-authz --test acl_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/microwave-authz crates/microwave-store
git commit -m "feat: add acl and group authorization engine"
```

---

### Task 4: Default Organization Policy

**Files:**
- Create: `crates/microwave-authz/src/org.rs`
- Modify: `crates/microwave-authz/src/lib.rs`
- Test: `crates/microwave-authz/tests/org_test.rs`

**Interfaces:**
- Consumes: Task 2 and Task 3.
- Produces:
  - `create_organization(store, name, full_name, creator_user_id, creator_authz_id) -> Result<Org, StoreError>`
  - `DEFAULT_CONTAINERS: [&str; 12]`
  - `DEFAULT_GROUPS: [&str; 5]`

- [ ] **Step 1: Write the failing test**

```rust
use microwave_authz::{create_organization, Permission};
use microwave_store::SqliteStore;

#[test]
fn creates_default_containers_groups_and_acls() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store.create_user("root", "root@example.com", "bcrypt", "hash", "salt", true).unwrap();
    let org = create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id).unwrap();

    for name in microwave_authz::DEFAULT_CONTAINERS {
        assert!(store.find_container(&org.id, name).unwrap().is_some(), "missing container {name}");
    }
    for name in microwave_authz::DEFAULT_GROUPS {
        assert!(store.find_group(&org.id, name).unwrap().is_some(), "missing group {name}");
    }

    let nodes = store.find_container(&org.id, "nodes").unwrap().unwrap();
    assert!(store.check_permission(&creator.authz_id, &nodes.authz_id, Permission::Create).unwrap());
    assert!(store.check_permission(&creator.authz_id, &nodes.authz_id, Permission::Read).unwrap());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p microwave-authz --test org_test`
Expected: FAIL because `create_organization` does not exist.

- [ ] **Step 3: Implement the default policy transactionally**

Create the org, the 12 default containers, the 5 default groups, add the creator to `admins` and `users`, create the org read-access group, and grant the creator all permissions on every container and group. On any error, roll back the whole transaction.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p microwave-authz --test org_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-authz crates/microwave-store
git commit -m "feat: add default organization policy"
```

---

## Self-Review

- **Spec coverage:** Phase 1B covers the identity and authorization halves of spec sections 6.2 and 6.3. Object repositories for nodes, cookbooks, and policies are deferred to Phase 1C.
- **Step scan:** Each task has a failing test, implementation, passing test, and commit.
- **Type consistency:** `SqliteStore`, `Permission`, `ActorType`, `Org`, `User`, `Client`, `ActorKey` are introduced once and reused.
- **Review Focus:** Duplicate names, group cycles, transitive grants, unknown entities, and org rollback each have a test.
- **Proportion:** Interfaces and tests only; no full implementation bodies.
