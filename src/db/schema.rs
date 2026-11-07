//! Database schema and migrations

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// Current schema version
const SCHEMA_VERSION: i32 = 6;

/// Every column that stores a filesystem path, as `(table, column)`. Each table
/// has an integer `id` primary key.
const PATH_COLUMNS: &[(&str, &str)] = &[
    ("sources", "path"),
    ("file_locations", "path"),
    ("file_locations", "archive_path"),
    ("collection_versions", "dat_path"),
    ("dat_config", "dest_path"),
    ("quarantine", "original_path"),
    ("quarantine", "quarantine_path"),
    ("operation_log", "source_path"),
    ("operation_log", "dest_path"),
];

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
        if from_version < 4 {
            // Migrate to v4: index files(crc32, size) so the CRC-only match
            // branch (merged arcade sets) uses an index instead of a full scan.
            self.conn.execute_batch(include_str!("schema_v4.sql"))?;
            self.conn
                .execute("INSERT INTO schema_version (version) VALUES (?)", [4])?;
        }
        if from_version < 5 {
            // Migrate to v5: per-source disposition (consume/preserve),
            // replacing the `plan --move` flag. All existing sources → preserve
            // (safe); staging sources are flipped to consume separately.
            self.conn.execute_batch(include_str!("schema_v5.sql"))?;
            self.conn
                .execute("INSERT INTO schema_version (version) VALUES (?)", [5])?;
        }
        if from_version < 6 {
            // Migrate to v6: stored paths take catalogue form, `/`-separated
            // with no verbatim prefix (decisions/catalogue-path-separator.md).
            // Only Windows ever stored `\` separators; elsewhere `\` is a legal
            // filename character, so only the version is recorded.
            let tx = self.conn.unchecked_transaction()?;
            if cfg!(windows) {
                rewrite_stored_paths(&tx, crate::paths::windows_to_catalogue)?;
            }
            tx.execute("INSERT INTO schema_version (version) VALUES (?)", [6])?;
            tx.commit()?;
        }
        Ok(())
    }
}

/// Rewrite every stored path containing a `\` through `convert`.
///
/// Run inside a transaction: an error leaves the caller to roll back, so a
/// half-rewritten catalogue is never committed. Two rows that would end up
/// holding the same unique key abort with an error naming them, rather than
/// one silently replacing the other. Loose `file_locations` rows are checked
/// explicitly, because their NULL `archive_path` puts them outside the UNIQUE
/// constraint.
fn rewrite_stored_paths(conn: &Connection, convert: impl Fn(&str) -> String) -> Result<()> {
    let mut changed_locations: Vec<i64> = Vec::new();
    for (table, column) in PATH_COLUMNS {
        let mut select = conn.prepare(&format!(
            r"SELECT id, {column} FROM {table} WHERE instr({column}, '\') > 0"
        ))?;
        let rows = select
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut update =
            conn.prepare(&format!("UPDATE {table} SET {column} = ?1 WHERE id = ?2"))?;
        for (id, old) in rows {
            let new = convert(&old);
            if new == old {
                continue;
            }
            match update.execute(rusqlite::params![new, id]) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    return Err(collision_error(&format!(
                        "{table}.{column} {old:?} becomes {new:?}"
                    )));
                }
                Err(e) => return Err(e.into()),
            }
            if *table == "file_locations" {
                changed_locations.push(id);
            }
        }
    }

    let mut duplicate = conn.prepare(
        "SELECT o.path
           FROM file_locations c
           JOIN file_locations o
             ON o.source_id = c.source_id
            AND o.path = c.path
            AND o.archive_path IS c.archive_path
            AND o.id <> c.id
          WHERE c.id = ?1
          LIMIT 1",
    )?;
    for id in changed_locations {
        if let Some(path) = duplicate
            .query_row([id], |row| row.get::<_, String>(0))
            .optional()?
        {
            return Err(collision_error(&format!(
                "a loose file_locations row becomes {path:?}"
            )));
        }
    }
    Ok(())
}

