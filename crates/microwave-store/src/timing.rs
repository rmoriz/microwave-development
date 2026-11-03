//! Timing helpers for hot metadata-store operations.
//!
//! A [`StoreTimer`] is a drop guard: [`StoreTimer::finish`] records `ok` or
//! `error` for the result it is handed, and a guard dropped without finishing
//! records `cancelled` — the caller dropped the future mid-flight.
//! `StoreTimer::start(None, …)` keeps the instrumentation nearly free while the
//! server runs without a metrics registry.

use crate::sqlite::StoreError;
use microwave_metrics::Metrics;
use std::sync::Arc;
use std::time::Instant;

/// The database system a store timer labels its samples with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbSystem {
    Sqlite,
    Postgres,
}

impl DbSystem {
    /// Label used for the `db.system` dimension.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
        }
    }
}

/// Measures one store operation and records it when dropped.
///
/// The guard holds no lock and only an `Arc`, so it stays `Send` across the
/// `.await` points of the instrumented methods. Without a metrics handle the
/// drop is a single `Option` check.
pub struct StoreTimer {
    metrics: Option<Arc<Metrics>>,
    system: DbSystem,
    operation: &'static str,
    started: Instant,
    outcome: &'static str,
    finished: bool,
}

impl StoreTimer {
    /// Start timing `operation` against `system`.
    pub fn start(metrics: Option<Arc<Metrics>>, system: DbSystem, operation: &'static str) -> Self {
        Self {
            metrics,
            system,
            operation,
            started: Instant::now(),
            outcome: "ok",
            finished: false,
        }
    }

    /// Record the operation as failed instead of successful and finish the
    /// timer, so the drop records `error`.
    pub fn failed(&mut self) {
        self.outcome = "error";
        self.finished = true;
    }

    /// Finish the operation: record `error` when `result` is an `Err`, then
    /// drop the guard and hand the result back unchanged.
    ///
    /// This is the one-line form of the `match` every instrumented method used
    /// to spell out, so an error arm cannot forget [`Self::failed`].
    pub fn finish<T>(mut self, result: Result<T, StoreError>) -> Result<T, StoreError> {
        self.finished = true;
        if result.is_err() {
            self.outcome = "error";
        }
        result
    }
}

