# Per-Organization Object Quotas Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in, hard, per-organization ceilings for nodes, cookbook versions, environments, roles and policy revisions; a creation over the ceiling answers `403`, everything else keeps working.

**Architecture:** A `[quotas]` config section is validated and resolved to organization ids at startup and on reload, then installed into a store-held `QuotaPolicy`. Each `create_*` that carries a quota checks the limit inside its existing transaction, takes a per-(org, kind) PostgreSQL advisory lock, short-circuits when the target row already exists, counts, and inserts or answers `StoreError::Quota`. `StoreError::Quota` maps to `403` once, so HTTP and MCP share the behaviour and the message.

**Tech Stack:** Rust, axum, rusqlite, sqlx/PostgreSQL, the existing config/store/metrics stacks. No new external crate; `microwave-store`, `microwave-store-postgres` and `microwave-authz` gain a `microwave-config` dependency for `OrgQuotas`.

**Spec:** `docs/superpowers/specs/2026-11-08-org-quotas-design.md`

## Global Constraints

- Five kinds only: node, cookbook version, environment, role, policy revision.
- Every field is `Option<u64>`; absent means unlimited, `0` is a valid ceiling.
- The auto-created `_default` environment is exempt from `max_environments`.
- Limits are hard: a creation at the ceiling fails with `403`, `{"error":["<kind> limit of <limit> reached"]}`; updates and deletes stay allowed.
- An operation that creates no row keeps its normal `409`/`200`, never `403`.
- Re-uploading an existing cookbook version, policy revision, node, environment or role is short-circuited before the count.
- The policyfile association route (`associate_policy_revision`) carries the same policy-revision guard.
- Counting unit: every cookbook version and every policy revision, not names. Cookbook artifact versions share the cookbook-version ceiling.
- Quotas are independent of `object_store.require_org_bucket` and of `limits.enabled`.
- `quotas.enabled = false` installs an empty policy; entries stay in the file.
- A rejection records `Metrics::record_limit("quota:<kind>", <org id>, "rejected")` and logs `WARN` with org id, kind and limit.
- An unknown organization in `[quotas.organizations]` is a `check-config` problem and a reload warning.
- `microwave-migrate` imports bypass quotas (they build a store without a policy).
- Every task ends with `cargo fmt --all`, the focused tests, and one commit.
- Kind labels: `node`, `cookbook version`, `environment`, `role`, `policy revision`.

## Review Focus

1. `org-create` bootstraps a `_default` environment; `max_environments = 0` must not block it and `1` must allow exactly zero custom environments. Tested in Task 3 and Task 4.
2. A `PUT` of an existing node, environment, role, cookbook version or policy revision at the ceiling must answer `200`/`409`, never `403`. Tested in Task 3 and Task 4.
3. A policyfile association that would create a revision must fail with the quota error, while associating an already existing revision succeeds. Tested in Task 3 and Task 4.
4. More concurrent creators than free slots must never push the final count over the limit, on both backends. Tested in Task 2.
5. An entry for a name that does not exist fails `check-config`, only warns on reload, and `org-create` applies a pre-provisioned entry immediately. Tested in Task 4 and Task 5.

---

### Task 1: Configuration surface

**Files:**
- Modify: `crates/microwave-config/src/config.rs` (`Config.quotas`, `QuotasConfig`, `OrgQuotas`, `validate`, `apply_env`, env doc list)
- Modify: `crates/microwave-config/src/lib.rs` (re-exports)
- Modify: `packaging/microwave.toml.example` (new commented block after `[limits]`)
- Test: `crates/microwave-config/tests/config_test.rs`

**Interfaces:**
- Consumes: `valid_org_name`, `parse_bool`, `serde_json::from_str`, `ConfigError::InvalidValue`.
- Produces:
  - `Config.quotas: QuotasConfig`
  - `pub struct QuotasConfig { pub enabled: bool, pub organizations: std::collections::BTreeMap<String, OrgQuotas> }`
  - `pub struct OrgQuotas { pub max_nodes: Option<u64>, pub max_cookbook_versions: Option<u64>, pub max_environments: Option<u64>, pub max_roles: Option<u64>, pub max_policy_revisions: Option<u64> }`
  - `QuotasConfig::org(&self, name: &str) -> Option<OrgQuotas>` — returns `None` when `enabled` is false or the name has no entry.
  - `QuotasConfig::validate(&self) -> Result<(), (String, String)>`

- [ ] **Step 1: Write the failing tests**

Append to `crates/microwave-config/tests/config_test.rs`:

```rust
#[test]
fn quotas_default_to_enabled_and_empty() {
    let c = Config::default();
    assert!(c.quotas.enabled);
    assert!(c.quotas.organizations.is_empty());
    assert_eq!(c.quotas.org("acme"), None);
}

#[test]
fn quotas_parse_every_field_and_accept_zero() {
    let c = Config::from_toml_str(
        "[quotas]\nenabled = true\n[quotas.organizations.acme]\nmax_nodes = 0\nmax_cookbook_versions = 5000\nmax_environments = 20\nmax_roles = 100\nmax_policy_revisions = 200\n",
    )
    .unwrap();
    let acme = c.quotas.org("acme").expect("active entry");
    assert_eq!(acme.max_nodes, Some(0));
    assert_eq!(acme.max_cookbook_versions, Some(5000));
    assert_eq!(acme.max_environments, Some(20));
    assert_eq!(acme.max_roles, Some(100));
    assert_eq!(acme.max_policy_revisions, Some(200));

    let disabled = Config::from_toml_str(
        "[quotas]\nenabled = false\n[quotas.organizations.acme]\nmax_nodes = 5\n",
    )
    .unwrap();
    assert_eq!(disabled.quotas.org("acme"), None);
}

#[test]
fn quotas_reject_an_invalid_organization_name() {
    let error = Config::from_toml_str("[quotas.organizations.BadOrg]\nmax_nodes = 5\n")
        .expect_err("invalid name");
    assert!(matches!(
        error,
        microwave_config::ConfigError::InvalidValue { ref key, ref value }
            if key == "quotas.organizations.BadOrg" && value == "BadOrg"
    ));
}

#[test]
fn quotas_env_overrides() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_QUOTAS__ENABLED", "false");
    std::env::set_var("MW_QUOTAS__ORGANIZATIONS", r#"{"acme":{"max_nodes":7}}"#);
    let mut config = Config::default();
    config.apply_env().unwrap();
    std::env::remove_var("MW_QUOTAS__ENABLED");
    std::env::remove_var("MW_QUOTAS__ORGANIZATIONS");
    assert!(!config.quotas.enabled);
    assert_eq!(config.quotas.organizations["acme"].max_nodes, Some(7));
}
```

