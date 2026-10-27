# Run Report Storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Optionally persist Chef `run_converge` reports in SQLite or PostgreSQL, alongside or instead of forwarding them to an external data collector.

**Architecture:** Add a `ReportRepository` trait and a `node_runs` table to both backends. Extend `[data_collector]` with `store = forward|local|both` and `keep_runs` (default 50). The existing `POST /organizations/:org/data-collector` handler parses only `run_converge` messages, stores them before forwarding, and keeps today's forward-only behavior as the default.

**Tech Stack:** Rust, axum, rusqlite, sqlx/PostgreSQL, serde_json, chrono, zlib/base64 compression helper.

**Spec:** `docs/superpowers/specs/2026-10-27-mcp-run-reports-design.md`

## Global Constraints

- SQLite and PostgreSQL both implement `ReportRepository`; the dual-backend matrix is the parity gate.
- Default behavior is unchanged: `store = "forward"`, `keep_runs = 50`, no local storage unless configured.
- Only `message_type = "run_converge"` is stored. `run_start` and `action` messages are forwarded/acknowledged but never stored.
- `start_time` is stored normalized to UTC with millisecond precision (`YYYY-MM-DDTHH:MM:SS.sssZ`); a missing or invalid value falls back to the server receive time.
- `keep_runs = 0` disables pruning.
- Reports are not added to logical snapshots or `microwave-migrate`.
- Data-collector authentication semantics do not change: shared `x-data-collector-token` or Chef signature.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets`, and `cargo test --workspace` must pass.

## Review Focus

These are the failure modes most likely to bite an operator; each gets a test in the task that owns the code.

1. `keep_runs = 0` must keep every run, not delete everything.
2. A retried `run_converge` with the same `(org, node_name, run_id)` must leave one row, preserve the original row id, and update the payload/status.
3. `store = "both"` with a failing upstream must return the upstream 502/504 but keep the locally stored row.
4. An unknown organization with local storage enabled must return 404 and store nothing; forward-only mode must keep forwarding without an org lookup.
5. A missing or unparseable `start_time` must fall back to the receive time, never fail the request.

---

### Task 1: Data-collector store mode and retention config

**Files:**
- Modify: `crates/microwave-config/src/config.rs`
- Modify: `crates/microwave-config/src/lib.rs`
- Test: `crates/microwave-config/tests/config_test.rs`

**Interfaces:**
- Produces: `pub enum DataCollectorStore { Forward, Local, Both }`, `#[derive(Default)]` with `Forward`.
- Produces: `DataCollectorConfig { store: DataCollectorStore, keep_runs: usize, .. }`.
- Produces: `DataCollectorConfig::is_enabled() -> bool`, `stores_locally() -> bool`, `forwards() -> bool`.
- Consumes: nothing from earlier tasks.

- [ ] **Step 1: Write the failing config tests**

Add to `crates/microwave-config/tests/config_test.rs`:

```rust
use microwave_config::DataCollectorStore;

#[test]
fn data_collector_defaults_to_forward_with_50_runs() {
    let c = Config::default();
    assert_eq!(c.data_collector.store, DataCollectorStore::Forward);
    assert_eq!(c.data_collector.keep_runs, 50);
    assert!(!c.data_collector.is_enabled());
    assert!(c.data_collector.forwards());
    assert!(!c.data_collector.stores_locally());
}

#[test]
fn data_collector_local_enables_the_surface_without_forwarding() {
    let c = Config::from_toml_str(
        r#"
        [data_collector]
        store = "local"
        keep_runs = 5
    "#,
    )
    .unwrap();
    assert_eq!(c.data_collector.store, DataCollectorStore::Local);
    assert_eq!(c.data_collector.keep_runs, 5);
    assert!(!c.data_collector.is_configured());
    assert!(c.data_collector.is_enabled());
    assert!(c.data_collector.stores_locally());
    assert!(!c.data_collector.forwards());
}

#[test]
fn data_collector_both_requires_a_root_url() {
    assert!(Config::from_toml_str(
        r#"
        [data_collector]
        store = "both"
    "#
    )
    .is_err());

    let c = Config::from_toml_str(
        r#"
        [data_collector]
        store = "both"
        root_url = "http://127.0.0.1:1/data-collector/v0/"
    "#,
    )
    .unwrap();
    assert!(c.data_collector.is_enabled());
    assert!(c.data_collector.forwards());
    assert!(c.data_collector.stores_locally());
}

#[test]
fn data_collector_rejects_unknown_modes_and_negative_retention() {
    assert!(Config::from_toml_str(
        r#"
        [data_collector]
        store = "sometimes"
    "#
    )
    .is_err());
    assert!(Config::from_toml_str(
        r#"
        [data_collector]
        keep_runs = -1
    "#
    )
    .is_err());
}

#[test]
fn data_collector_local_suppresses_action_notifications() {
    let c = Config::from_toml_str(
        r#"
        [data_collector]
        store = "local"
        root_url = "http://127.0.0.1:1/data-collector/v0/"
        actions_enabled = true
    "#,
    )
    .unwrap();
    assert!(!c.data_collector.actions_enabled());
}

#[test]
fn data_collector_store_and_retention_can_come_from_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_DATA_COLLECTOR__STORE", "local");
    std::env::set_var("MW_DATA_COLLECTOR__KEEP_RUNS", "7");
    let mut c = Config::default();
    c.apply_env().unwrap();
    assert_eq!(c.data_collector.store, DataCollectorStore::Local);
    assert_eq!(c.data_collector.keep_runs, 7);

    std::env::set_var("MW_DATA_COLLECTOR__STORE", "nope");
    let mut c = Config::default();
    assert!(c.apply_env().is_err());

    std::env::remove_var("MW_DATA_COLLECTOR__STORE");
    std::env::remove_var("MW_DATA_COLLECTOR__KEEP_RUNS");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-config --test config_test data_collector`
Expected: FAIL to compile (`DataCollectorStore` and fields do not exist).

- [ ] **Step 3: Implement the config**

In `crates/microwave-config/src/config.rs`:

```rust
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DataCollectorStore {
    #[default]
    Forward,
    Local,
    Both,
}

impl DataCollectorStore {
    pub fn stores_locally(self) -> bool {
        matches!(self, Self::Local | Self::Both)
    }

    pub fn forwards(self) -> bool {
        matches!(self, Self::Forward | Self::Both)
    }
}
```

Add to `DataCollectorConfig`:

```rust
pub store: DataCollectorStore,
pub keep_runs: usize,
```

Initialize them in `Default` (`Forward`, `50`). Add:

```rust
pub fn is_enabled(&self) -> bool {
    self.is_configured() || self.store.stores_locally()
}
```

Change `actions_enabled()` to require forwarding:

```rust
pub fn actions_enabled(&self) -> bool {
    self.is_configured() && self.store.forwards() && self.actions_enabled
}
```

In `Config::validate`, after the timeout check:

```rust
if self.data_collector.store == DataCollectorStore::Both
    && !self.data_collector.is_configured()
{
    return Err(ConfigError::InvalidValue {
        key: "data_collector.root_url".into(),
        value: "required when data_collector.store is \"both\"".into(),
    });
}
```

