//! Database operations for per-collection configuration

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::filter::FilterPreferences;

/// A collection's configuration from the dat_config table
#[derive(Debug, Clone)]
pub struct CollectionConfig {
    pub id: i64,
    pub path_pattern: String,
    pub dest_path: Option<String>,
    pub output_format: Option<String>,
    pub merge_mode: Option<String>,
    pub extra_config: Option<ExtraConfig>,
}

/// Extended configuration stored as JSON
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraConfig {
    /// Enable 1G1R filtering (One Game One ROM)
    #[serde(default)]
    pub one_g_one_r: bool,
    /// Region priority for 1G1R (first = most preferred)
    #[serde(default)]
    pub region_priority: Vec<String>,
    /// Exclude cracks, trainers, hacks, etc.
    #[serde(default = "default_true")]
    pub exclude_modified: bool,
    /// Exclude bad dumps
    #[serde(default = "default_true")]
    pub exclude_bad_dumps: bool,
    /// Exclude betas, protos, demos
    #[serde(default)]
    pub exclude_prereleases: bool,
    /// Prefer verified dumps [!]
    #[serde(default = "default_true")]
    pub prefer_verified: bool,
}

impl Default for ExtraConfig {
    fn default() -> Self {
        Self {
            one_g_one_r: false,
            region_priority: Vec::new(),
            exclude_modified: true,
            exclude_bad_dumps: true,
            exclude_prereleases: false,
            prefer_verified: true,
        }
    }
}

fn default_true() -> bool {
    true
}

impl ExtraConfig {
    /// Convert to FilterPreferences for use in plan generation
    pub fn to_filter_preferences(&self) -> FilterPreferences {
        let mut prefs = if self.region_priority.is_empty() {
            FilterPreferences::default()
        } else {
            FilterPreferences::with_regions(self.region_priority.clone())
        };
        prefs.exclude_modified = self.exclude_modified;
        prefs.exclude_bad_dumps = self.exclude_bad_dumps;
        prefs.exclude_prereleases = self.exclude_prereleases;
        prefs.prefer_verified = self.prefer_verified;
        prefs
    }
}

/// Get configuration for a specific collection (exact match on path_pattern)
pub fn get_collection_config(conn: &Connection, collection: &str) -> Result<Option<CollectionConfig>> {
    let mut stmt = conn.prepare(
        "SELECT id, path_pattern, dest_path, output_format, merge_mode, config_json
         FROM dat_config
         WHERE path_pattern = ?",
    )?;

    let config = stmt
        .query_row([collection], |row| {
            let config_json: Option<String> = row.get(5)?;
            let extra_config = config_json.and_then(|json| serde_json::from_str(&json).ok());
            Ok(CollectionConfig {
                id: row.get(0)?,
                path_pattern: row.get(1)?,
                dest_path: row.get(2)?,
                output_format: row.get(3)?,
                merge_mode: row.get(4)?,
                extra_config,
            })
        })
        .optional()?;

    Ok(config)
}

/// Set destination path for a collection
pub fn set_dest_path(conn: &Connection, collection: &str, dest_path: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO dat_config (path_pattern, dest_path)
         VALUES (?, ?)
         ON CONFLICT(path_pattern) DO UPDATE SET dest_path = excluded.dest_path",
        [collection, dest_path],
    )?;
    Ok(())
}

/// Set output format for a collection
pub fn set_output_format(conn: &Connection, collection: &str, format: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO dat_config (path_pattern, output_format)
         VALUES (?, ?)
         ON CONFLICT(path_pattern) DO UPDATE SET output_format = excluded.output_format",
        [collection, format],
    )?;
    Ok(())
}

/// Set merge mode for a collection
pub fn set_merge_mode(conn: &Connection, collection: &str, mode: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO dat_config (path_pattern, merge_mode)
         VALUES (?, ?)
         ON CONFLICT(path_pattern) DO UPDATE SET merge_mode = excluded.merge_mode",
        [collection, mode],
    )?;
    Ok(())
}