Add to the existing `shipped_example_config_parses_with_safe_defaults` test:

```rust
    assert!(c.quotas.enabled);
    assert!(c.quotas.organizations.is_empty());
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-config quotas_`
Expected: FAIL with `no field quotas` / `no method org`.

- [ ] **Step 3: Add the types, defaults, validation and environment overlay**

Add `pub quotas: QuotasConfig` to `Config`. Add the two structs near `LimitsConfig`; `OrgQuotas` derives `Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq` with `#[serde(default)]` so every missing field is unset. `QuotasConfig` derives `Debug, Clone, Serialize, Deserialize, PartialEq, Eq` with `#[serde(default)]` and hand-writes `Default` (`enabled: true`, empty map).

`QuotasConfig::validate` iterates the map and returns `(format!("quotas.organizations.{name}"), name.clone())` for `!valid_org_name(name)`.

Call `self.quotas.validate().map_err(|(key, value)| ConfigError::InvalidValue { key, value })?;` next to the `self.limits.validate()` call in `Config::validate`.

Add to `apply_env`, next to the `MW_LIMITS__*` block:

```rust
        if let Ok(value) = std::env::var("MW_QUOTAS__ENABLED") {
            self.quotas.enabled = parse_bool("MW_QUOTAS__ENABLED", &value)?;
        }
        if let Ok(value) = std::env::var("MW_QUOTAS__ORGANIZATIONS") {
            self.quotas.organizations =
                serde_json::from_str(&value).map_err(|error| ConfigError::InvalidValue {
                    key: "MW_QUOTAS__ORGANIZATIONS".into(),
                    value: error.to_string(),
                })?;
        }
```

Name both in the `apply_env` doc comment list.

Re-export `QuotasConfig` and `OrgQuotas` from `crates/microwave-config/src/lib.rs`.

- [ ] **Step 4: Add the example block**

After the `[limits]` block in `packaging/microwave.toml.example`:

```toml
# Per-organization object quotas. Absent means unlimited; 0 is a valid ceiling.
# The auto-created `_default` environment never counts.
# [quotas]
# enabled = true
# [quotas.organizations."acme"]
# max_nodes = 500
# max_cookbook_versions = 5000
# max_environments = 20
# max_roles = 100
# max_policy_revisions = 200
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p microwave-config`
Expected: PASS, including `shipped_example_config_parses_with_safe_defaults`.

- [ ] **Step 6: Commit**

```bash
git add crates/microwave-config packaging/microwave.toml.example
git commit -m "feat(config): add opt-in per-organization quotas"
```

---

### Task 2: Store quota core and node guard

**Files:**
- Create: `crates/microwave-store/src/quotas.rs`
- Create: `crates/microwave-store-postgres/src/quotas.rs`
- Modify: `crates/microwave-store/Cargo.toml` (add `microwave-config.workspace = true`, `tracing.workspace = true`)
- Modify: `crates/microwave-store-postgres/Cargo.toml` (add `microwave-config.workspace = true`, `tracing.workspace = true`)
- Modify: `crates/microwave-store/src/lib.rs` (module, re-exports)
- Modify: `crates/microwave-store/src/sqlite.rs` (`StoreError::Quota`, `pub(crate) quotas` field, `quota_limit`, `quota_metrics`)
- Modify: `crates/microwave-store/src/backend.rs` (`QuotaRepository`, `MetadataStore`)
- Modify: `crates/microwave-store/src/objects.rs` (`create_node` guard)
- Modify: `crates/microwave-store-postgres/src/store.rs` (`pub(crate) quotas` field, `quota_limit`, `quota_metrics`)
- Modify: `crates/microwave-store-postgres/src/lib.rs` (module)
- Modify: `crates/microwave-store-postgres/src/objects.rs` (`create_node` guard)
- Test: `crates/microwave-store/tests/dual_backend.rs`

**Interfaces:**
- Consumes: `microwave_config::OrgQuotas`, `microwave_metrics::Metrics`, `encode_...` none.
- Produces:
  - `pub enum QuotaKind { Node, CookbookVersion, Environment, Role, PolicyRevision }` with `Display`/`label() -> &'static str` and `fn limit(self, quotas: &OrgQuotas) -> Option<u64>`
  - `pub enum QuotaTarget<'a>` with variants `Node { name }`, `CookbookVersion { name, version }`, `Environment { name }`, `Role { name }`, `PolicyRevision { name, revision_id }`, `CookbookArtifactVersion { name, identifier }`, `fn kind(self) -> QuotaKind` and `fn is_exempt(&self) -> bool`
  - `pub struct QuotaPolicy` with `Default`, `limit(&self, org_id: &str, kind: QuotaKind) -> Option<u64>`, `set_org(&mut self, org_id: &str, quotas: Option<OrgQuotas>)`, `replace(&mut self, other: QuotaPolicy)`
  - `StoreError::Quota { kind: QuotaKind, limit: u64 }`, `Display` = `"<kind> limit of <limit> reached"`
  - `pub trait QuotaRepository: Send + Sync { fn set_quotas(&self, policy: QuotaPolicy); fn set_org_quotas(&self, org_id: &str, quotas: Option<OrgQuotas>); fn quota_limit(&self, org_id: &str, kind: QuotaKind) -> Option<u64>; fn set_metrics(&self, metrics: Arc<Metrics>); }`. Both stores also get `pub(crate)` inherent `quota_limit`/`set_metrics` helpers of the same name; the inherent method wins on concrete types, so the trait impls delegate with `SqliteStore::quota_limit(self, org_id, kind)` / `PostgresStore::quota_limit(self, org_id, kind)` and `SqliteStore::set_metrics(self, metrics)` / `PostgresStore::set_metrics(self, metrics)`.
  - `SqliteStore::enforce_quota(&self, tx: &Transaction<'_>, org_id: &str, target: QuotaTarget<'_>) -> Result<(), StoreError>`
  - `PostgresStore::enforce_quota(&self, tx: &mut Transaction<'_, Postgres>, org_id: &str, target: QuotaTarget<'_>) -> Result<(), StoreError>`