In `apply_env`, after the timeout override:

```rust
if let Ok(value) = std::env::var("MW_DATA_COLLECTOR__STORE") {
    self.data_collector.store = match value.trim().to_ascii_lowercase().as_str() {
        "forward" => DataCollectorStore::Forward,
        "local" => DataCollectorStore::Local,
        "both" => DataCollectorStore::Both,
        _ => {
            return Err(ConfigError::InvalidValue {
                key: "MW_DATA_COLLECTOR__STORE".into(),
                value,
            })
        }
    };
}
if let Ok(value) = std::env::var("MW_DATA_COLLECTOR__KEEP_RUNS") {
    self.data_collector.keep_runs =
        parse_value("MW_DATA_COLLECTOR__KEEP_RUNS", value)?;
}
```

Add `DataCollectorStore` to the `pub use config::{...}` list in
`crates/microwave-config/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-config --test config_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-config/src/config.rs crates/microwave-config/src/lib.rs crates/microwave-config/tests/config_test.rs
git commit -m "feat(config): add data collector store mode and retention"
```

---

### Task 2: Report repository schema and SQLite backend

**Files:**
- Modify: `crates/microwave-store/migrations/0001_initial.sql`
- Modify: `crates/microwave-store/src/backend.rs`
- Create: `crates/microwave-store/src/reports.rs`
- Modify: `crates/microwave-store/src/lib.rs`
- Modify: `crates/microwave-store/src/backend_sqlite.rs`
- Create: `crates/microwave-store-postgres/src/reports.rs` (compiling stub only)
- Modify: `crates/microwave-store-postgres/src/lib.rs`
- Test: `crates/microwave-store/src/reports.rs` (`#[cfg(test)] mod tests`)
- Test: `crates/microwave-store/tests/migration_security_test.rs`

**Interfaces:**
- Produces: `NewNodeRun<'a>`, `NodeRunFilter`, `StoredNodeRun`.
- Produces: `ReportRepository` with `store_node_run`, `list_node_runs`, `find_node_run`, `latest_node_run`.
- Adds `ReportRepository` to the `MetadataStore` supertrait list.
- Produces: `SqliteStore` inherent methods used by the trait impl.
- Produces: a compiling `ReportRepository for PostgresStore` stub whose methods return
  `Err(StoreError::Invalid("report repository not implemented yet"))`; Task 3 replaces it.

- [ ] **Step 1: Write the failing SQLite tests**

At the bottom of the new `crates/microwave-store/src/reports.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::MetadataStore;
    use std::sync::Arc;

    async fn fixture() -> (Arc<dyn MetadataStore>, String) {
        let store: Arc<dyn MetadataStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let org = store.create_org("reports-org", "Reports").await.unwrap();
        (store, org.id)
    }

    fn run<'a>(node: &'a str, run_id: &'a str, start: &'a str) -> NewNodeRun<'a> {
        NewNodeRun {
            node_name: node,
            run_id,
            status: "success",
            start_time: Some(start),
            end_time: Some("2026-10-27T10:01:00.000Z"),
            resource_count: 3,
            updated_resource_count: 2,
            error_count: 0,
            report: r#"{"message_type":"run_converge","run_id":"placeholder"}"#,
        }
    }

    #[tokio::test]
    async fn stores_lists_and_finds_a_run() {
        let (store, org_id) = fixture().await;
        let stored = store
            .store_node_run(
                &org_id,
                run("web1", "run-1", "2026-10-27T10:00:00.000Z"),
                10,
            )
            .await
            .unwrap();
        assert_eq!(stored.node_name, "web1");
        assert_eq!(stored.report, r#"{"message_type":"run_converge","run_id":"placeholder"}"#);

        let listed = store
            .list_node_runs(
                &org_id,
                &NodeRunFilter {
                    node_name: None,
                    status: None,
                    since: None,
                    failures_only: false,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(listed, vec![stored.clone()]);
        assert_eq!(
            store.find_node_run(&org_id, "web1", "run-1").await.unwrap(),
            Some(stored.clone())
        );
        assert_eq!(
            store.latest_node_run(&org_id, "web1").await.unwrap(),
            Some(stored)
        );
    }

    #[tokio::test]
    async fn reinsert_is_idempotent_and_preserves_the_id() {
        let (store, org_id) = fixture().await;
        let first = store
            .store_node_run(&org_id, run("web1", "run-1", "2026-10-27T10:00:00.000Z"), 10)
            .await
            .unwrap();
        let mut second_run = run("web1", "run-1", "2026-10-27T10:00:00.000Z");
        second_run.status = "failure";
        second_run.error_count = 1;
        second_run.report = r#"{"message_type":"run_converge","run_id":"retry"}"#;
        let second = store.store_node_run(&org_id, second_run, 10).await.unwrap();

        assert_eq!(second.id, first.id);
        assert_eq!(second.status, "failure");
        assert_eq!(second.error_count, 1);
        assert_eq!(
            store
                .list_node_runs(
                    &org_id,
                    &NodeRunFilter {
                        node_name: Some("web1".into()),
                        status: None,
                        since: None,
                        failures_only: false,
                        limit: 10,
                    },
                )
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn filters_and_orders_by_start_time() {
        let (store, org_id) = fixture().await;
        for (run_id, start, status, errors) in [
            ("run-1", "2026-10-27T10:00:00.000Z", "success", 0),
            ("run-2", "2026-10-27T11:00:00.000Z", "failure", 1),
            ("run-3", "2026-10-27T12:00:00.000Z", "success", 0),
        ] {
            let mut value = run("web1", run_id, start);
            value.status = status;
            value.error_count = errors;
            store.store_node_run(&org_id, value, 10).await.unwrap();
        }

        let failures = store
            .list_node_runs(
                &org_id,
                &NodeRunFilter {
                    node_name: None,
                    status: None,
                    since: None,
                    failures_only: true,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].run_id, "run-2");

        let since = store
            .list_node_runs(
                &org_id,
                &NodeRunFilter {
                    node_name: None,
                    status: None,
                    since: Some("2026-10-27T11:30:00.000Z".into()),
                    failures_only: false,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].run_id, "run-3");
    }

    #[tokio::test]
    async fn prunes_to_keep_runs_and_zero_disables_pruning() {
        let (store, org_id) = fixture().await;
        for (run_id, start) in [
            ("run-1", "2026-10-27T10:00:00.000Z"),
            ("run-2", "2026-10-27T11:00:00.000Z"),
            ("run-3", "2026-10-27T12:00:00.000Z"),
        ] {
            store
                .store_node_run(&org_id, run("web1", run_id, start), 2)
                .await
                .unwrap();
        }
        let kept = store
            .list_node_runs(
                &org_id,
                &NodeRunFilter {
                    node_name: None,
                    status: None,
                    since: None,
                    failures_only: false,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            kept.iter().map(|run| run.run_id.as_str()).collect::<Vec<_>>(),
            vec!["run-3", "run-2"]
        );

        store
            .store_node_run(&org_id, run("web2", "run-1", "2026-10-27T10:00:00.000Z"), 0)
            .await
            .unwrap();
        store
            .store_node_run(&org_id, run("web2", "run-2", "2026-10-27T11:00:00.000Z"), 0)
            .await
            .unwrap();
        let web2 = store
            .list_node_runs(
                &org_id,
                &NodeRunFilter {
                    node_name: Some("web2".into()),
                    status: None,
                    since: None,
                    failures_only: false,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(web2.len(), 2);
    }

    #[tokio::test]
    async fn isolates_organizations_and_round_trips_missing_end_time() {
        let (store, org_id) = fixture().await;
        let other = store.create_org("reports-other", "Other").await.unwrap();
        let mut value = run("web1", "run-1", "2026-10-27T10:00:00.000Z");
        value.end_time = None;
        store.store_node_run(&org_id, value, 10).await.unwrap();

        let other_runs = store
            .list_node_runs(
                &other.id,
                &NodeRunFilter {
                    node_name: None,
                    status: None,
                    since: None,
                    failures_only: false,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert!(other_runs.is_empty());
        let stored = store
            .find_node_run(&org_id, "web1", "run-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.end_time, None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-store --lib reports`