/// List all collection configurations
pub fn list_all_configs(conn: &Connection) -> Result<Vec<CollectionConfig>> {
    let mut stmt = conn.prepare(
        "SELECT id, path_pattern, dest_path, output_format, merge_mode, config_json
         FROM dat_config
         ORDER BY path_pattern",
    )?;

    let configs = stmt
        .query_map([], |row| {
            let config_json: Option<String> = row.get(5)?;
            let extra_config = config_json.and_then(|json| serde_json::from_str(&json).ok());
            Ok(CollectionConfig {
                id: row.get(0)?,
                path_pattern: row.get(1)?,
                dest_path: row.get(2)?,
                output_format: row.get(3)?,
                merge_mode: row.get(4)?,
                extra_config,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(configs)
}

/// Get the effective destination path for a collection
/// Returns None if not configured
pub fn get_dest_path(conn: &Connection, collection: &str) -> Result<Option<String>> {
    let config = get_collection_config(conn, collection)?;
    Ok(config.and_then(|c| c.dest_path))
}

/// Enable or disable 1G1R filtering for a collection
pub fn set_one_g_one_r(conn: &Connection, collection: &str, enabled: bool) -> Result<()> {
    let existing = get_collection_config(conn, collection)?;
    let mut extra = existing
        .and_then(|c| c.extra_config)
        .unwrap_or_default();
    extra.one_g_one_r = enabled;
    set_extra_config(conn, collection, &extra)
}

/// Set region priority for 1G1R filtering
pub fn set_region_priority(conn: &Connection, collection: &str, regions: Vec<String>) -> Result<()> {
    let existing = get_collection_config(conn, collection)?;
    let mut extra = existing
        .and_then(|c| c.extra_config)
        .unwrap_or_default();
    extra.region_priority = regions;
    set_extra_config(conn, collection, &extra)
}

/// Set whether to exclude prereleases (betas, protos, demos)
pub fn set_exclude_prereleases(conn: &Connection, collection: &str, exclude: bool) -> Result<()> {
    let existing = get_collection_config(conn, collection)?;
    let mut extra = existing
        .and_then(|c| c.extra_config)
        .unwrap_or_default();
    extra.exclude_prereleases = exclude;
    set_extra_config(conn, collection, &extra)
}

/// Set the extra config JSON for a collection
fn set_extra_config(conn: &Connection, collection: &str, extra: &ExtraConfig) -> Result<()> {
    let json = serde_json::to_string(extra)?;
    conn.execute(
        "INSERT INTO dat_config (path_pattern, config_json)
         VALUES (?, ?)
         ON CONFLICT(path_pattern) DO UPDATE SET config_json = excluded.config_json",
        [collection, &json],
    )?;
    Ok(())
}

/// Get the extra config for a collection
pub fn get_extra_config(conn: &Connection, collection: &str) -> Result<Option<ExtraConfig>> {
    let config = get_collection_config(conn, collection)?;
    Ok(config.and_then(|c| c.extra_config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_set_and_get_dest_path() {
        let db = setup_test_db();
        let conn = db.conn();

        // Initially no config
        let config = get_collection_config(conn, "Test Collection").unwrap();
        assert!(config.is_none());

        // Set dest_path
        set_dest_path(conn, "Test Collection", "/roms/test").unwrap();

        // Verify it's set
        let config = get_collection_config(conn, "Test Collection")
            .unwrap()
            .unwrap();
        assert_eq!(config.path_pattern, "Test Collection");
        assert_eq!(config.dest_path, Some("/roms/test".to_string()));
        assert!(config.output_format.is_none());
        assert!(config.merge_mode.is_none());
    }

    #[test]
    fn test_set_output_format() {
        let db = setup_test_db();
        let conn = db.conn();

        set_output_format(conn, "MAME", "zip").unwrap();

        let config = get_collection_config(conn, "MAME").unwrap().unwrap();
        assert_eq!(config.output_format, Some("zip".to_string()));
    }

    #[test]
    fn test_set_merge_mode() {
        let db = setup_test_db();
        let conn = db.conn();

        set_merge_mode(conn, "MAME", "merged").unwrap();

        let config = get_collection_config(conn, "MAME").unwrap().unwrap();
        assert_eq!(config.merge_mode, Some("merged".to_string()));
    }

    #[test]
    fn test_update_existing_config() {
        let db = setup_test_db();
        let conn = db.conn();

        // Set initial values
        set_dest_path(conn, "Collection", "/initial").unwrap();
        set_output_format(conn, "Collection", "loose").unwrap();

        // Update dest_path
        set_dest_path(conn, "Collection", "/updated").unwrap();

        let config = get_collection_config(conn, "Collection").unwrap().unwrap();
        assert_eq!(config.dest_path, Some("/updated".to_string()));
        assert_eq!(config.output_format, Some("loose".to_string()));
    }

    #[test]
    fn test_list_all_configs() {
        let db = setup_test_db();
        let conn = db.conn();

        // Initially empty
        let configs = list_all_configs(conn).unwrap();
        assert!(configs.is_empty());

        // Add some configs
        set_dest_path(conn, "Collection A", "/a").unwrap();
        set_dest_path(conn, "Collection B", "/b").unwrap();
        set_output_format(conn, "Collection B", "zip").unwrap();

        let configs = list_all_configs(conn).unwrap();
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].path_pattern, "Collection A");
        assert_eq!(configs[1].path_pattern, "Collection B");
    }

    #[test]
    fn test_get_dest_path_helper() {
        let db = setup_test_db();
        let conn = db.conn();

        // Not set
        let path = get_dest_path(conn, "Test").unwrap();
        assert!(path.is_none());

        // Set it
        set_dest_path(conn, "Test", "/roms").unwrap();
        let path = get_dest_path(conn, "Test").unwrap();
        assert_eq!(path, Some("/roms".to_string()));
    }

    #[test]
    fn test_set_one_g_one_r() {
        let db = setup_test_db();
        let conn = db.conn();

        // Enable 1G1R
        set_one_g_one_r(conn, "Test Collection", true).unwrap();

        let extra = get_extra_config(conn, "Test Collection").unwrap().unwrap();
        assert!(extra.one_g_one_r);
        assert!(extra.exclude_modified); // default true
        assert!(extra.exclude_bad_dumps); // default true
    }

    #[test]
    fn test_set_region_priority() {
        let db = setup_test_db();
        let conn = db.conn();

        set_region_priority(
            conn,
            "Japan Collection",
            vec!["Japan".to_string(), "USA".to_string()],
        )
        .unwrap();

        let extra = get_extra_config(conn, "Japan Collection").unwrap().unwrap();
        assert_eq!(extra.region_priority, vec!["Japan", "USA"]);
    }

    #[test]
    fn test_extra_config_to_filter_preferences() {
        let db = setup_test_db();
        let conn = db.conn();

        // Set up config
        set_one_g_one_r(conn, "Test", true).unwrap();
        set_region_priority(conn, "Test", vec!["Japan".to_string()]).unwrap();
        set_exclude_prereleases(conn, "Test", true).unwrap();

        let extra = get_extra_config(conn, "Test").unwrap().unwrap();
        let prefs = extra.to_filter_preferences();

        assert_eq!(prefs.region_priority[0], "Japan");
        assert!(prefs.exclude_prereleases);
        assert!(prefs.exclude_modified);
    }

    #[test]
    fn test_extra_config_preserved_with_other_settings() {
        let db = setup_test_db();
        let conn = db.conn();

        // Set dest_path first
        set_dest_path(conn, "Mixed", "/roms").unwrap();

        // Then add extra config
        set_one_g_one_r(conn, "Mixed", true).unwrap();

        // Both should be present
        let config = get_collection_config(conn, "Mixed").unwrap().unwrap();
        assert_eq!(config.dest_path, Some("/roms".to_string()));
        assert!(config.extra_config.unwrap().one_g_one_r);
    }
}
