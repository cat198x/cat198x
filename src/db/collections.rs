//! Collection and version CRUD operations

use anyhow::Result;
use rusqlite::{Connection, params};

/// A collection (e.g., "Nintendo - NES", "MAME")
#[derive(Debug, Clone)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub source_type: String,
    pub created_at: String,
}

/// A specific version of a collection's DAT
#[derive(Debug, Clone)]
pub struct CollectionVersion {
    pub id: i64,
    pub collection_id: i64,
    pub version: String,
    pub dat_path: String,
    pub is_active: bool,
    pub imported_at: String,
}

/// Create a new collection
pub fn create_collection(conn: &Connection, name: &str, source_type: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO collections (name, source_type) VALUES (?, ?)",
        params![name, source_type],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get a collection by name
pub fn get_collection_by_name(conn: &Connection, name: &str) -> Result<Option<Collection>> {
    let mut stmt =
        conn.prepare("SELECT id, name, source_type, created_at FROM collections WHERE name = ?")?;

    let result = stmt.query_row([name], |row| {
        Ok(Collection {
            id: row.get(0)?,
            name: row.get(1)?,
            source_type: row.get(2)?,
            created_at: row.get(3)?,
        })
    });

    match result {
        Ok(c) => Ok(Some(c)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// List all collections
pub fn list_collections(conn: &Connection) -> Result<Vec<Collection>> {
    let mut stmt =
        conn.prepare("SELECT id, name, source_type, created_at FROM collections ORDER BY name")?;

    let collections = stmt
        .query_map([], |row| {
            Ok(Collection {
                id: row.get(0)?,
                name: row.get(1)?,
                source_type: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(collections)
}

/// Add a new version to a collection
pub fn add_version(
    conn: &Connection,
    collection_id: i64,
    version: &str,
    dat_path: &str,
    activate: bool,
) -> Result<i64> {
    // If activating, deactivate all other versions first
    if activate {
        conn.execute(
            "UPDATE collection_versions SET is_active = 0 WHERE collection_id = ?",
            [collection_id],
        )?;
    }

    conn.execute(
        "INSERT INTO collection_versions (collection_id, version, dat_path, is_active) VALUES (?, ?, ?, ?)",
        params![collection_id, version, dat_path, activate],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Get the active version for a collection
pub fn get_active_version(
    conn: &Connection,
    collection_id: i64,
) -> Result<Option<CollectionVersion>> {
    let mut stmt = conn.prepare(
        "SELECT id, collection_id, version, dat_path, is_active, imported_at
         FROM collection_versions
         WHERE collection_id = ? AND is_active = 1",
    )?;

    let result = stmt.query_row([collection_id], |row| {
        Ok(CollectionVersion {
            id: row.get(0)?,
            collection_id: row.get(1)?,
            version: row.get(2)?,
            dat_path: row.get(3)?,
            is_active: row.get(4)?,
            imported_at: row.get(5)?,
        })
    });

    match result {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Update the on-disk DAT path recorded for a version (used by `dat relink`
/// when a DAT file has moved). Returns true if a row was updated.
pub fn update_dat_path(conn: &Connection, version_id: i64, new_path: &str) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE collection_versions SET dat_path = ? WHERE id = ?",
        params![new_path, version_id],
    )?;
    Ok(changed > 0)
}

/// Activate a specific version
pub fn activate_version(conn: &Connection, collection_id: i64, version: &str) -> Result<bool> {
    // Deactivate all versions
    conn.execute(
        "UPDATE collection_versions SET is_active = 0 WHERE collection_id = ?",
        [collection_id],
    )?;

    // Activate the specified version
    let updated = conn.execute(
        "UPDATE collection_versions SET is_active = 1 WHERE collection_id = ? AND version = ?",
        params![collection_id, version],
    )?;

    Ok(updated > 0)
}

/// Remove a collection and all its versions (CASCADE deletes related data)
pub fn remove_collection(conn: &Connection, collection_id: i64) -> Result<bool> {
    let deleted = conn.execute("DELETE FROM collections WHERE id = ?", [collection_id])?;
    Ok(deleted > 0)
}

/// Remove a specific version from a collection
/// Returns (was_deleted, was_active) - caller may want to activate another version
pub fn remove_version(conn: &Connection, version_id: i64) -> Result<(bool, bool)> {
    // Check if this version was active before deleting
    let was_active: bool = conn
        .query_row(
            "SELECT is_active FROM collection_versions WHERE id = ?",
            [version_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    let deleted = conn.execute("DELETE FROM collection_versions WHERE id = ?", [version_id])?;
    Ok((deleted > 0, was_active))
}

/// Get a version by collection_id and version string
pub fn get_version_by_name(
    conn: &Connection,
    collection_id: i64,
    version: &str,
) -> Result<Option<CollectionVersion>> {
    let mut stmt = conn.prepare(
        "SELECT id, collection_id, version, dat_path, is_active, imported_at
         FROM collection_versions
         WHERE collection_id = ? AND version = ?",
    )?;

    let result = stmt.query_row(params![collection_id, version], |row| {
        Ok(CollectionVersion {
            id: row.get(0)?,
            collection_id: row.get(1)?,
            version: row.get(2)?,
            dat_path: row.get(3)?,
            is_active: row.get(4)?,
            imported_at: row.get(5)?,
        })
    });

    match result {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Count versions for a collection
pub fn count_versions(conn: &Connection, collection_id: i64) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM collection_versions WHERE collection_id = ?",
        [collection_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// List all versions for a collection
pub fn list_versions(conn: &Connection, collection_id: i64) -> Result<Vec<CollectionVersion>> {
    let mut stmt = conn.prepare(
        "SELECT id, collection_id, version, dat_path, is_active, imported_at
         FROM collection_versions
         WHERE collection_id = ?
         ORDER BY imported_at DESC",
    )?;

    let versions = stmt
        .query_map([collection_id], |row| {
            Ok(CollectionVersion {
                id: row.get(0)?,
                collection_id: row.get(1)?,
                version: row.get(2)?,
                dat_path: row.get(3)?,
                is_active: row.get(4)?,
                imported_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_create_and_get_collection() {
        let db = setup_db();
        let conn = db.conn();

        let id = create_collection(conn, "Nintendo - NES", "nointro").unwrap();
        assert!(id > 0);

        let collection = get_collection_by_name(conn, "Nintendo - NES").unwrap();
        assert!(collection.is_some());

        let c = collection.unwrap();
        assert_eq!(c.id, id);
        assert_eq!(c.name, "Nintendo - NES");
        assert_eq!(c.source_type, "nointro");
    }

    #[test]
    fn test_get_nonexistent_collection() {
        let db = setup_db();
        let conn = db.conn();

        let collection = get_collection_by_name(conn, "Does Not Exist").unwrap();
        assert!(collection.is_none());
    }

    #[test]
    fn test_list_collections() {
        let db = setup_db();
        let conn = db.conn();

        create_collection(conn, "MAME", "mame").unwrap();
        create_collection(conn, "Nintendo - NES", "nointro").unwrap();
        create_collection(conn, "Atari - 2600", "nointro").unwrap();

        let collections = list_collections(conn).unwrap();
        assert_eq!(collections.len(), 3);

        // Should be sorted by name
        assert_eq!(collections[0].name, "Atari - 2600");
        assert_eq!(collections[1].name, "MAME");
        assert_eq!(collections[2].name, "Nintendo - NES");
    }

    #[test]
    fn test_add_version_and_activate() {
        let db = setup_db();
        let conn = db.conn();

        let coll_id = create_collection(conn, "Nintendo - NES", "nointro").unwrap();

        // Add first version (activated)
        let v1_id = add_version(conn, coll_id, "20231201", "/path/to/v1.dat", true).unwrap();
        assert!(v1_id > 0);

        // Check it's active
        let active = get_active_version(conn, coll_id).unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().version, "20231201");

        // Add second version (activated - should deactivate first)
        let _v2_id = add_version(conn, coll_id, "20231215", "/path/to/v2.dat", true).unwrap();

        let active = get_active_version(conn, coll_id).unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().version, "20231215");
    }

    #[test]
    fn test_add_version_without_activating() {
        let db = setup_db();
        let conn = db.conn();

        let coll_id = create_collection(conn, "MAME", "mame").unwrap();

        // Add version without activating
        add_version(conn, coll_id, "0.261", "/path/to/mame.dat", false).unwrap();

        // Should have no active version
        let active = get_active_version(conn, coll_id).unwrap();
        assert!(active.is_none());
    }

    #[test]
    fn test_activate_specific_version() {
        let db = setup_db();
        let conn = db.conn();

        let coll_id = create_collection(conn, "Nintendo - NES", "nointro").unwrap();

        add_version(conn, coll_id, "20231201", "/path/to/v1.dat", true).unwrap();
        add_version(conn, coll_id, "20231215", "/path/to/v2.dat", false).unwrap();
        add_version(conn, coll_id, "20231225", "/path/to/v3.dat", false).unwrap();

        // Active should be v1
        let active = get_active_version(conn, coll_id).unwrap().unwrap();
        assert_eq!(active.version, "20231201");

        // Activate v2
        let success = activate_version(conn, coll_id, "20231215").unwrap();
        assert!(success);

        let active = get_active_version(conn, coll_id).unwrap().unwrap();
        assert_eq!(active.version, "20231215");

        // Try to activate non-existent version
        let success = activate_version(conn, coll_id, "99999999").unwrap();
        assert!(!success);
    }

    #[test]
    fn test_list_versions() {
        let db = setup_db();
        let conn = db.conn();

        let coll_id = create_collection(conn, "Nintendo - NES", "nointro").unwrap();

        add_version(conn, coll_id, "20231201", "/path/to/v1.dat", false).unwrap();
        add_version(conn, coll_id, "20231215", "/path/to/v2.dat", true).unwrap();

        let versions = list_versions(conn, coll_id).unwrap();
        assert_eq!(versions.len(), 2);

        // Find the active one
        let active_count = versions.iter().filter(|v| v.is_active).count();
        assert_eq!(active_count, 1);
    }

    #[test]
    fn test_duplicate_collection_name_fails() {
        let db = setup_db();
        let conn = db.conn();

        create_collection(conn, "Nintendo - NES", "nointro").unwrap();
        let result = create_collection(conn, "Nintendo - NES", "nointro");

        assert!(result.is_err());
    }

    #[test]
    fn test_remove_collection() {
        let db = setup_db();
        let conn = db.conn();

        let coll_id = create_collection(conn, "Nintendo - NES", "nointro").unwrap();
        add_version(conn, coll_id, "20231201", "/path/to/v1.dat", true).unwrap();
        add_version(conn, coll_id, "20231215", "/path/to/v2.dat", false).unwrap();

        // Verify it exists
        assert!(
            get_collection_by_name(conn, "Nintendo - NES")
                .unwrap()
                .is_some()
        );

        // Remove it
        let removed = remove_collection(conn, coll_id).unwrap();
        assert!(removed);

        // Verify it's gone
        assert!(
            get_collection_by_name(conn, "Nintendo - NES")
                .unwrap()
                .is_none()
        );

        // Verify versions are gone too (CASCADE)
        let versions = list_versions(conn, coll_id).unwrap();
        assert!(versions.is_empty());
    }

    #[test]
    fn test_remove_version() {
        let db = setup_db();
        let conn = db.conn();

        let coll_id = create_collection(conn, "Nintendo - NES", "nointro").unwrap();
        let v1_id = add_version(conn, coll_id, "20231201", "/path/to/v1.dat", true).unwrap();
        add_version(conn, coll_id, "20231215", "/path/to/v2.dat", false).unwrap();

        // Remove the active version
        let (deleted, was_active) = remove_version(conn, v1_id).unwrap();
        assert!(deleted);
        assert!(was_active);

        // Collection should still exist
        assert!(
            get_collection_by_name(conn, "Nintendo - NES")
                .unwrap()
                .is_some()
        );

        // Only one version should remain
        let versions = list_versions(conn, coll_id).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "20231215");
    }

    #[test]
    fn test_get_version_by_name() {
        let db = setup_db();
        let conn = db.conn();

        let coll_id = create_collection(conn, "Nintendo - NES", "nointro").unwrap();
        add_version(conn, coll_id, "20231201", "/path/to/v1.dat", true).unwrap();

        let version = get_version_by_name(conn, coll_id, "20231201").unwrap();
        assert!(version.is_some());
        assert_eq!(version.unwrap().dat_path, "/path/to/v1.dat");

        let missing = get_version_by_name(conn, coll_id, "nonexistent").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_count_versions() {
        let db = setup_db();
        let conn = db.conn();

        let coll_id = create_collection(conn, "Nintendo - NES", "nointro").unwrap();
        assert_eq!(count_versions(conn, coll_id).unwrap(), 0);

        add_version(conn, coll_id, "20231201", "/path/to/v1.dat", true).unwrap();
        assert_eq!(count_versions(conn, coll_id).unwrap(), 1);

        add_version(conn, coll_id, "20231215", "/path/to/v2.dat", false).unwrap();
        assert_eq!(count_versions(conn, coll_id).unwrap(), 2);
    }
}
