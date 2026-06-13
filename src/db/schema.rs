//! Database schema and migrations

use anyhow::Result;
use rusqlite::Connection;

/// Current schema version
const SCHEMA_VERSION: i32 = 3;

/// Database wrapper with schema management
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open or create database at the given path
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.initialize()?;
        Ok(db)
    }

    /// Open an in-memory database (for testing)
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.initialize()?;
        Ok(db)
    }

    /// Get a reference to the underlying connection
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Initialize the database schema
    fn initialize(&self) -> Result<()> {
        // Enable foreign keys
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        // Check current schema version
        let version = self.get_schema_version()?;

        if version == 0 {
            // Fresh database - create all tables
            self.create_schema()?;
        } else if version < SCHEMA_VERSION {
            // Need migration
            self.migrate(version)?;
        }

        Ok(())
    }

    /// Get the current schema version (0 if not initialized)
    fn get_schema_version(&self) -> Result<i32> {
        // Check if schema_version table exists
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_version')",
            [],
            |row| row.get(0),
        )?;

        if !exists {
            return Ok(0);
        }

        let version: i32 = self.conn.query_row(
            "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
            [],
            |row| row.get(0),
        )?;

        Ok(version)
    }

    /// Create the initial schema
    fn create_schema(&self) -> Result<()> {
        self.conn.execute_batch(include_str!("schema_v1.sql"))?;

        // Record schema version
        self.conn.execute(
            "INSERT INTO schema_version (version) VALUES (?)",
            [SCHEMA_VERSION],
        )?;

        Ok(())
    }

    /// Migrate from an older schema version
    fn migrate(&self, from_version: i32) -> Result<()> {
        if from_version < 2 {
            // Migrate to v2: add quarantine table
            self.conn.execute_batch(include_str!("schema_v2.sql"))?;
            self.conn
                .execute("INSERT INTO schema_version (version) VALUES (?)", [2])?;
        }
        if from_version < 3 {
            // Migrate to v3: add is_disk to dat_roms (CHD <disk> support)
            self.conn.execute_batch(include_str!("schema_v3.sql"))?;
            self.conn
                .execute("INSERT INTO schema_version (version) VALUES (?)", [3])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_database_creation() {
        let db = Database::open_in_memory().unwrap();
        let version = db.get_schema_version().unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }
}
