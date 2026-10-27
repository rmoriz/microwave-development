//! PostgreSQL implementation of the report repository.
//!
//! Semantics mirror the SQLite backend: `(org_id, node_name, run_id)` is the
//! retry identity, a retried report refreshes its row without changing the
//! original `id`/`created_at`, and pruning only happens for a positive
//! `keep_runs`.

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use microwave_store::{
    decode_serialized, encode_serialized, NewNodeRun, NodeRunFilter, ReportRepository, StoreError,
    StoredNodeRun,
};
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::store::{db, new_id, now, PostgresStore};

#[async_trait]
impl ReportRepository for PostgresStore {
    async fn store_node_run(
        &self,
        org_id: &str,
        run: NewNodeRun<'_>,
        keep_runs: usize,
    ) -> Result<StoredNodeRun, StoreError> {
        let id = new_id();
        let created_at = now();
        // Fall back to a fresh timestamp at millisecond precision so a run
        // without a reported start time still sorts deterministically.
        let start_time = match run.start_time {
            Some(start_time) => start_time.to_string(),
            None => Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        };
        let report = encode_serialized(run.report, self.compression())?;
        let mut transaction = self.pool().begin().await.map_err(db)?;

        sqlx::query(
            "INSERT INTO node_runs
             (id, org_id, node_name, run_id, status, start_time, end_time,
              resource_count, updated_resource_count, error_count, report, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT (org_id, node_name, run_id) DO UPDATE SET
                status = excluded.status,
                start_time = excluded.start_time,
                end_time = excluded.end_time,
                resource_count = excluded.resource_count,
                updated_resource_count = excluded.updated_resource_count,
                error_count = excluded.error_count,
                report = excluded.report",
        )
        .bind(&id)
        .bind(org_id)
        .bind(run.node_name)
        .bind(run.run_id)
        .bind(run.status)
        .bind(&start_time)
        .bind(run.end_time)
        .bind(run.resource_count)
        .bind(run.updated_resource_count)
        .bind(run.error_count)
        .bind(&report)
        .bind(&created_at)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;

        // Read the row before pruning: a retried run that is older than the
        // retained window is inserted (or refreshed) and then intentionally
        // pruned, but the caller still needs the stored value.
        let row = sqlx::query(
            "SELECT id, org_id, node_name, run_id, status, start_time, end_time,
                    resource_count, updated_resource_count, error_count, report, created_at
             FROM node_runs WHERE org_id = $1 AND node_name = $2 AND run_id = $3",
        )
        .bind(org_id)
        .bind(run.node_name)
        .bind(run.run_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        let stored = run_from_row(&row)?;

        if keep_runs > 0 {
            // `usize` can exceed `i64`; saturate instead of wrapping negative.
            let limit = i64::try_from(keep_runs).unwrap_or(i64::MAX);
            sqlx::query(
                "DELETE FROM node_runs
                 WHERE org_id = $1 AND node_name = $2
                   AND id NOT IN (
                     SELECT id FROM node_runs
                     WHERE org_id = $1 AND node_name = $2
                     ORDER BY start_time DESC, created_at DESC
                     LIMIT $3
                   )",
            )
            .bind(org_id)
            .bind(run.node_name)
            .bind(limit)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }

        transaction.commit().await.map_err(db)?;
        Ok(stored)
    }

    async fn list_node_runs(
        &self,
        org_id: &str,
        filter: &NodeRunFilter,
    ) -> Result<Vec<StoredNodeRun>, StoreError> {
        let since = filter
            .since
            .as_deref()
            .map(microwave_store::normalize_rfc3339)
            .transpose()?;
        // `usize` can exceed `i64`; saturate instead of wrapping negative.
        let limit = i64::try_from(filter.limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(
            "SELECT id, org_id, node_name, run_id, status, start_time, end_time,
                    resource_count, updated_resource_count, error_count, report, created_at
             FROM node_runs
             WHERE org_id = $1
               AND ($2::text IS NULL OR node_name = $2)
               AND ($3::text IS NULL OR status = $3)
               AND ($4::text IS NULL OR start_time >= $4)
               AND (NOT $5 OR status <> 'success' OR error_count > 0)
             ORDER BY start_time DESC, created_at DESC
             LIMIT $6",
        )
        .bind(org_id)
        .bind(filter.node_name.as_deref())
        .bind(filter.status.as_deref())
        .bind(since.as_deref())
        .bind(filter.failures_only)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        rows.iter().map(run_from_row).collect()
    }

    async fn find_node_run(
        &self,
        org_id: &str,
        node_name: &str,
        run_id: &str,
    ) -> Result<Option<StoredNodeRun>, StoreError> {
        let row = sqlx::query(
            "SELECT id, org_id, node_name, run_id, status, start_time, end_time,
                    resource_count, updated_resource_count, error_count, report, created_at
             FROM node_runs WHERE org_id = $1 AND node_name = $2 AND run_id = $3",
        )
        .bind(org_id)
        .bind(node_name)
        .bind(run_id)
        .fetch_optional(self.pool())
        .await
        .map_err(db)?;
        row.as_ref().map(run_from_row).transpose()
    }

    async fn latest_node_run(
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
            )
            .await?
            .into_iter()
            .next())
    }
}

/// Decode the canonical node-run column order used by every report query.
fn run_from_row(row: &PgRow) -> Result<StoredNodeRun, StoreError> {
    let report: String = row.try_get(10).map_err(db)?;
    Ok(StoredNodeRun {
        id: row.try_get(0).map_err(db)?,
        org_id: row.try_get(1).map_err(db)?,
        node_name: row.try_get(2).map_err(db)?,
        run_id: row.try_get(3).map_err(db)?,
        status: row.try_get(4).map_err(db)?,
        start_time: row.try_get(5).map_err(db)?,
        end_time: row.try_get(6).map_err(db)?,
        resource_count: row.try_get(7).map_err(db)?,
        updated_resource_count: row.try_get(8).map_err(db)?,
        error_count: row.try_get(9).map_err(db)?,
        report: decode_serialized(&report)?,
        created_at: row.try_get(11).map_err(db)?,
    })
}