Expected: FAIL to compile (`ReportRepository`, `NewNodeRun`, `NodeRunFilter` missing).

- [ ] **Step 3: Add the migration**

Append to `crates/microwave-store/migrations/0001_initial.sql`:

```sql
CREATE TABLE IF NOT EXISTS node_runs (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    node_name TEXT NOT NULL,
    run_id TEXT NOT NULL,
    status TEXT NOT NULL,
    start_time TEXT NOT NULL,
    end_time TEXT,
    resource_count INTEGER NOT NULL DEFAULT 0,
    updated_resource_count INTEGER NOT NULL DEFAULT 0,
    error_count INTEGER NOT NULL DEFAULT 0,
    report TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (org_id, node_name, run_id),
    FOREIGN KEY (org_id) REFERENCES orgs (id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS node_runs_org_node_time_index
    ON node_runs (org_id, node_name, start_time DESC);
CREATE INDEX IF NOT EXISTS node_runs_org_time_index
    ON node_runs (org_id, start_time DESC);
```

- [ ] **Step 4: Add the trait and types**

In `crates/microwave-store/src/backend.rs`, next to the other repository traits:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewNodeRun<'a> {
    pub node_name: &'a str,
    pub run_id: &'a str,
    pub status: &'a str,
    pub start_time: Option<&'a str>,
    pub end_time: Option<&'a str>,
    pub resource_count: i64,
    pub updated_resource_count: i64,
    pub error_count: i64,
    pub report: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRunFilter {
    pub node_name: Option<String>,
    pub status: Option<String>,
    pub since: Option<String>,
    pub failures_only: bool,
    pub limit: usize,
}

#[async_trait]
pub trait ReportRepository: Send + Sync {
    async fn store_node_run(
        &self,
        org_id: &str,
        run: NewNodeRun<'_>,
        keep_runs: usize,
    ) -> Result<crate::StoredNodeRun, StoreError>;

    async fn list_node_runs(
        &self,
        org_id: &str,
        filter: &NodeRunFilter,
    ) -> Result<Vec<crate::StoredNodeRun>, StoreError>;

    async fn find_node_run(
        &self,
        org_id: &str,
        node_name: &str,
        run_id: &str,
    ) -> Result<Option<crate::StoredNodeRun>, StoreError>;

    async fn latest_node_run(
        &self,
        org_id: &str,
        node_name: &str,
    ) -> Result<Option<crate::StoredNodeRun>, StoreError>;
}
```

Add `+ ReportRepository` to `pub trait MetadataStore:` and to the blanket
`impl<T> MetadataStore for T where ...` bounds.

- [ ] **Step 5: Implement the SQLite backend**

In `crates/microwave-store/src/reports.rs`, define:

```rust
use crate::backend::{NewNodeRun, NodeRunFilter, ReportRepository};
use crate::compression::{decode_serialized, encode_serialized};
use crate::identity::{new_id, now};
use crate::sqlite::{SqliteStore, StoreError};
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredNodeRun {
    pub id: String,
    pub org_id: String,
    pub node_name: String,
    pub run_id: String,
    pub status: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub resource_count: i64,
    pub updated_resource_count: i64,
    pub error_count: i64,
    pub report: String,
    pub created_at: String,
}