- [ ] **Step 1: Write the failing node tests**

Add a `quotas` suite to `mod suite` in `crates/microwave-store/tests/dual_backend.rs`. Use the existing `fixture`, `unique`, `checksum`, `run_concurrently` helpers, extend the `use microwave_store::{..}` import with `QuotaKind` and `QuotaPolicy`, and import `microwave_config::OrgQuotas` plus `microwave_metrics::Metrics` at the file top.

```rust
    pub async fn quota_nodes(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let metrics = Arc::new(Metrics::new());
        store.set_metrics(metrics.clone());
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;
        let creator = fixture.creator_authz_id.as_str();
        let mut policy = QuotaPolicy::default();
        policy.set_org(&org.id, Some(OrgQuotas { max_nodes: Some(1), ..Default::default() }));
        store.set_quotas(policy);

        store.create_node(&org.id, NewNode { name: "web1", body: r#"{"name":"web1","run_list":[]}"#, environment: "_default", policy_name: None, policy_group: None, creator_authz_id: creator }).await.expect("first node");

        let error = store.create_node(&org.id, NewNode { name: "web2", body: r#"{"name":"web2","run_list":[]}"#, environment: "_default", policy_name: None, policy_group: None, creator_authz_id: creator }).await.expect_err("second node over the limit");
        assert_eq!(error.to_string(), "node limit of 1 reached");
        assert!(matches!(error, StoreError::Quota { kind: QuotaKind::Node, limit: 1 }));
        let series = metrics.limit_series();
        assert!(series.iter().any(|series| {
            series.scope == "quota:node" && series.outcome == "rejected" && series.org == org.id
        }));

        // Re-upload of the existing row at the ceiling stays a 409.
        let conflict = store.create_node(&org.id, NewNode { name: "web1", body: r#"{"name":"web1","run_list":[]}"#, environment: "_default", policy_name: None, policy_group: None, creator_authz_id: creator }).await.expect_err("duplicate");
        assert!(matches!(conflict, StoreError::Conflict(_)), "{conflict}");

        store.delete_node(&org.id, "web1").await.expect("delete frees the slot");
        store.create_node(&org.id, NewNode { name: "web2", body: r#"{"name":"web2","run_list":[]}"#, environment: "_default", policy_name: None, policy_group: None, creator_authz_id: creator }).await.expect("slot after delete");

        // A different organization without an entry is unlimited.
        let other = store.create_org(&unique("quota-other"), "Other").await.expect("other org");
        store.create_node(&other.id, NewNode { name: "web1", body: r#"{"name":"web1","run_list":[]}"#, environment: "_default", policy_name: None, policy_group: None, creator_authz_id: creator }).await.expect("unlimited org");

        cleanup_org(store.as_ref(), &org.id).await;
        cleanup_org(store.as_ref(), &other.id).await;
        Vec::new()
    }

    pub async fn quota_nodes_concurrent(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;
        let mut policy = QuotaPolicy::default();
        policy.set_org(&org.id, Some(OrgQuotas { max_nodes: Some(2), ..Default::default() }));
        store.set_quotas(policy);

        let creator = fixture.creator_authz_id.clone();
        let results = run_concurrently(4, {
            let store = Arc::clone(store);
            let org_id = org.id.clone();
            let creator = creator.clone();
            move |index| {
                let store = Arc::clone(&store);
                let org_id = org_id.clone();
                let creator = creator.clone();
                async move {
                    let name = format!("web{index}");
                    let body = format!(r#"{{"name":"{name}","run_list":[]}}"#);
                    store.create_node(&org_id, NewNode { name: &name, body: &body, environment: "_default", policy_name: None, policy_group: None, creator_authz_id: &creator }).await.is_ok()
                }
            }
        })
        .await;
        assert_eq!(results.iter().filter(|ok| **ok).count(), 2, "{results:?}");
        // `count_nodes` is global; parallel matrix tests share a PostgreSQL
        // database, so count this organization's nodes instead.
        assert_eq!(store.list_nodes(&org.id).await.expect("list").len(), 2);

        cleanup_org(store.as_ref(), &org.id).await;
        Vec::new()
    }
```

Add the two names to both `matrix!` lists.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-store --test dual_backend quota`
Expected: FAIL with unresolved `QuotaPolicy`/`QuotaKind`/`set_quotas`.

- [ ] **Step 3: Add the store core**

Create `crates/microwave-store/src/quotas.rs` with the types from the Interfaces block. Declare `pub mod quotas;` in `crates/microwave-store/src/lib.rs` and re-export `QuotaKind`, `QuotaPolicy`, `QuotaTarget`, `count_sql` and `reject_if_full`. In `crates/microwave-store/src/sqlite.rs`, `use crate::quotas::QuotaKind;`. Plus:

```rust
impl fmt::Display for QuotaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl QuotaKind {
    pub fn label(self) -> &'static str { /* node, cookbook version, environment, role, policy revision */ }
    pub fn limit(self, quotas: &OrgQuotas) -> Option<u64> { /* the matching field */ }
}
```

`QuotaPolicy` holds `BTreeMap<String, OrgQuotas>`; `limit` reads `self.quotas.get(org_id).and_then(|quotas| quotas.limit(kind))`. `set_org` inserts or removes; `replace` swaps the whole map.

Add `StoreError::Quota` to `crates/microwave-store/src/sqlite.rs`:

```rust
    #[error("{kind} limit of {limit} reached")]
    Quota { kind: QuotaKind, limit: u64 },