fn collision_error(what: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Cannot upgrade the catalogue to schema v6: {what} with `/` separators, \
         which another row already holds. Nothing was changed. \
         Back up db.sqlite, delete one of the duplicate rows with an SQLite tool, \
         and run cat198x again."
    )
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

    use crate::paths::windows_to_catalogue;

    fn text(conn: &Connection, sql: &str) -> Option<String> {
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn rewrite_converts_every_windows_path_column() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        conn.execute_batch(
            r"
            INSERT INTO sources (id, path, case_sensitive) VALUES
                (1, '\\?\C:\ROMs\ToSort', 0),
                (2, '\\?\UNC\nas\media\ROMs', 0),
                (3, 'C:/Library', 0);
            INSERT INTO files (sha1, size) VALUES ('AAA', 1);
            INSERT INTO file_locations (id, sha1, source_id, path, archive_path) VALUES
                (1, 'AAA', 1, 'Sinclair\game.rom', NULL),
                (2, 'AAA', 2, 'Sets\pack.zip', 'inner\a.rom');
            INSERT INTO collections (id, name, source_type) VALUES (1, 'NES', 'nointro');
            INSERT INTO collection_versions (id, collection_id, version, dat_path)
                VALUES (1, 1, 'v1', '\\?\C:\DATs\nes.dat');
            INSERT INTO dat_config (id, path_pattern, dest_path)
                VALUES (1, 'NES', 'D:\Library\NES');
            INSERT INTO quarantine (id, sha1, original_path, quarantine_path, size, reason)
                VALUES (1, 'AAA', 'C:\ROMs\ToSort\game.rom', 'AAA_game.rom', 1, 'set_removed');
            INSERT INTO operation_log (id, plan_hash, operation_type, source_path, dest_path)
                VALUES (1, 'h', 'copy', 'C:\ROMs\a.rom', 'D:\Library\a.rom');
            ",
        )
        .unwrap();

        rewrite_stored_paths(conn, windows_to_catalogue).unwrap();

        for (sql, expected) in [
            (
                "SELECT path FROM sources WHERE id = 1",
                Some("C:/ROMs/ToSort"),
            ),
            (
                "SELECT path FROM sources WHERE id = 2",
                Some("//nas/media/ROMs"),
            ),
            ("SELECT path FROM sources WHERE id = 3", Some("C:/Library")),
            (
                "SELECT path FROM file_locations WHERE id = 1",
                Some("Sinclair/game.rom"),
            ),
            ("SELECT archive_path FROM file_locations WHERE id = 1", None),
            (
                "SELECT path FROM file_locations WHERE id = 2",
                Some("Sets/pack.zip"),
            ),
            (
                "SELECT archive_path FROM file_locations WHERE id = 2",
                Some("inner/a.rom"),
            ),
            (
                "SELECT dat_path FROM collection_versions WHERE id = 1",
                Some("C:/DATs/nes.dat"),
            ),
            (
                "SELECT dest_path FROM dat_config WHERE id = 1",
                Some("D:/Library/NES"),
            ),
            (
                "SELECT original_path FROM quarantine WHERE id = 1",
                Some("C:/ROMs/ToSort/game.rom"),
            ),
            (
                "SELECT quarantine_path FROM quarantine WHERE id = 1",
                Some("AAA_game.rom"),
            ),
            (
                "SELECT source_path FROM operation_log WHERE id = 1",
                Some("C:/ROMs/a.rom"),
            ),
            (
                "SELECT dest_path FROM operation_log WHERE id = 1",
                Some("D:/Library/a.rom"),
            ),
        ] {
            assert_eq!(text(conn, sql).as_deref(), expected, "{sql}");
        }
    }

    #[test]
    fn rewrite_refuses_a_unique_collision_and_rolls_back() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        conn.execute_batch(
            r"
            INSERT INTO sources (id, path, case_sensitive) VALUES
                (1, 'C:\ROMs', 0),
                (2, 'C:/ROMs', 0);
            ",
        )
        .unwrap();

        let tx = conn.unchecked_transaction().unwrap();
        let err = rewrite_stored_paths(&tx, windows_to_catalogue).unwrap_err();
        drop(tx);

        let message = err.to_string();
        assert!(message.contains("sources.path"), "{message}");
        assert!(message.contains("C:/ROMs"), "{message}");
        assert_eq!(
            text(conn, "SELECT path FROM sources WHERE id = 1").as_deref(),
            Some(r"C:\ROMs"),
            "both rows survive"
        );
        assert_eq!(
            text(conn, "SELECT path FROM sources WHERE id = 2").as_deref(),
            Some("C:/ROMs")
        );
    }

    #[test]
    fn rewrite_refuses_duplicate_loose_locations_and_rolls_back_earlier_tables() {
        // Loose rows have a NULL archive_path, which the UNIQUE constraint
        // ignores, so the rewrite has to catch this collision itself.
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        conn.execute_batch(
            r"
            INSERT INTO sources (id, path, case_sensitive) VALUES (1, 'C:\ROMs', 0);
            INSERT INTO files (sha1, size) VALUES ('AAA', 1), ('BBB', 1);
            INSERT INTO file_locations (id, sha1, source_id, path, archive_path) VALUES
                (1, 'AAA', 1, 'Sinclair\game.rom', NULL),
                (2, 'BBB', 1, 'Sinclair/game.rom', NULL);
            ",
        )
        .unwrap();

        let tx = conn.unchecked_transaction().unwrap();
        let err = rewrite_stored_paths(&tx, windows_to_catalogue).unwrap_err();
        drop(tx);

        let message = err.to_string();
        assert!(message.contains("Sinclair/game.rom"), "{message}");
        assert_eq!(
            text(conn, "SELECT path FROM sources WHERE id = 1").as_deref(),
            Some(r"C:\ROMs"),
            "the source rewrite is rolled back with the rest"
        );
        assert_eq!(
            text(conn, "SELECT path FROM file_locations WHERE id = 1").as_deref(),
            Some(r"Sinclair\game.rom")
        );
    }

    #[test]
    fn v5_catalogue_upgrades_to_v6_rewriting_paths_only_on_windows() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        conn.execute("UPDATE schema_version SET version = 5", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (1, ?1, 0)",
            [r"C:\ROMs\odd\name"],
        )
        .unwrap();

        db.initialize().unwrap();

        assert_eq!(db.get_schema_version().unwrap(), 6);
        let expected = if cfg!(windows) {
            "C:/ROMs/odd/name"
        } else {
            // A backslash is a legal filename character outside Windows.
            r"C:\ROMs\odd\name"
        };
        assert_eq!(
            text(conn, "SELECT path FROM sources WHERE id = 1").as_deref(),
            Some(expected)
        );
    }
}