impl SqliteStore {
    pub fn store_node_run(
        &self,
        org_id: &str,
        run: NewNodeRun<'_>,
        keep_runs: usize,
    ) -> Result<StoredNodeRun, StoreError> {
        let id = new_id();
        let created_at = now();
        let start_time = run.start_time.unwrap_or(created_at.as_str()).to_string();
        let report = encode_serialized(run.report, self.compression())?;
        let mut connection = self.connection();
        let tx = connection.transaction()?;

        tx.execute(
            "INSERT INTO node_runs
             (id, org_id, node_name, run_id, status, start_time, end_time,
              resource_count, updated_resource_count, error_count, report, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(org_id, node_name, run_id) DO UPDATE SET
                status = excluded.status,
                start_time = excluded.start_time,
                end_time = excluded.end_time,
                resource_count = excluded.resource_count,
                updated_resource_count = excluded.updated_resource_count,
                error_count = excluded.error_count,
                report = excluded.report",
            rusqlite::params![
                id,
                org_id,
                run.node_name,
                run.run_id,
                run.status,
                start_time,
                run.end_time,
                run.resource_count,
                run.updated_resource_count,
                run.error_count,
                report,
                created_at,
            ],
        )?;

        // Read the row before pruning: a retried run that is older than the
        // retained window is inserted (or refreshed) and then intentionally
        // pruned, but the caller still needs the stored value.
        let stored = tx.query_row(
            "SELECT id, org_id, node_name, run_id, status, start_time, end_time,
                    resource_count, updated_resource_count, error_count, report, created_at
             FROM node_runs WHERE org_id = ?1 AND node_name = ?2 AND run_id = ?3",
            rusqlite::params![org_id, run.node_name, run.run_id],
            |row| {
                Ok(StoredNodeRun {
                    id: row.get(0)?,
                    org_id: row.get(1)?,
                    node_name: row.get(2)?,
                    run_id: row.get(3)?,
                    status: row.get(4)?,
                    start_time: row.get(5)?,
                    end_time: row.get(6)?,
                    resource_count: row.get(7)?,
                    updated_resource_count: row.get(8)?,
                    error_count: row.get(9)?,
                    report: decode_serialized(&row.get::<_, String>(10)?)
                        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
                    created_at: row.get(11)?,
                })
            },
        )?;

        if keep_runs > 0 {
            tx.execute(
                "DELETE FROM node_runs
                 WHERE org_id = ?1 AND node_name = ?2
                   AND id NOT IN (
                     SELECT id FROM node_runs
                     WHERE org_id = ?1 AND node_name = ?2
                     ORDER BY start_time DESC, created_at DESC
                     LIMIT ?3
                   )",
                rusqlite::params![org_id, run.node_name, keep_runs as i64],
            )?;
        }

        tx.commit()?;
        Ok(stored)
    }

    pub fn list_node_runs(
        &self,
        org_id: &str,
        filter: &NodeRunFilter,
    ) -> Result<Vec<StoredNodeRun>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, org_id, node_name, run_id, status, start_time, end_time,
                    resource_count, updated_resource_count, error_count, report, created_at
             FROM node_runs
             WHERE org_id = ?1
               AND (?2 IS NULL OR node_name = ?2)
               AND (?3 IS NULL OR status = ?3)
               AND (?4 IS NULL OR start_time >= ?4)
               AND (?5 = 0 OR status <> 'success' OR error_count > 0)
             ORDER BY start_time DESC, created_at DESC
             LIMIT ?6",
        )?;
        let rows = statement.query_map(
            rusqlite::params![
                org_id,
                filter.node_name.as_deref(),
                filter.status.as_deref(),
                filter.since.as_deref(),
                filter.failures_only as i64,
                filter.limit as i64,
            ],
            |row| {
                Ok(StoredNodeRun {
                    id: row.get(0)?,
                    org_id: row.get(1)?,
                    node_name: row.get(2)?,
                    run_id: row.get(3)?,
                    status: row.get(4)?,
                    start_time: row.get(5)?,
                    end_time: row.get(6)?,
                    resource_count: row.get(7)?,
                    updated_resource_count: row.get(8)?,
                    error_count: row.get(9)?,
                    report: decode_serialized(&row.get::<_, String>(10)?)
                        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
                    created_at: row.get(11)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn find_node_run(
        &self,
        org_id: &str,
        node_name: &str,
        run_id: &str,
    ) -> Result<Option<StoredNodeRun>, StoreError> {
        let connection = self.connection();
        connection
            .query_row(
                "SELECT id, org_id, node_name, run_id, status, start_time, end_time,
                        resource_count, updated_resource_count, error_count, report, created_at
                 FROM node_runs WHERE org_id = ?1 AND node_name = ?2 AND run_id = ?3",
                rusqlite::params![org_id, node_name, run_id],
                |row| {
                    Ok(StoredNodeRun {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        node_name: row.get(2)?,
                        run_id: row.get(3)?,
                        status: row.get(4)?,
                        start_time: row.get(5)?,
                        end_time: row.get(6)?,
                        resource_count: row.get(7)?,
                        updated_resource_count: row.get(8)?,
                        error_count: row.get(9)?,
                        report: decode_serialized(&row.get::<_, String>(10)?)
                            .map_err(|error| {
                                rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                            })?,
                        created_at: row.get(11)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn latest_node_run(
        &self,
        org_id: &str,
        node_name: &str,
    ) -> Result<Option<StoredNodeRun>, StoreError> {
        Ok(self
            .list_node_runs(
                org_id,
                &NodeRunFilter {
                    node_name: Some(node_name.to_string()),
                    status: None,
                    since: None,
                    failures_only: false,
                    limit: 1,
                },
            )?
            .into_iter()
            .next())
    }
}

#[async_trait]
impl ReportRepository for SqliteStore {
    async fn store_node_run(
        &self,
        org_id: &str,
        run: NewNodeRun<'_>,
        keep_runs: usize,
    ) -> Result<StoredNodeRun, StoreError> {
        self.store_node_run(org_id, run, keep_runs)
    }

    async fn list_node_runs(
        &self,
        org_id: &str,
        filter: &NodeRunFilter,
    ) -> Result<Vec<StoredNodeRun>, StoreError> {
        self.list_node_runs(org_id, filter)
    }

    async fn find_node_run(
        &self,
        org_id: &str,
        node_name: &str,
        run_id: &str,
    ) -> Result<Option<StoredNodeRun>, StoreError> {
        self.find_node_run(org_id, node_name, run_id)
    }

    async fn latest_node_run(
        &self,
        org_id: &str,
        node_name: &str,
    ) -> Result<Option<StoredNodeRun>, StoreError> {
        self.latest_node_run(org_id, node_name)
    }
}
```

Add `use rusqlite::OptionalExtension;` to `reports.rs`. In
`crates/microwave-store/src/lib.rs`, add `mod reports;`, re-export
`NewNodeRun` and `NodeRunFilter` from `backend`, `ReportRepository` from
`backend`, and `StoredNodeRun` from `reports`:

```rust
pub use backend::{
    ..., NewNodeRun, NodeRunFilter, ReportRepository, ...,
};
pub use reports::StoredNodeRun;
```

In `crates/microwave-store/src/backend_sqlite.rs`, add:

```rust
#[async_trait]
impl crate::ReportRepository for SqliteStore {
    async fn store_node_run(
        &self,
        org_id: &str,
        run: crate::NewNodeRun<'_>,
        keep_runs: usize,
    ) -> Result<crate::StoredNodeRun, StoreError> {
        self.store_node_run(org_id, run, keep_runs)
    }
    // Delegate list_node_runs, find_node_run and latest_node_run in the same
    // style.
}
```

- [ ] **Step 6: Add the PostgreSQL compiling stub**

Task 2 adds `ReportRepository` to `MetadataStore`, so `PostgresStore` must
implement it or the workspace stops compiling. Create
`crates/microwave-store-postgres/src/reports.rs` with all four methods
returning `Err(StoreError::Invalid("report repository not implemented yet".into()))`,
using `#[async_trait]` and the same `NewNodeRun`/`NodeRunFilter`/`StoredNodeRun`
types. Add `mod reports;` to `crates/microwave-store-postgres/src/lib.rs`.
Task 3 replaces this stub with the real implementation.

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 7: Add the snapshot-exclusion test and run the suite**

Add to `crates/microwave-store/tests/migration_security_test.rs`:

```rust
#[test]
fn node_runs_are_not_part_of_logical_snapshots() {
    let store = microwave_store::SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("snapshot-org", "Snapshot").unwrap();
    store
        .store_node_run(
            &org.id,
            microwave_store::NewNodeRun {
                node_name: "web1",
                run_id: "run-1",
                status: "success",
                start_time: Some("2026-10-27T10:00:00.000Z"),
                end_time: None,
                resource_count: 1,
                updated_resource_count: 1,
                error_count: 0,
                report: r#"{"message_type":"run_converge","run_id":"run-1"}"#,
            },
            10,
        )
        .unwrap();
    let snapshot = store.export_snapshot().unwrap();
    let serialized = serde_json::to_string(&snapshot).unwrap();
    assert!(!serialized.contains("node_runs"));
    assert!(!serialized.contains("run-1"));
}
```

Run: `cargo test -p microwave-store --lib reports`
Expected: PASS.

Run: `cargo test -p microwave-store --test migration_security_test node_runs_are_not_part_of_logical_snapshots`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/microwave-store/migrations/0001_initial.sql \
  crates/microwave-store/src/backend.rs \
  crates/microwave-store/src/backend_sqlite.rs \
  crates/microwave-store/src/lib.rs \
  crates/microwave-store/src/reports.rs \
  crates/microwave-store/tests/migration_security_test.rs \
  crates/microwave-store-postgres/src/lib.rs \
  crates/microwave-store-postgres/src/reports.rs
git commit -m "feat(store): add report repository for SQLite"
```

---

### Task 3: PostgreSQL backend and dual-backend parity

**Files:**
- Modify: `crates/microwave-store-postgres/migrations/0001_initial.sql`
- Modify: `crates/microwave-store-postgres/src/reports.rs`
- Modify: `crates/microwave-store-postgres/src/lib.rs`
- Test: `crates/microwave-store/tests/dual_backend.rs`

**Interfaces:**
- Consumes: `NewNodeRun`, `NodeRunFilter`, `StoredNodeRun`, `ReportRepository` from Task 2.
- Produces: `impl ReportRepository for PostgresStore`.
- Produces: `suite::reports(&Arc<dyn MetadataStore>)` in the matrix.

- [ ] **Step 1: Add the PostgreSQL migration**

Append to `crates/microwave-store-postgres/migrations/0001_initial.sql`:

```sql
CREATE TABLE IF NOT EXISTS node_runs (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    node_name TEXT NOT NULL,
    run_id TEXT NOT NULL,
    status TEXT NOT NULL,
    start_time TEXT NOT NULL,
    end_time TEXT,
    resource_count BIGINT NOT NULL DEFAULT 0,
    updated_resource_count BIGINT NOT NULL DEFAULT 0,
    error_count BIGINT NOT NULL DEFAULT 0,
    report TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (org_id, node_name, run_id),
    FOREIGN KEY (org_id) REFERENCES orgs (id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS node_runs_org_node_time_index
    ON node_runs (org_id, node_name, start_time DESC);
CREATE INDEX IF NOT EXISTS node_runs_org_time_index
    ON node_runs (org_id, start_time DESC);
```

- [ ] **Step 2: Add the parity suite**

In `crates/microwave-store/tests/dual_backend.rs`, add
`NewNodeRun`, `NodeRunFilter` to the `use microwave_store::{...}` list, add
`reports` to both `matrix!` lists, and add inside `mod suite`:

```rust
pub async fn reports(store: &Arc<dyn MetadataStore>) -> Vec<String> {
    let fixture = fixture(store).await;
    let org_id = &fixture.org.id;

    let stored = store
        .store_node_run(
            org_id,
            NewNodeRun {
                node_name: "web1",
                run_id: "run-1",
                status: "success",
                start_time: Some("2026-10-27T10:00:00.000Z"),
                end_time: Some("2026-10-27T10:01:00.000Z"),
                resource_count: 3,
                updated_resource_count: 2,
                error_count: 0,
                report: r#"{"message_type":"run_converge","run_id":"run-1"}"#,
            },
            10,
        )
        .await
        .expect("store report");
    assert_eq!(stored.node_name, "web1");
    assert_eq!(stored.start_time, "2026-10-27T10:00:00.000Z");
    assert_eq!(stored.report, r#"{"message_type":"run_converge","run_id":"run-1"}"#);

    let retry = store
        .store_node_run(
            org_id,
            NewNodeRun {
                node_name: "web1",
                run_id: "run-1",
                status: "failure",
                start_time: Some("2026-10-27T10:00:00.000Z"),
                end_time: Some("2026-10-27T10:02:00.000Z"),
                resource_count: 3,
                updated_resource_count: 1,
                error_count: 1,
                report: r#"{"message_type":"run_converge","run_id":"retry"}"#,
            },
            10,
        )
        .await
        .expect("retry report");
    assert_eq!(retry.id, stored.id);
    assert_eq!(retry.status, "failure");

    let failures = store
        .list_node_runs(
            org_id,
            &NodeRunFilter {
                node_name: Some("web1".into()),
                status: None,
                since: None,
                failures_only: true,
                limit: 10,
            },
        )
        .await
        .expect("list failures");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].run_id, "run-1");

    let since = store
        .list_node_runs(
            org_id,
            &NodeRunFilter {
                node_name: None,
                status: None,
                since: Some("2026-10-27T09:30:00.000Z".into()),
                failures_only: false,
                limit: 10,
            },
        )
        .await
        .expect("list since");
    assert_eq!(since.len(), 1);

    assert_eq!(
        store
            .find_node_run(org_id, "web1", "run-1")
            .await
            .expect("find report")
            .map(|run| run.id),
        Some(stored.id.clone())
    );
    assert_eq!(
        store
            .latest_node_run(org_id, "web1")
            .await
            .expect("latest report")
            .map(|run| run.run_id),
        Some("run-1".to_string())
    );

    for (run_id, start) in [
        ("run-2", "2026-10-27T11:00:00.000Z"),
        ("run-3", "2026-10-27T12:00:00.000Z"),
    ] {
        store
            .store_node_run(
                org_id,
                NewNodeRun {
                    node_name: "web1",
                    run_id,
                    status: "success",
                    start_time: Some(start),
                    end_time: None,
                    resource_count: 1,
                    updated_resource_count: 1,
                    error_count: 0,
                    report: r#"{"message_type":"run_converge"}"#,
                },
                2,
            )
            .await
            .expect("store pruned report");
    }
    let kept = store
        .list_node_runs(
            org_id,
            &NodeRunFilter {
                node_name: Some("web1".into()),
                status: None,
                since: None,
                failures_only: false,
                limit: 10,
            },
        )
        .await
        .expect("list kept");
    assert_eq!(
        kept.iter().map(|run| run.run_id.as_str()).collect::<Vec<_>>(),
        vec!["run-3", "run-2"]
    );

    let other = store
        .create_org(&unique("reports-other"), "Other")
        .await
        .expect("create other org");
    assert!(store
        .list_node_runs(
            &other.id,
            &NodeRunFilter {
                node_name: None,
                status: None,
                since: None,
                failures_only: false,
                limit: 10,
            },
        )
        .await
        .expect("list other org")
        .is_empty());

    cleanup_org(store.as_ref(), &fixture.org.id).await;
    cleanup_org(store.as_ref(), &other.id).await;
    Vec::new()
}
```

- [ ] **Step 3: Run the suite to verify it fails**

Run: `cargo test -p microwave-store --test dual_backend reports`
Expected: FAIL to compile or PostgreSQL test fails because the impl is missing.

- [ ] **Step 4: Implement the PostgreSQL backend**

Replace the stub in `crates/microwave-store-postgres/src/reports.rs` while
keeping the SQLite semantics exactly:

- `store_node_run`: generate `new_id()`/`now()`, compress with
  `encode_serialized(run.report, self.compression())`, `INSERT ... ON CONFLICT
  (org_id, node_name, run_id) DO UPDATE SET ...` in a transaction, select and
  decode the row, then prune with `DELETE ... WHERE id NOT IN (SELECT id ...
  ORDER BY start_time DESC, created_at DESC LIMIT $3)` when `keep_runs > 0`.
- `list_node_runs`: one query with the same filters:
  `($2::text IS NULL OR node_name = $2)`, `($3::text IS NULL OR status = $3)`,
  `($4::text IS NULL OR start_time >= $4)`,
  `(NOT $5 OR status <> 'success' OR error_count > 0)`,
  `ORDER BY start_time DESC, created_at DESC LIMIT $6`.
- `find_node_run` and `latest_node_run`: same as SQLite.
- Map rows with a `run_from_row` helper and `decode_serialized`.
- Map SQL errors with `crate::store::db`.

Add any required re-exports in
`crates/microwave-store-postgres/src/lib.rs` (the `mod reports;` declaration
already exists from Task 2).

- [ ] **Step 5: Run the parity suite**

Run: `cargo test -p microwave-store --test dual_backend reports`
Expected: the SQLite half passes; PostgreSQL prints the skip message when
`MICROWAVE_TEST_POSTGRES_URL` is unset.

Then run: `scripts/run-postgres-tests.sh -p microwave-store --test dual_backend reports`
Expected: PASS against the disposable PostgreSQL container.

- [ ] **Step 6: Commit**

```bash
git add crates/microwave-store-postgres/migrations/0001_initial.sql \
  crates/microwave-store-postgres/src/lib.rs \
  crates/microwave-store-postgres/src/reports.rs \
  crates/microwave-store/tests/dual_backend.rs
git commit -m "feat(store-postgres): add report repository and parity suite"
```

---

### Task 4: `run_converge` payload parser

**Files:**
- Create: `crates/microwave-api/src/report_ingest.rs`
- Modify: `crates/microwave-api/src/lib.rs`
- Test: `crates/microwave-api/src/report_ingest.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub(crate) struct PreparedNodeRun { node_name, run_id, status, start_time, end_time, resource_count, updated_resource_count, error_count, report }`.
- Produces: `pub(crate) fn prepare_node_run(body: &[u8]) -> Result<Option<PreparedNodeRun>, ApiError>`.
- Consumes: `ApiError` from the API crate.

- [ ] **Step 1: Write the failing parser tests**

At the bottom of `crates/microwave-api/src/report_ingest.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &[u8] = br#"{
      "message_type":"run_converge",
      "node_name":"web1",
      "run_id":"abc",
      "status":"failure",
      "start_time":"2026-10-27T10:00:00+02:00",
      "end_time":"2026-10-27T10:05:00+02:00",
      "total_resource_count":4,
      "updated_resource_count":2,
      "error":{"class":"RuntimeError","message":"boom"},
      "unknown_field":42,
      "resources":[
        {"status":"updated","error":null},
        {"status":"failed","error":{"class":"Errno::ENOENT","message":"missing"}}
      ]
    }"#;

    #[test]
    fn parses_a_converged_run() {
        let run = prepare_node_run(FULL).unwrap().unwrap();
        assert_eq!(run.node_name, "web1");
        assert_eq!(run.run_id, "abc");
        assert_eq!(run.status, "failure");
        assert_eq!(run.start_time, "2026-10-27T08:00:00.000Z");
        assert_eq!(run.end_time.as_deref(), Some("2026-10-27T08:05:00.000Z"));
        assert_eq!(run.resource_count, 4);
        assert_eq!(run.updated_resource_count, 2);
        assert_eq!(run.error_count, 2);
        assert!(run.report.contains("\"unknown_field\":42"));
    }

    #[test]
    fn ignores_non_converge_and_missing_message_types() {
        assert!(prepare_node_run(br#"{"message_type":"run_start"}"#).unwrap().is_none());
        assert!(prepare_node_run(br#"{"message_type":"action"}"#).unwrap().is_none());
        assert!(prepare_node_run(br#"{"node_name":"web1"}"#).unwrap().is_none());
    }

    #[test]
    fn derives_counts_and_unknown_status() {
        let run = prepare_node_run(
            br#"{"message_type":"run_converge","node_name":"web1","run_id":"1",
                 "resources":[{"status":"updated"},{"status":"updated"},{"status":"skipped"}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(run.status, "unknown");
        assert_eq!(run.resource_count, 3);
        assert_eq!(run.updated_resource_count, 2);
        assert_eq!(run.error_count, 0);
    }

    #[test]
    fn falls_back_to_receive_time_for_missing_or_invalid_start() {
        for body in [
            br#"{"message_type":"run_converge","node_name":"web1","run_id":"1"}"#.as_slice(),
            br#"{"message_type":"run_converge","node_name":"web1","run_id":"1","start_time":"nope"}"#
                .as_slice(),
        ] {
            let run = prepare_node_run(body).unwrap().unwrap();
            assert!(chrono::DateTime::parse_from_rfc3339(&run.start_time).is_ok());
            assert!(run.start_time.ends_with('Z'));
            assert_eq!(run.start_time.len(), 24);
        }
    }

    #[test]
    fn normalizes_an_invalid_end_time_to_none() {
        let run = prepare_node_run(
            br#"{"message_type":"run_converge","node_name":"web1","run_id":"1","end_time":"nope"}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(run.end_time, None);
    }

    #[test]
    fn rejects_missing_identity_and_invalid_json() {
        for body in [
            br#"{"message_type":"run_converge","run_id":"1"}"#.as_slice(),
            br#"{"message_type":"run_converge","node_name":"web1"}"#.as_slice(),
            br#"{"message_type":"run_converge","node_name":"","run_id":"1"}"#.as_slice(),
            b"not-json".as_slice(),
        ] {
            assert!(prepare_node_run(body).is_err(), "body should be rejected");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --lib report_ingest`
Expected: FAIL to compile (`prepare_node_run` missing).

- [ ] **Step 3: Implement the parser**

Create `crates/microwave-api/src/report_ingest.rs`:

```rust
//! Extraction of Chef `run_converge` payloads for local report storage.

use crate::ApiError;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;

pub(crate) struct PreparedNodeRun {
    pub node_name: String,
    pub run_id: String,
    pub status: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub resource_count: i64,
    pub updated_resource_count: i64,
    pub error_count: i64,
    pub report: String,
}

pub(crate) fn prepare_node_run(body: &[u8]) -> Result<Option<PreparedNodeRun>, ApiError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| ApiError::bad_request("Invalid JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| ApiError::bad_request("Incorrect JSON type for request body"))?;
    if object.get("message_type").and_then(Value::as_str) != Some("run_converge") {
        return Ok(None);
    }

    let node_name = required_string(object.get("node_name"), "node_name")?;
    let run_id = required_string(object.get("run_id"), "run_id")?;
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string();

    let start_time = normalize_timestamp(object.get("start_time"))
        .unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true));
    let end_time = normalize_timestamp(object.get("end_time"));

    let resources = object.get("resources").and_then(Value::as_array);
    let resource_count = integer(object.get("total_resource_count"))
        .or_else(|| resources.map(|items| items.len() as i64))
        .unwrap_or(0)
        .max(0);
    let updated_resource_count = integer(object.get("updated_resource_count"))
        .or_else(|| {
            resources.map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("status").and_then(Value::as_str) == Some("updated"))
                    .count() as i64
            })
        })
        .unwrap_or(0)
        .max(0);
    let error_count = (object.get("error").is_some_and(|value| !value.is_null()) as i64)
        + resources
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("error").is_some_and(|value| !value.is_null()))
                    .count() as i64
            })
            .unwrap_or(0);

    let report = std::str::from_utf8(body)
        .map_err(|_| ApiError::bad_request("Invalid JSON"))?
        .to_string();

    Ok(Some(PreparedNodeRun {
        node_name,
        run_id,
        status,
        start_time,
        end_time,
        resource_count,
        updated_resource_count,
        error_count,
        report,
    }))
}

fn required_string(value: Option<&Value>, field: &str) -> Result<String, ApiError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| ApiError::bad_request(format!("Field '{field}' missing")))
}

fn integer(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

fn normalize_timestamp(value: Option<&Value>) -> Option<String> {
    let parsed = DateTime::parse_from_rfc3339(value?.as_str()?).ok()?;
    Some(
        parsed
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true),
    )
}
```

Add `mod report_ingest;` to `crates/microwave-api/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p microwave-api --lib report_ingest`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/microwave-api/src/lib.rs crates/microwave-api/src/report_ingest.rs
git commit -m "feat(api): parse run_converge payloads for storage"
```

---

### Task 5: Data-collector local branch and routing

**Files:**
- Modify: `crates/microwave-api/src/data_collector.rs`
- Modify: `crates/microwave-api/src/middleware.rs:711`
- Modify: `crates/microwave-api/src/lib.rs` (route merge)
- Test: `crates/microwave-api/tests/data_collector_test.rs`

**Interfaces:**
- Consumes: `DataCollectorConfig::is_enabled()`, `stores_locally()`, `forwards()`, `keep_runs` from Task 1.
- Consumes: `ReportRepository::store_node_run`, `NewNodeRun` from Task 2.
- Consumes: `prepare_node_run` from Task 4.

- [ ] **Step 1: Write the failing API tests**

In `crates/microwave-api/tests/data_collector_test.rs`, add:

```rust
use microwave_config::DataCollectorStore;
use microwave_store::NodeRunFilter;

const RUN_CONVERGE: &[u8] = br#"{"message_type":"run_converge","node_name":"web1","run_id":"run-1","status":"failure","start_time":"2026-10-27T10:00:00+02:00","end_time":"2026-10-27T10:05:00+02:00","total_resource_count":4,"updated_resource_count":2,"error":{"class":"RuntimeError","message":"boom"},"resources":[{"status":"updated","error":null},{"status":"failed","error":{"class":"Errno::ENOENT","message":"missing"}}]}"#;

fn local_config(token: Option<&str>, keep_runs: usize) -> Config {
    let mut config = Config::default();
    config.data_collector.store = DataCollectorStore::Local;
    config.data_collector.token = token.map(ToString::to_string);
    config.data_collector.keep_runs = keep_runs;
    config
}

fn token_request_owned(path: &str, token: &str, body: Vec<u8>) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .header("x-data-collector-token", token)
        .header("x-data-collector-auth", "version=1.0")
        .body(Body::from(body))
        .unwrap()
}

fn converge_body(run_id: &str, start_time: &str) -> Vec<u8> {
    json!({
        "message_type": "run_converge",
        "node_name": "web1",
        "run_id": run_id,
        "status": "success",
        "start_time": start_time
    })
    .to_string()
    .into_bytes()
}

async fn runs(store: &Arc<dyn microwave_store::MetadataStore>, org_id: &str) -> Vec<microwave_store::StoredNodeRun> {
    store
        .list_node_runs(
            org_id,
            &NodeRunFilter {
                node_name: None,
                status: None,
                since: None,
                failures_only: false,
                limit: 100,
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn local_storage_stores_without_forwarding() {
    let state = support::state_with_config(local_config(Some("server-token"), 50));
    let org = support::add_org(&state, "acme").await;
    let store = state.store.clone();
    let app = support::app(state);

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "server-token",
            RUN_CONVERGE,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await, b"{}".to_vec());

    let runs = runs(&store, &org.id).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].start_time, "2026-10-27T08:00:00.000Z");
    assert_eq!(runs[0].error_count, 2);
}

#[tokio::test]
async fn both_stores_and_forwards() {
    let (root_url, captured) = start_mock(202, r#"{"status":"accepted"}"#, Duration::ZERO).await;
    let mut config = data_collector_config(&root_url, Some("server-token"), 5_000);
    config.data_collector.store = DataCollectorStore::Both;
    let state = support::state_with_config(config);
    let org = support::add_org(&state, "acme").await;
    let store = state.store.clone();
    let app = support::app(state);

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "server-token",
            RUN_CONVERGE,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(captured.lock().await.len(), 1);
    assert_eq!(runs(&store, &org.id).await.len(), 1);
}

#[tokio::test]
async fn both_keeps_the_row_when_forwarding_fails() {
    let (root_url, _) = start_mock(502, r#"{"error":"down"}"#, Duration::ZERO).await;
    let mut config = data_collector_config(&root_url, Some("server-token"), 5_000);
    config.data_collector.store = DataCollectorStore::Both;
    let state = support::state_with_config(config);
    let org = support::add_org(&state, "acme").await;
    let store = state.store.clone();
    let app = support::app(state);

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "server-token",
            RUN_CONVERGE,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(runs(&store, &org.id).await.len(), 1);
}

#[tokio::test]
async fn local_storage_rejects_unknown_organizations() {
    let state = support::state_with_config(local_config(Some("server-token"), 50));
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let response = app
        .oneshot(token_request(
            "/organizations/missing/data-collector",
            "server-token",
            RUN_CONVERGE,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn local_storage_ignores_run_start_and_action_messages() {
    let state = support::state_with_config(local_config(Some("server-token"), 50));
    let org = support::add_org(&state, "acme").await;
    let store = state.store.clone();
    let app = support::app(state);

    for body in [
        br#"{"message_type":"run_start","node_name":"web1","run_id":"run-1"}"#.as_slice(),
        br#"{"message_type":"action","node_name":"web1"}"#.as_slice(),
    ] {
        let response = app
            .clone()
            .oneshot(token_request_owned(
                "/organizations/acme/data-collector",
                "server-token",
                body.to_vec(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert!(runs(&store, &org.id).await.is_empty());
}

#[tokio::test]
async fn local_storage_rejects_invalid_json_and_missing_run_id() {
    let state = support::state_with_config(local_config(Some("server-token"), 50));
    let org = support::add_org(&state, "acme").await;
    let store = state.store.clone();
    let app = support::app(state);

    for body in [
        b"not-json".to_vec(),
        br#"{"message_type":"run_converge","node_name":"web1"}"#.to_vec(),
    ] {
        let response = app
            .clone()
            .oneshot(token_request_owned(
                "/organizations/acme/data-collector",
                "server-token",
                body,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(runs(&store, &org.id).await.is_empty());
}

#[tokio::test]
async fn local_storage_prunes_to_keep_runs() {
    let state = support::state_with_config(local_config(Some("server-token"), 2));
    let org = support::add_org(&state, "acme").await;
    let store = state.store.clone();
    let app = support::app(state);

    for (run_id, start) in [
        ("run-1", "2026-10-27T10:00:00Z"),
        ("run-2", "2026-10-27T11:00:00Z"),
        ("run-3", "2026-10-27T12:00:00Z"),
    ] {
        let response = app
            .clone()
            .oneshot(token_request_owned(
                "/organizations/acme/data-collector",
                "server-token",
                converge_body(run_id, start),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let runs = runs(&store, &org.id).await;
    assert_eq!(
        runs.iter().map(|run| run.run_id.as_str()).collect::<Vec<_>>(),
        vec!["run-3", "run-2"]
    );
}

#[tokio::test]
async fn local_storage_accepts_signed_requests() {
    let state = support::state_with_config(local_config(None, 50));
    let org = support::add_org(&state, "acme").await;
    let store = state.store.clone();
    let app = support::app(state);

    let response = app
        .oneshot(signed_post(
            "/organizations/acme/data-collector",
            RUN_CONVERGE,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(runs(&store, &org.id).await.len(), 1);
}

#[tokio::test]
async fn local_storage_still_rejects_a_wrong_token() {
    let state = support::state_with_config(local_config(Some("server-token"), 50));
    let org = support::add_org(&state, "acme").await;
    let store = state.store.clone();
    let app = support::app(state);

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "wrong-token",
            RUN_CONVERGE,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(runs(&store, &org.id).await.is_empty());
}
```

Import `Arc` is already present in the test file. Add `StoredNodeRun` to the
`microwave_store` import or use the fully qualified path shown above.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p microwave-api --test data_collector_test local_storage`
Expected: FAIL because local storage is not implemented and the route may still
be absent when `root_url` is unset.

- [ ] **Step 3: Implement the handler**

In `crates/microwave-api/src/data_collector.rs`:

1. Keep `configured_token` and `has_valid_token` unchanged.
2. Rename the existing forwarding body to
   `async fn forward_upstream(state: &AppState, headers: HeaderMap, body: Bytes) -> Response`.
3. Replace `forward` with:

```rust
async fn forward(
    State(state): State<AppState>,
    Path(organization): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let collector = &state.config.data_collector;
    let storing = collector.stores_locally();
    let forwarding = collector.is_configured() && collector.forwards();

    if storing {
        let org = match state.store.find_org(&organization).await {
            Ok(Some(org)) => org,
            Ok(None) => return ApiError::not_found("not found").into_response(),
            Err(error) => {
                tracing::warn!(%error, org = %organization, "failed to resolve data collector organization");
                return ApiError::internal().into_response();
            }
        };

        match crate::report_ingest::prepare_node_run(&body) {
            Ok(Some(run)) => {
                let new_run = microwave_store::NewNodeRun {
                    node_name: &run.node_name,
                    run_id: &run.run_id,
                    status: &run.status,
                    start_time: Some(&run.start_time),
                    end_time: run.end_time.as_deref(),
                    resource_count: run.resource_count,
                    updated_resource_count: run.updated_resource_count,
                    error_count: run.error_count,
                    report: &run.report,
                };
                if let Err(error) = state
                    .store
                    .store_node_run(&org.id, new_run, collector.keep_runs)
                    .await
                {
                    tracing::warn!(%error, org = %org.name, "failed to store node run");
                    return ApiError::internal().into_response();
                }
            }
            Ok(None) => {}
            Err(error) => return error.into_response(),
        }
    }

    if forwarding {
        forward_upstream(&state, headers, body).await
    } else {
        (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response()
    }
}
```

`forward_upstream` keeps the existing logic, taking `&AppState` instead of
`State<AppState>` and borrowing the headers/body.

- [ ] **Step 4: Wire the route and middleware**

In `crates/microwave-api/src/middleware.rs`, change the data-collector branch's
`state.config.data_collector.is_configured()` to
`state.config.data_collector.is_enabled()` (only the local variable at the start
of the `is_data_collector_path` branch).

In `crates/microwave-api/src/lib.rs`, change the route merge condition:

```rust
if state.config.data_collector.is_enabled() {
    inner = inner.merge(data_collector::routes());
}
```

- [ ] **Step 5: Run the data-collector tests**

Run: `cargo test -p microwave-api --test data_collector_test`
Expected: all old and new tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/microwave-api/src/data_collector.rs \
  crates/microwave-api/src/lib.rs \
  crates/microwave-api/src/middleware.rs \
  crates/microwave-api/tests/data_collector_test.rs
git commit -m "feat(api): store run reports in the data collector handler"
```

---

### Task 6: Documentation, example config, and full verification

**Files:**
- Modify: `packaging/microwave.toml.example`
- Modify: `docs/deployment.md`
- Test: workspace commands

**Interfaces:**
- Consumes: all previous tasks.

- [ ] **Step 1: Document the new configuration**

In `packaging/microwave.toml.example`, extend the `[data_collector]` block:

```toml
# Where run reports go:
#   forward (default) - forward only, no local storage
#   local             - store in the metadata database, do not forward
#   both              - store locally and forward
# `local` enables the route even when root_url is unset.
store = "forward"
# Keep this many reports per node; 0 keeps everything. Old reports are pruned
# after each successful insert.
keep_runs = 50
```

In `docs/deployment.md`, add a short paragraph under the data-collector/
Automate section:

```markdown
`data_collector.store` can be `forward` (default), `local`, or `both`.
`local`/`both` persist Chef `run_converge` reports in the metadata database,
one row per `(organization, node, run)` with 50 runs kept per node by default.
The stable database grows with report volume; size `keep_runs` accordingly.
Reports are not included in logical snapshots or `microwave-migrate`.
```

- [ ] **Step 2: Run the full verification**

Run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all commands pass. If PostgreSQL is available, additionally run:

```bash
scripts/run-postgres-tests.sh -p microwave-store --test dual_backend reports
```

- [ ] **Step 3: Commit**

```bash
git add packaging/microwave.toml.example docs/deployment.md
git commit -m "docs: document local run report storage"
```

---

## Plan self-review

- **Spec coverage:** config modes/retention (Task 1), schema and both backends
  (Tasks 2–3), `run_converge` extraction and normalization (Task 4), ingestion
  ordering, org resolution, auth behavior, pruning, and error mapping (Task 5),
  operator docs (Task 6). The MCP surfaces are deliberately outside this plan
  and get their own plans after Phase 1 lands.
- **Review Focus tests:** `keep_runs = 0` (Task 2), idempotent retry (Tasks 2–3),
  `both` with a failing upstream (Task 5), unknown org (Task 5), missing/invalid
  `start_time` (Task 4).
- **Type consistency:** `NewNodeRun`, `NodeRunFilter`, `StoredNodeRun`,
  `prepare_node_run`, `is_enabled`, `stores_locally`, `forwards`, and `keep_runs`
  are used with the same names in every task.
- **Out of scope, intentionally:** MCP auth/tools, cookbook writes, snapshot
  export, and orphaned blob cleanup.