```

Add `QuotaRepository` to the supertrait list of `MetadataStore` and to the `where` clause of its blanket impl in `crates/microwave-store/src/backend.rs` (it is a plain marker trait, so the methods come from the new trait). The trait itself goes in the same file, above `MetadataStore`, with `use microwave_metrics::Metrics;`:

```rust
#[async_trait]
pub trait QuotaRepository: Send + Sync {
    fn set_quotas(&self, policy: QuotaPolicy);
    fn set_org_quotas(&self, org_id: &str, quotas: Option<OrgQuotas>);
    fn quota_limit(&self, org_id: &str, kind: QuotaKind) -> Option<u64>;
    fn set_metrics(&self, metrics: Arc<Metrics>);
}
```

Add `pub(crate) quotas: RwLock<QuotaPolicy>` to both store structs, initialized `RwLock::new(QuotaPolicy::default())` in every constructor, and:

```rust
    pub(crate) fn quota_metrics(&self) -> Option<Arc<Metrics>> {
        self.metrics.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
```

`SqliteStore::enforce_quota`:

```rust
    pub(crate) fn enforce_quota(
        &self, tx: &Transaction<'_>, org_id: &str, target: QuotaTarget<'_>,
    ) -> Result<(), StoreError> {
        if target.is_exempt() { return Ok(()) }
        let kind = target.kind();
        let Some(limit) = self.quota_limit(org_id, kind) else { return Ok(()) };
        if target.exists_sqlite(tx, org_id)? { return Ok(()); }
        let count = count_quota(tx, org_id, kind)?;
        reject_if_full(self.quota_metrics().as_deref(), org_id, kind, limit, count)
    }
```

`QuotaTarget::is_exempt(&self) -> bool` is `true` only for `Environment { name: "_default" }`: the environment is auto-created and cannot be deleted, so it must never be rejected — not even at `max_environments = 0`. The count query excludes it for the same reason.

`PostgresStore::enforce_quota` is the same shape, but reads the limit first (no lock when unlimited), then takes the lock, then checks existence:

```rust
        if target.is_exempt() { return Ok(()) }
        let kind = target.kind();
        let Some(limit) = self.quota_limit(org_id, kind) else { return Ok(()) };
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{org_id}:{}", kind.label()))
            .execute(&mut **tx)
            .await
            .map_err(db)?;
        if target.exists_postgres(tx, org_id).await? { return Ok(()); }
        let count = count_quota(tx, org_id, kind).await?;
        reject_if_full(self.quota_metrics().as_deref(), org_id, kind, limit, count)
```

`count_quota(tx, org_id, kind) -> Result<u64, StoreError>` exists once per backend (async on PostgreSQL, `db` comes from `crate::store`) and runs [`count_sql`]:

```rust
pub fn count_sql(kind: QuotaKind) -> &'static str {
    match kind {
        QuotaKind::Node => "SELECT COUNT(*) FROM nodes WHERE org_id = $1",
        QuotaKind::CookbookVersion => {
            "SELECT (SELECT COUNT(*) FROM cookbook_versions WHERE org_id = $1) \
                   + (SELECT COUNT(*) FROM cookbook_artifact_versions WHERE org_id = $1)"
        }
        QuotaKind::Environment => {
            "SELECT COUNT(*) FROM environments WHERE org_id = $1 AND name <> '_default'"
        }
        QuotaKind::Role => "SELECT COUNT(*) FROM roles WHERE org_id = $1",
        QuotaKind::PolicyRevision => "SELECT COUNT(*) FROM policy_revisions WHERE org_id = $1",
    }
}
```

rusqlite maps the `$1` placeholder to bind index 1, so `params![org_id]` works on SQLite and `$1` works unchanged on PostgreSQL.

Shared free functions in `quotas.rs`:

```rust
pub fn reject_if_full(metrics: Option<&Metrics>, org_id: &str, kind: QuotaKind, limit: u64, count: u64) -> Result<(), StoreError> {
    if count < limit { return Ok(()); }
    tracing::warn!(org_id, kind = kind.label(), limit, "quota reached");
    if let Some(metrics) = metrics {
        metrics.record_limit(&format!("quota:{}", kind.label()), org_id, "rejected");
    }
    Err(StoreError::Quota { kind, limit })
}
```

`exists_sqlite`/`exists_postgres` are inherent methods on `QuotaTarget` in each backend crate, one `SELECT EXISTS(...)` each; the cookbook variant parses the version with `microwave_store::parse_version` first (public in `microwave-store`).

Remove the "Count per kind" prose line above the code blocks and add the inherent limit accessor to both stores (used by `enforce_quota` and the reload test):

```rust
    pub(crate) fn quota_limit(&self, org_id: &str, kind: QuotaKind) -> Option<u64> {
        self.quotas
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .limit(org_id, kind)
    }
```

- [ ] **Step 4: Guard `create_node` in both backends**

In `crates/microwave-store/src/objects.rs`, right after `let tx = connection.transaction()?;`:

```rust
            self.enforce_quota(&tx, org_id, QuotaTarget::Node { name })?;
```

In `crates/microwave-store-postgres/src/objects.rs`, right after the transaction begins:

```rust
            self.enforce_quota(&mut transaction, org_id, QuotaTarget::Node { name })
                .await?;
```

Implement the trait methods `set_quotas`, `set_org_quotas`, `quota_limit` and `set_metrics` for both stores (`QuotaRepository` impl blocks in `backend_sqlite.rs` and a new `impl QuotaRepository for PostgresStore` in `quotas.rs`).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p microwave-store --test dual_backend quota`
Expected: PASS on SQLite; the PostgreSQL half skips without `MICROWAVE_TEST_POSTGRES_URL`.

- [ ] **Step 6: Commit**

```bash
git add crates/microwave-store crates/microwave-store-postgres Cargo.lock
git commit -m "feat(store): add quota policy and node ceiling"
```

---

### Task 3: Cookbook, environment, role and policy-revision guards

