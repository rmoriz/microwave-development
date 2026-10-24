use crate::identity::{new_id, now};
use crate::sqlite::{SqliteStore, StoreError};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxCheck {
    pub checksum: String,
    pub needs_upload: bool,
}

impl SqliteStore {
    pub fn create_sandbox(
        &self,
        org_id: &str,
        checksums: &[String],
    ) -> Result<(String, Vec<SandboxCheck>), StoreError> {
        let sandbox_id = new_id();
        let timestamp = now();
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        tx.execute(
            "INSERT INTO sandboxes (org_id, sandbox_id, created_at) VALUES (?1, ?2, ?3)",
            params![org_id, sandbox_id, timestamp],
        )?;
        let mut checks = Vec::with_capacity(checksums.len());
        for checksum in checksums {
            let exists: i64 = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM checksums WHERE org_id = ?1 AND checksum = ?2)",
                params![org_id, checksum],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO sandboxed_checksums
                 (org_id, sandbox_id, checksum, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![org_id, sandbox_id, checksum, timestamp],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO sandbox_checksums
                 (org_id, sandbox_id, checksum, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![org_id, sandbox_id, checksum, timestamp],
            )?;
            checks.push(SandboxCheck {
                checksum: checksum.clone(),
                needs_upload: exists == 0,
            });
        }
        tx.commit()?;
        Ok((sandbox_id, checks))
    }

    pub fn sandbox_checksums(
        &self,
        org_id: &str,
        sandbox_id: &str,
    ) -> Result<Vec<String>, StoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT checksum FROM sandbox_checksums
             WHERE org_id = ?1 AND sandbox_id = ?2 ORDER BY checksum",
        )?;
        let rows = statement.query_map(params![org_id, sandbox_id], |row| row.get(0))?;
        let mut checksums = Vec::new();
        for row in rows {
            checksums.push(row?);
        }
        Ok(checksums)
    }

    pub fn sandbox_exists(&self, org_id: &str, sandbox_id: &str) -> Result<bool, StoreError> {
        let connection = self.connection();
        let exists: i64 = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sandboxes WHERE org_id = ?1 AND sandbox_id = ?2
             )",
            params![org_id, sandbox_id],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    pub fn sandbox_created_at(
        &self,
        org_id: &str,
        sandbox_id: &str,
    ) -> Result<Option<String>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT created_at FROM sandboxes
                 WHERE org_id = ?1 AND sandbox_id = ?2",
                params![org_id, sandbox_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn commit_sandbox(&self, org_id: &str, sandbox_id: &str) -> Result<(), StoreError> {
        let timestamp = now();
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO checksums (org_id, checksum, created_at)
             SELECT org_id, checksum, ?3 FROM sandbox_checksums
             WHERE org_id = ?1 AND sandbox_id = ?2",
            params![org_id, sandbox_id, timestamp],
        )?;
        tx.execute(
            "DELETE FROM sandboxed_checksums WHERE org_id = ?1 AND sandbox_id = ?2",
            params![org_id, sandbox_id],
        )?;
        tx.execute(
            "DELETE FROM sandbox_checksums WHERE org_id = ?1 AND sandbox_id = ?2",
            params![org_id, sandbox_id],
        )?;
        tx.execute(
            "DELETE FROM sandboxes WHERE org_id = ?1 AND sandbox_id = ?2",
            params![org_id, sandbox_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn checksum_exists(&self, org_id: &str, checksum: &str) -> Result<bool, StoreError> {
        let connection = self.connection();
        let exists: i64 = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM checksums WHERE org_id = ?1 AND checksum = ?2)",
            params![org_id, checksum],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    pub fn find_checksum(
        &self,
        org_id: &str,
        checksum: &str,
    ) -> Result<Option<String>, StoreError> {
        let connection = self.connection();
        Ok(connection
            .query_row(
                "SELECT checksum FROM checksums WHERE org_id = ?1 AND checksum = ?2",
                params![org_id, checksum],
                |row| row.get(0),
            )
            .optional()?)
    }
}