impl Drop for StoreTimer {
    fn drop(&mut self) {
        let Some(metrics) = &self.metrics else {
            return;
        };
        let outcome = if self.finished {
            self.outcome
        } else {
            "cancelled"
        };
        metrics.record_store_operation(
            self.system.as_str(),
            self.operation,
            outcome,
            self.started.elapsed(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;
    use microwave_metrics::Metrics;
    use std::sync::Arc;

    #[test]
    fn a_finished_timer_records_the_operation() {
        let metrics = Arc::new(Metrics::new());
        {
            let timer = StoreTimer::start(Some(metrics.clone()), DbSystem::Sqlite, "create_node");
            timer.finish(Ok(())).unwrap();
        }
        let entry = &metrics.store_series()[0];
        assert_eq!(entry.operation, "create_node");
        assert_eq!(entry.outcome, "ok");
    }

    #[test]
    fn a_timer_dropped_before_finish_records_cancelled() {
        let metrics = Arc::new(Metrics::new());
        {
            // No `finish`: the caller dropped the future mid-flight.
            let _timer = StoreTimer::start(Some(metrics.clone()), DbSystem::Postgres, "find_node");
        }
        assert_eq!(metrics.store_series()[0].outcome, "cancelled");
    }

    #[test]
    fn a_failed_timer_records_the_error_outcome() {
        let metrics = Arc::new(Metrics::new());
        {
            let mut timer =
                StoreTimer::start(Some(metrics.clone()), DbSystem::Postgres, "find_node");
            timer.failed();
        }
        let entry = &metrics.store_series()[0];
        assert_eq!(entry.db_system, "postgres");
        assert_eq!(entry.operation, "find_node");
        assert_eq!(entry.outcome, "error");
    }

    #[test]
    fn the_guard_stays_send_across_await_points() {
        fn assert_send<T: Send>() {}
        assert_send::<StoreTimer>();
    }

    #[test]
    fn a_timer_without_metrics_is_a_no_op() {
        // No handle, so there is nothing to inspect: the guard must simply drop
        // without panicking, in both outcomes.
        drop(StoreTimer::start(None, DbSystem::Sqlite, "list_nodes"));
        let mut timer = StoreTimer::start(None, DbSystem::Sqlite, "delete_node");
        timer.failed();
    }

    /// Store with one org, a `nodes` container and an actor to attribute the
    /// created nodes to.
    fn fixture() -> (SqliteStore, String, String) {
        let store = SqliteStore::open_in_memory().unwrap();
        let org = store.create_org("acme", "Acme").unwrap();
        store
            .create_container_for_org_with_acl(&org.id, "nodes", &org.authz_id)
            .unwrap();
        (store, org.id, org.authz_id)
    }

    fn node(creator_authz_id: &str) -> crate::NewNode<'_> {
        crate::NewNode {
            name: "web1",
            body: r#"{"name":"web1"}"#,
            environment: "_default",
            policy_name: None,
            policy_group: None,
            creator_authz_id,
        }
    }

    #[test]
    fn an_attached_store_times_the_operation() {
        let metrics = Arc::new(Metrics::new());
        let (store, org_id, actor) = fixture();
        store.set_metrics(metrics.clone());
        store.create_node(&org_id, node(&actor)).unwrap();
        let entry = &metrics.store_series()[0];
        assert_eq!(entry.db_system, "sqlite");
        assert_eq!(entry.operation, "create_node");
        assert_eq!(entry.outcome, "ok");
    }

    #[test]
    fn a_missing_node_is_a_successful_find() {
        let metrics = Arc::new(Metrics::new());
        let (store, org_id, _actor) = fixture();
        store.set_metrics(metrics.clone());
        assert_eq!(store.find_node(&org_id, "absent").unwrap(), None);
        let entry = &metrics.store_series()[0];
        assert_eq!(entry.operation, "find_node");
        assert_eq!(entry.outcome, "ok");
    }

    #[test]
    fn deleting_a_missing_node_is_an_error() {
        let metrics = Arc::new(Metrics::new());
        let (store, org_id, _actor) = fixture();
        store.set_metrics(metrics.clone());
        assert!(store.delete_node(&org_id, "absent").is_err());
        let entry = &metrics.store_series()[0];
        assert_eq!(entry.operation, "delete_node");
        assert_eq!(entry.outcome, "error");
    }

    #[test]
    fn an_unattached_store_runs_without_recording() {
        // The server builds the store before the registry, so the very first
        // operations run with no handle attached. They must work unchanged and
        // must not panic on the missing guard payload.
        let (store, org_id, actor) = fixture();
        store.create_node(&org_id, node(&actor)).unwrap();
        assert_eq!(store.list_nodes(&org_id).unwrap().len(), 1);
        assert_eq!(store.count_nodes().unwrap(), 1);
    }

    #[test]
    fn an_attached_store_times_a_failing_operation() {
        let metrics = Arc::new(Metrics::new());
        let (store, org_id, actor) = fixture();
        store.set_metrics(metrics.clone());
        store.create_node(&org_id, node(&actor)).unwrap();
        store.create_node(&org_id, node(&actor)).unwrap_err();
        let series = metrics.store_series();
        // The outcome is part of the series key, so the failed call gets its own
        // entry instead of being folded into the successful one.
        assert_eq!(series.len(), 2);
        assert!(series.iter().all(|entry| entry.operation == "create_node"));
        let error = series
            .iter()
            .find(|entry| entry.outcome == "error")
            .expect("error series");
        assert_eq!(error.count, 1);
    }
}