**Files:**
- Modify: `crates/microwave-store/src/cookbooks.rs`
- Modify: `crates/microwave-store/src/artifacts.rs`
- Modify: `crates/microwave-store/src/objects.rs` (`create_named_object`)
- Modify: `crates/microwave-store/src/policies.rs` (`create_policy_revision`, `associate_policy_revision`)
- Modify: `crates/microwave-store-postgres/src/cookbooks.rs`
- Modify: `crates/microwave-store-postgres/src/artifacts.rs`
- Modify: `crates/microwave-store-postgres/src/objects.rs` (`create_named_object`)
- Modify: `crates/microwave-store-postgres/src/policies.rs`
- Test: `crates/microwave-store/tests/dual_backend.rs`

**Interfaces:**
- Consumes: `enforce_quota`, `QuotaTarget`, `QuotaKind` from Task 2.
- Produces: `QuotaTarget::for_named_table(table: &'a str, name: &'a str) -> Option<QuotaTarget<'a>>` (maps only `"environments"` and `"roles"`), and guarded `create_cookbook_version`, `create_cookbook_artifact_version`, `create_named_object`, `create_policy_revision` and `associate_policy_revision` on both backends.

- [ ] **Step 1: Write the failing tests**

Add `quota_cookbooks`, `quota_environments`, `quota_roles` and `quota_policies` suites. Each: fixture, one org with the matching ceiling, one allowed create, one rejected create with the exact message, one re-upload at the ceiling that must not be a quota error, delete frees the slot, a second org without an entry stays unlimited. Specifics:

- `quota_cookbooks` uses `max_cookbook_versions: Some(1)`; create `1.0.0`, then `2.0.0` fails with `"cookbook version limit of 1 reached"`; `create_cookbook_artifact_version` for the same ceiling with a new artifact name fails with the same message. `delete_cookbook_version` frees the slot.
- `quota_environments` uses `max_environments: Some(2)`; create two custom environments, the third fails. In a second org, `create_environment(&org.id, "_default", ..., creator)` runs first with `max_environments: Some(1)`, then exactly one custom environment fits and a second fails. In a third org with `max_environments: Some(0)`, `_default` still succeeds and a custom environment fails with `"environment limit of 0 reached"`.
- `quota_roles` uses `max_roles: Some(1)`; the second role fails with `"role limit of 1 reached"`.
- `quota_policies` uses `max_policy_revisions: Some(1)`; a second revision fails. Separately, with the ceiling reached, `associate_policy_revision` to a *new* revision fails with `"policy revision limit of 1 reached"`, and associating the *existing* revision succeeds. Delete the revision, then the association can create it.

Manifest strings: `format!(r#"{{"name":"{name}","version":"1.0.0"}}"#)`; checksums: `&[]` for artifact versions.

Add all four names to both `matrix!` lists.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-store --test dual_backend quota_`
Expected: FAIL with the second create succeeding instead of returning `Quota`.

- [ ] **Step 3: Add the guards**

- `create_cookbook_version` (both backends): after the transaction opens, `enforce_quota(..., QuotaTarget::CookbookVersion { name, version })`.
- `create_cookbook_artifact_version` (both backends): after the transaction opens, `enforce_quota(..., QuotaTarget::CookbookArtifactVersion { name, identifier })`. This runs before the artifact row is created, which is correct: a new artifact name has no versions yet.
- `create_named_object` (both backends): after the transaction opens,

```rust
        if let Some(target) = QuotaTarget::for_named_table(table, name) {
            self.enforce_quota(&tx, org_id, target)?;
        }
```

with `QuotaTarget::for_named_table(table: &'a str, name: &'a str) -> Option<QuotaTarget<'a>>` mapping only `"environments"` and `"roles"`. On PostgreSQL the call is `.await?` against `&mut transaction`.

