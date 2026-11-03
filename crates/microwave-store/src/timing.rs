//! Timing helpers for hot metadata-store operations.
//!
//! A [`StoreTimer`] is a drop guard: it records `ok` when the instrumented
//! method returns normally and `error` when the caller calls [`StoreTimer::failed`].
//! `StoreTimer::start(None, …)` keeps the instrumentation nearly free while the
//! server runs without a metrics registry.

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
    fn as_str(self) -> &'static str {
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
        }
    }

    /// Record the operation as failed instead of successful.
    pub fn failed(&mut self) {
        self.outcome = "error";
    }
}

impl Drop for StoreTimer {
    fn drop(&mut self) {
        if let Some(metrics) = &self.metrics {
            metrics.record_store_operation(
                self.system.as_str(),
                self.operation,
                self.outcome,
                self.started.elapsed(),
            );
        }
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
            let _timer = StoreTimer::start(Some(metrics.clone()), DbSystem::Sqlite, "create_node");
        }
        let entry = &metrics.store_series()[0];
        assert_eq!(entry.operation, "create_node");
        assert_eq!(entry.outcome, "ok");
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
