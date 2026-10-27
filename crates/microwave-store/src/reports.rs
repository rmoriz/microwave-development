use crate::backend::{NewNodeRun, NodeRunFilter};
use crate::compression::{decode_serialized, encode_serialized};
use crate::identity::{new_id, now};
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::OptionalExtension;

pub fn normalize_rfc3339(value: &str) -> Result<String, StoreError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| {
            value
                .with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .map_err(|_| StoreError::Invalid(format!("invalid RFC 3339 timestamp: {value}")))
}

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
        let start_time = match run.start_time {
            Some(value) => value.to_string(),
            None => chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        };
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
                    report: decode_serialized(&row.get::<_, String>(10)?).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
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
        let since = filter
            .since
            .as_deref()
            .map(crate::normalize_rfc3339)
            .transpose()?;
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
                since.as_deref(),
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
                    report: decode_serialized(&row.get::<_, String>(10)?).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
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
                        report: decode_serialized(&row.get::<_, String>(10)?).map_err(|error| {
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
        assert_eq!(
            stored.report,
            r#"{"message_type":"run_converge","run_id":"placeholder"}"#
        );

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
            .store_node_run(
                &org_id,
                run("web1", "run-1", "2026-10-27T10:00:00.000Z"),
                10,
            )
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
            kept.iter()
                .map(|run| run.run_id.as_str())
                .collect::<Vec<_>>(),
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