- `create_policy_revision` (both backends): after the transaction opens, `enforce_quota(..., QuotaTarget::PolicyRevision { name, revision_id })`.
- `associate_policy_revision` (both backends): after the transaction opens and before `ensure_policy_revision`, the same call. `ensure_policy_revision` already returns an existing row early, so an association that reuses the revision costs no count.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-store --test dual_backend quota_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-store crates/microwave-store-postgres
git commit -m "feat(store): enforce quotas on every remaining creation path"
```

---

### Task 4: `403` mapping, `org-create` entry, and API tests

**Files:**
- Modify: `crates/microwave-api/src/error.rs` (`From<StoreError>`)
- Modify: `crates/microwave-api/src/organizations.rs`
- Modify: `crates/microwave-authz/src/org.rs` (`create_organization` signature)
- Modify: `crates/microwave-authz/Cargo.toml` (add `microwave-config.workspace = true`)
- Modify: `crates/microwave-authz/tests/org_test.rs` (seven calls)
- Modify: `crates/microwave-api/tests/support/mod.rs:118`
- Modify: `crates/microwave-api/tests/{membership_shapes,groups,oauth}_test.rs`
- Modify: `crates/microwave-migrate/tests/{roundtrip,goiardi_policy}_test.rs`
- Modify: `crates/microwave-api/tests/mcp_test.rs`
- Test: `crates/microwave-api/tests/quotas_test.rs` (new)

**Interfaces:**
- Consumes: `StoreError::Quota`, `OrgQuotas`, `QuotasConfig::org`, `QuotaRepository`.
- Produces:
  - `StoreError::Quota { .. }` maps to `ApiError::forbidden_with`.
  - `create_organization<S>(store, name, full_name, creator_user_id, creator_authz_id, quotas: Option<OrgQuotas>) -> Result<Org, StoreError>` where `S: IdentityRepository + AuthzRepository + ObjectRepository + QuotaRepository + ?Sized`.

- [ ] **Step 1: Write the failing API tests**

Create `crates/microwave-api/tests/quotas_test.rs` modelled on `limits_test.rs` (that file's `config()` helper, `support::app(state)`, `support::signed_request`) and copy the policyfile request shape from `policies_test.rs:130-146`. Build the state with `state_with_config`, then for each case create the org through `support::add_org` and install the ceiling with `state.sqlite().set_org_quotas(&org.id, Some(OrgQuotas { .. }))`; import `microwave_config::OrgQuotas` and `microwave_store::QuotaRepository`.

Cases:
- `node_creation_over_the_ceiling_is_forbidden`: `max_nodes = Some(1)`; `PUT /organizations/acme/nodes/web1` → `201`; `web2` → `403` with `{"error":["node limit of 1 reached"]}`; `PUT` of `web1` again → `200`; `DELETE web1`, then `PUT web2` → `201`.
- `cookbook_version_over_the_ceiling_is_forbidden`: same shape for `PUT /organizations/acme/cookbooks/{name}/{version}`; the second version is `403`; re-`PUT` of the first version is `200`.
- `environment_over_the_ceiling_is_forbidden`: `max_environments = Some(1)`; one custom environment `201`, the second `403`; `_default` does not count.
- `role_over_the_ceiling_is_forbidden`.
- `policy_revision_over_the_ceiling_is_forbidden`: `max_policy_revisions = Some(1)`; create `rev1` (`201`), then `rev2` → `403` and `"policy revision limit of 1 reached"`; a `PUT /organizations/acme/policy_groups/prod/policies/app` (create the group first via `POST /policy_groups`) that carries `rev2` → `403` with the same text; `DELETE /policies/app/revisions/rev1`, then the same `PUT` → `201`.
- `no_cross_organization_bleed`: `acme` at its ceiling, `beta` without an entry still creates.
- `the_rejection_is_counted`: after the `403`, `state.metrics.limit_series()` contains `scope == "quota:node"`, `outcome == "rejected"`, `org == <org id>`.
- `org_create_installs_the_entry_immediately`: build the config with `config.quotas.organizations.insert("acme".into(), OrgQuotas { max_nodes: Some(0), max_environments: Some(0), ..Default::default() })` before `state_with_config`; `POST /organizations` with `{"name":"acme","full_name":"Acme"}` → `201` (the `_default` environment fits); `PUT /organizations/acme/nodes/web1` → `403` with `"node limit of 0 reached"`.
- MCP: add to `crates/microwave-api/tests/mcp_test.rs` (import `microwave_config::OrgQuotas` and `microwave_store::QuotaRepository`):

```rust
#[tokio::test]
async fn create_node_over_a_quota_answers_is_error() {
    let (state, org, app) = agent_fixture(mcp_config_with_principal("acme", "a", "agent")).await;
    state
        .sqlite()
        .set_org_quotas(&org.id, Some(OrgQuotas { max_nodes: Some(1), ..Default::default() }));
    let created = call_tool(&app, "acme", "a", 1, "create_node", json!({ "name": "web1" })).await;
    assert_eq!(created["name"], "web1");
    let result = call_tool_raw(&app, "acme", "a", 2, "create_node", json!({ "name": "web2" })).await;
    assert_eq!(result["result"]["isError"], json!(true));
    assert_eq!(
        result["result"]["content"][0]["text"],
        "node limit of 1 reached"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test quotas_test`
Expected: FAIL with `403` missing (today the second create succeeds) or unresolved `OrgQuotas`.

- [ ] **Step 3: Map the error and wire `org-create`**

In `From<StoreError>` add:

```rust
            StoreError::Quota { kind, limit } => {
                Self::forbidden_with(format!("{kind} limit of {limit} reached"))
            }
```

In `crates/microwave-authz/src/org.rs`: add the `QuotaRepository` bound and the `microwave_config::OrgQuotas` import, add the parameter, and immediately after `create_org` (before the `apply_default_policy` call):

```rust
    store.set_org_quotas(&org.id, quotas);
```

In the rollback arm, also `store.set_org_quotas(&org.id, None);`.

In `organizations.rs`, pass `state.config.quotas.org(&name)` as the last argument.

Update every other caller with `None`: `support/mod.rs:118`, `membership_shapes_test.rs:34`, `groups_test.rs:360`, `oauth_test.rs:53`, `org_test.rs` (seven), `roundtrip_test.rs:68,71`, `goiardi_policy_test.rs:125,620`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --test quotas_test && cargo test -p microwave-authz --test org_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api crates/microwave-authz crates/microwave-migrate
git commit -m "feat(api): answer 403 on a quota and apply org entries at create"
```

---

### Task 5: Startup, reload and ctl diagnostics

**Files:**
- Create: `bin/microwave-server/src/quotas.rs`
- Modify: `bin/microwave-server/src/main.rs` (module, install after the superuser bootstrap)
- Modify: `bin/microwave-server/src/reload.rs` (install after `verify_buckets`, tests)
- Modify: `crates/microwave-ctl/src/diagnostics.rs` (quota problems, summary)
- Modify: `crates/microwave-ctl/src/commands.rs:331` (pass the entry)
- Test: `crates/microwave-ctl/src/diagnostics.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Config.quotas`, `QuotaPolicy::set_org`, `MetadataStore::set_quotas`, `microwave_config::tenant_issues`-style issue handling.
- Produces:
  - `bin/microwave-server/src/quotas.rs`: `pub(crate) async fn resolve(config: &Config, store: &dyn MetadataStore) -> Result<QuotaPolicy, StoreError>`
  - `diagnostics` reports `quotas.organizations.<name> names an unknown organization` as a problem and prints `quotas: <n> organization(s)` (or `quotas: disabled`).

- [ ] **Step 1: Write the failing ctl and reload tests**

In `crates/microwave-ctl/src/diagnostics.rs` `mod tests`, add `quotas_summary` to the `use super::{..}` list and build a `Context` whose SQLite file has an org:

```rust
    fn context_with_org(dir: &tempfile::TempDir, org: &str) -> Context {
        let path = dir.path().join("microwave.db");
        let store = SqliteStore::open(&path).unwrap();
        store.create_org(org, org).unwrap();
        drop(store);
        let mut context = context_with_pool_size(10);
        context.database_path = Some(path);
        context.config.quotas.organizations.insert("ghost".into(), Default::default());
        context
    }

    #[test]
    fn check_config_reports_an_unknown_quota_organization() {
        let dir = tempfile::tempdir().unwrap();
        let error = check_config(&context_with_org(&dir, "acme")).unwrap_err();
        assert!(error.to_string().contains("quotas.organizations.ghost names an unknown organization"));
    }

    #[test]
    fn check_config_accepts_a_known_quota_organization() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context_with_org(&dir, "acme");
        context.config.quotas.organizations.clear();
        context.config.quotas.organizations.insert("acme".into(), Default::default());
        assert!(check_config(&context).is_ok());
    }
```

Assert `quotas_summary`: with one entry it returns `quotas: 1 organization(s)`, and with `enabled = false` it returns `quotas: disabled`.

In `reload.rs` tests, import `microwave_store::{QuotaKind, QuotaRepository}`, add `store: Arc<SqliteStore>` to `Harness` (clone it in `harness()` before the store is coerced into `Reloader`) and add:

- `a_changed_quota_applies_to_the_store`: `harness()`, write a config with `[quotas.organizations.acme] max_nodes = 0`, reload, assert `harness.store.quota_limit(&harness.org_id, QuotaKind::Node) == Some(0)`.
- `a_rejected_reload_keeps_the_previous_quota`: start with `max_nodes = 5`, reload an invalid config (`base_config(None)`), assert the limit is still `Some(5)`.
- `an_unknown_quota_organization_only_warns`: capture logs like the existing `an_unknown_organization_only_warns` test, reload a config with `[quotas.organizations.ghost] max_nodes = 1`, assert the log mentions `ghost` and the reload succeeded.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-ctl diagnostics::tests::check_config_reports_an_unknown_quota_organization && cargo test -p microwave-server reload::tests::a_changed_quota`
Expected: FAIL.

- [ ] **Step 3: Implement resolution, install and diagnostics**

`bin/microwave-server/src/quotas.rs`:

```rust
pub(crate) async fn resolve(config: &Config, store: &dyn MetadataStore) -> Result<QuotaPolicy, StoreError> {
    let mut policy = QuotaPolicy::default();
    if !config.quotas.enabled || config.quotas.organizations.is_empty() {
        return Ok(policy);
    }
    let ids: BTreeMap<String, String> = store.list_orgs().await?
        .into_iter().map(|org| (org.name, org.id)).collect();
    for (name, quotas) in &config.quotas.organizations {
        match ids.get(name) {
            Some(id) => policy.set_org(id, Some(quotas.clone())),
            None => tracing::warn!(
                organization = %name,
                "quotas.organizations names an unknown organization; its entry stays inactive"
            ),
        }
    }
    Ok(policy)
}
```

In `main.rs`, add `QuotaRepository` to the `microwave_store` import, declare `mod quotas;`, and after `bootstrap::apply_otel_secrets(...)` / the bookshelf secrets and before `build_timed_object_store`, add `store.set_quotas(quotas::resolve(&config, store.as_ref()).await?);`.

In `reload.rs::run`, after `verify_buckets` and before `self.router.install(snapshot)`, resolve and remember, then install both:

```rust
        verify_buckets(&config.object_store, &snapshot).await.map_err(rejected)?;
        // Resolve only after the snapshot proved usable, then swap both.
        let quotas = crate::quotas::resolve(&config, self.store.as_ref())
            .await
            .map_err(rejected)?;
        self.router.install(snapshot);
        self.store.set_quotas(quotas);
```

In `diagnostics.rs`, inside the `TenantState::Known` arm, after the object-store block, gate on `!config.quotas.organizations.is_empty()` and:

```rust
                let mut quota_ok = true;
                for organization in config.quotas.organizations.keys() {
                    if !orgs.contains(organization) {
                        problems.push(format!(
                            "quotas.organizations.{organization} names an unknown organization"
                        ));
                        quota_ok = false;
                    }
                }
                if quota_ok {
                    checks.push(format!(
                        "quotas: {} organization(s)",
                        config.quotas.organizations.len()
                    ));
                }
```

Add `fn quotas_summary(config: &Config) -> String` returning `quotas: disabled` when `enabled` is false and `quotas: <n> organization(s)` otherwise, and print it in `diagnostics` right after `summary(...)`. On `TenantState::Unknown`, push `"quotas: (organization table not readable yet)"` to `checks` instead of the known-arm check. The tenant block must now also run when the object store is local and quota entries exist: change the outer condition to `config.object_store.backend != ObjectStoreBackend::Local || !config.quotas.organizations.is_empty()` and wrap the object-store specific checks in `if config.object_store.backend != ObjectStoreBackend::Local`.

In `commands.rs:331`, pass `context.config.quotas.org(name)` as the last argument.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-ctl && cargo test -p microwave-server reload`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bin/microwave-server crates/microwave-ctl
git commit -m "feat(server): install quotas at startup and reload; diagnose them"
```

---

### Task 6: Operator page and claims

**Files:**
- Create: `docs/quotas.md`
- Modify: `docs/index.md` (Start here bullet)
- Modify: `docs/deployment.md` (a `#### Object quotas` table after the request-limits table)
- Modify: `docs/multi-tenant-mode.md` (cross-reference in `What you need`)
- Modify: `docs/request-limits.md` (note after `What the limits can do`)
- Modify: `website/docs/sidebars.ts`
- Modify: `website/docs/docusaurus.config.ts` (`include` list)
- Modify: `docs/claims.toml`

**Interfaces:**
- Consumes: the shipped behaviour from Tasks 1–5.
- Produces: the operator page and its traceability.

- [ ] **Step 1: Write `docs/quotas.md`**

Start with the H1 `# Object quotas`.

Cover, in this order: what a quota is and that it is opt-in and hard; the five kinds; the full TOML example from the spec; `_default` exemption and `0` as a valid ceiling; the `403` body and that updates/deletes still work; that a re-upload is not blocked; the `quota:<kind>` metric series and the `WARN` log; `check-config` (unknown organization fails, reload only warns, new org applies immediately); independence from `limits.enabled` and `require_org_bucket`; and that `microwave-migrate` imports bypass quotas. State that a lowered limit leaves existing objects in place and only blocks additions.

Use these exact sentences, each pinned by a claim below:

```
A quota is a hard, per-organization ceiling on one object kind.
The auto-created `_default` environment is exempt, so `max_environments = 0` still creates an organization.
Zero is a valid ceiling: the organization may hold no object of that kind at all.
A creation over the ceiling answers `403` with `{"error":["node limit of 5 reached"]}`; the same `PUT` on an existing object still updates it.
Quotas are independent of `limits.enabled` and of `object_store.require_org_bucket`.
A rejection increments `microwave_server_limits_total{scope="quota:<kind>",outcome="rejected"}`.
An entry for a new organization is registered with `org-create`, so pre-provisioning works.
A `microwave-migrate` import runs without quotas, so a restore is never truncated.
```

- [ ] **Step 2: Wire the docs site**

Add `{type: 'doc', id: 'quotas', label: 'Object quotas'}` after the `request-limits` entry in `sidebars.ts` and `'quotas.md'` after `'request-limits.md'` in the `docusaurus.config.ts` include list.

Add an `index.md` bullet after the request-limits bullet:

```markdown
- **[Cap what a tenant stores](./quotas.md)** — opt-in, hard object ceilings per
  organization: the five kinds, the exact `403`, diagnostics, and the reload.
```

Add the cross-references: in `multi-tenant-mode.md` a sentence after the "The bucket name is the only per-organization value…" paragraph pointing at `quotas.md`; in `request-limits.md` a sentence after the "Show what is happening" bullet noting that object quotas are separate from these limits and pointing at `quotas.md`.

Extend the leading `//!` module doc block of `crates/microwave-migrate/src/lib.rs` with `//! Quota checks are not installed on the import store, so imports bypass them.`

Add a `#### Object quotas` section to `deployment.md` directly after the `limits.organizations.<name>` table row and before the `#### Key cache, Data Collector and integrations` heading: one row per field (`quotas.enabled` / `MW_QUOTAS__ENABLED`, `quotas.organizations` / `MW_QUOTAS__ORGANIZATIONS`, and the five `max_*` keys), each default "unset" (except `enabled`, default `true`), each with the meaning. Link `quotas.md` for the full behaviour.

- [ ] **Step 3: Add the claims**

Append to `docs/claims.toml`:

```toml
[[claim]]
id = "quotas-guide-published"
summary = "The object-quotas guide is part of the published docs site."
area = "docs"
status = "verified"
surfaces = [
  { path = "docs/quotas.md", needle = "Object quotas" },
  { path = "docs/index.md", needle = "quotas.md" },
  { path = "website/docs/sidebars.ts", needle = "quotas" },
]
verify = { kind = "grep", path = "website/docs/docusaurus.config.ts", pattern = "quotas.md" }

[[claim]]
id = "quotas-hard-and-403"
summary = "A quota is hard and a creation over it answers 403 while updates still work."
area = "docs"
status = "verified"
surfaces = [
  { path = "docs/quotas.md", needle = "A quota is a hard, per-organization ceiling on one object kind." },
  { path = "docs/quotas.md", needle = "the same `PUT` on an existing object still updates it" },
]
verify = { kind = "grep", path = "crates/microwave-store/src/sqlite.rs", pattern = "limit of" }

[[claim]]
id = "quotas-zero-and-default-exempt"
summary = "Zero is a valid ceiling and the _default environment never counts."
area = "docs"
status = "verified"
surfaces = [
  { path = "docs/quotas.md", needle = "Zero is a valid ceiling" },
  { path = "docs/quotas.md", needle = "`max_environments = 0` still creates an organization" },
]
verify = { kind = "grep", path = "crates/microwave-store/src/quotas.rs", pattern = "name <> '_default'" }

[[claim]]
id = "quotas-independent-of-limits"
summary = "Quotas do not share the limits.enabled switch."
area = "docs"
status = "verified"
surfaces = [ { path = "docs/quotas.md", needle = "Quotas are independent of" } ]
verify = { kind = "grep", path = "crates/microwave-config/src/config.rs", pattern = "pub quotas: QuotasConfig" }

[[claim]]
id = "quotas-metric-series"
summary = "A rejection lands on the request-limit metric series with a quota scope."
area = "docs"
status = "verified"
surfaces = [ { path = "docs/quotas.md", needle = "microwave_server_limits_total" } ]
verify = { kind = "grep", path = "crates/microwave-store/src/quotas.rs", pattern = "record_limit" }

[[claim]]
id = "quotas-org-create-immediate"
summary = "A pre-provisioned entry acts from the first request because org-create registers it."
area = "docs"
status = "verified"
surfaces = [ { path = "docs/quotas.md", needle = "org-create" } ]
verify = { kind = "grep", path = "crates/microwave-authz/src/org.rs", pattern = "set_org_quotas" }

[[claim]]
id = "quotas-migrate-bypass"
summary = "Migration imports run without quotas."
area = "docs"
status = "verified"
surfaces = [ { path = "docs/quotas.md", needle = "a restore is never truncated" } ]
verify = { kind = "grep", path = "crates/microwave-migrate/src/lib.rs", pattern = "Quota checks are not installed" }
```

- [ ] **Step 4: Run the docs checks**

Run: `cd website/docs && npm run build`
Expected: PASS.

Run: `python3 scripts/render-remaining-work.py`, then `python3 scripts/check-claims.py`, `cd website/homepage && npm test`, and `cargo test -p microwave-migrate`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add docs website crates/microwave-migrate README.md
git commit -m "docs(quotas): document the opt-in per-organization ceilings"
```

---

## Self-review notes

- Config validation, env overlay, example block: Task 1.
- Store check with existence short-circuit, advisory lock, per-kind count and shared metric: Tasks 2–3.
- `403` mapping and MCP: Task 4.
- Startup install, reload order, `org-create` immediate effect, diagnostics: Tasks 4–5.
- Docs, site wiring, claims: Task 6.
