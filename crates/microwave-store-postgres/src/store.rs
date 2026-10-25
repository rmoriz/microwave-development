use chrono::{SecondsFormat, Utc};
use microwave_store::{Compression, StoreError};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;
use uuid::Uuid;

/// Advisory lock key used while applying the embedded schema. It serializes
/// concurrent `PostgresStore::connect` calls from multiple replicas so the
/// idempotent DDL cannot race on PostgreSQL's catalog.
const MIGRATION_ADVISORY_LOCK: i64 = 0x6d69_6372_6f77_6176;

/// A PostgreSQL-backed metadata store.
///
/// The store owns a `sqlx::PgPool`; cloning is cheap and every repository
/// method is safe to call concurrently.
#[derive(Clone)]
pub struct PostgresStore {
    pool: PgPool,
    compression: Compression,
}

impl PostgresStore {
    /// Connect to PostgreSQL and apply the unified schema.
    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        Self::connect_with_compression(database_url, Compression::default()).await
    }

    /// Connect to PostgreSQL, apply the unified schema, and use an explicit
    /// serialized-object compression mode for new writes.
    pub async fn connect_with_compression(
        database_url: &str,
        compression: Compression,
    ) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(db)?;
        let store = Self { pool, compression };
        store.migrate().await?;
        Ok(store)
    }

    /// Connect to PostgreSQL without applying the embedded schema.
    ///
    /// Intended for read-only diagnostics and snapshot export against a
    /// database whose schema is already managed by a server. Unlike
    /// [`PostgresStore::connect`], this performs no DDL, so it works with a
    /// read-only database role.
    pub async fn connect_existing(database_url: &str) -> Result<Self, StoreError> {
        Self::connect_existing_with_compression(database_url, Compression::default()).await
    }

    /// Connect without applying the schema and use an explicit compression
    /// mode for any subsequent writes.
    pub async fn connect_existing_with_compression(
        database_url: &str,
        compression: Compression,
    ) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(db)?;
        Ok(Self { pool, compression })
    }

    /// Wrap an existing pool. The caller is responsible for calling
    /// [`PostgresStore::migrate`].
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            compression: Compression::default(),
        }
    }

    /// Wrap an existing pool with an explicit compression mode.
    pub fn from_pool_with_compression(pool: PgPool, compression: Compression) -> Self {
        Self { pool, compression }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Compression mode used for newly written serialized objects.
    pub fn compression(&self) -> Compression {
        self.compression
    }

    /// Apply the embedded unified schema. Safe to run repeatedly and from
    /// several processes at once thanks to the advisory transaction lock.
    pub async fn migrate(&self) -> Result<(), StoreError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(MIGRATION_ADVISORY_LOCK)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        sqlx::raw_sql(include_str!("../migrations/0001_initial.sql"))
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    /// List user tables in the current schema. Used by schema tests and
    /// operational diagnostics.
    pub async fn table_names(&self) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query(
            "SELECT table_name
             FROM information_schema.tables
             WHERE table_schema = current_schema() AND table_type = 'BASE TABLE'
             ORDER BY table_name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.into_iter()
            .map(|row| row.try_get::<String, _>(0).map_err(db))
            .collect()
    }
}

pub(crate) fn db(error: sqlx::Error) -> StoreError {
    StoreError::Postgres(error.to_string())
}

/// Map a database error to a conflict when it is a unique/primary-key
/// violation, otherwise preserve it as a backend error.
pub(crate) fn unique(error: sqlx::Error, message: impl Into<String>) -> StoreError {
    if let sqlx::Error::Database(database) = &error {
        if database.is_unique_violation() {
            return StoreError::Conflict(message.into());
        }
    }
    db(error)
}

pub(crate) fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}

pub(crate) fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Decode the canonical org column order used by every org query.
pub(crate) fn org_from_row(row: &PgRow) -> Result<microwave_store::Org, sqlx::Error> {
    Ok(microwave_store::Org {
        id: row.try_get(0)?,
        authz_id: row.try_get(1)?,
        name: row.try_get(2)?,
        full_name: row.try_get(3)?,
        created_at: row.try_get(4)?,
        updated_at: row.try_get(5)?,
    })
}
