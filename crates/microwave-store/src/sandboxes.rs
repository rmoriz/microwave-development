use crate::identity::{new_id, now};
use crate::sqlite::{in_placeholders, SqliteStore, StoreError};
use rusqlite::{params, params_from_iter, OptionalExtension};
use std::collections::HashSet;

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

        // One query for all checksums instead of one `EXISTS` per checksum.
        let existing: HashSet<String> = if checksums.is_empty() {
            HashSet::new()
        } else {
            let sql = format!(
                "SELECT checksum FROM checksums WHERE org_id = ? AND checksum IN ({})",
                in_placeholders(checksums.len())
            );
            let mut statement = tx.prepare(&sql)?;
            let mut values: Vec<&str> = Vec::with_capacity(checksums.len() + 1);
            values.push(org_id);
            values.extend(checksums.iter().map(String::as_str));
            let rows =
                statement.query_map(params_from_iter(values), |row| row.get::<_, String>(0))?;
            let mut found = HashSet::new();
            for row in rows {
                found.insert(row?);
            }
            found
        };

        // Both tables get the whole sandbox in one multi-row insert.
        if !checksums.is_empty() {
            let placeholders = ["(?, ?, ?, ?)"].repeat(checksums.len()).join(", ");
            let mut bindings: Vec<&str> = Vec::with_capacity(checksums.len() * 4);
            for checksum in checksums {
                bindings.push(org_id);
                bindings.push(sandbox_id.as_str());
                bindings.push(checksum.as_str());
                bindings.push(timestamp.as_str());
            }
            for table in ["sandboxed_checksums", "sandbox_checksums"] {
                let sql = format!(
                    "INSERT OR IGNORE INTO {table}
                     (org_id, sandbox_id, checksum, created_at) VALUES {placeholders}"
                );
                tx.execute(&sql, params_from_iter(bindings.iter().copied()))?;
            }
        }

        let checks = checksums
            .iter()
            .map(|checksum| SandboxCheck {
                checksum: checksum.clone(),
                needs_upload: !existing.contains(checksum),
            })
            .collect();
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

    /// Returns the subset of `checksums` that has already been committed for
    /// the organization, using one query instead of one per checksum.
    pub fn existing_checksums(
        &self,
        org_id: &str,
        checksums: &[String],
    ) -> Result<Vec<String>, StoreError> {
        if checksums.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.connection();
        let sql = format!(
            "SELECT checksum FROM checksums
             WHERE org_id = ? AND checksum IN ({}) ORDER BY checksum",
            in_placeholders(checksums.len())
        );
        let mut statement = connection.prepare(&sql)?;
        let mut values: Vec<&str> = Vec::with_capacity(checksums.len() + 1);
        values.push(org_id);
        values.extend(checksums.iter().map(String::as_str));
        let rows = statement.query_map(params_from_iter(values), |row| row.get::<_, String>(0))?;
        let mut found = Vec::new();
        for row in rows {
            found.push(row?);
        }
        Ok(found)
    }

    pub fn ensure_checksums(&self, org_id: &str, checksums: &[String]) -> Result<(), StoreError> {
        if checksums.is_empty() {
            return Ok(());
        }
        let timestamp = now();
        let mut connection = self.connection();
        let tx = connection.transaction()?;
        for checksum in checksums {
            tx.execute(
                "INSERT OR IGNORE INTO checksums (org_id, checksum, created_at)
                 VALUES (?1, ?2, ?3)",
                params![org_id, checksum, timestamp],
            )?;
        }
        tx.commit()?;
        Ok(())
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
