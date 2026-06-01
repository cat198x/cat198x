//! Initialize Cat198x command

use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing::info;

use crate::config::Config;
use crate::db::Database;

/// Default data directory name
const DATA_DIR_NAME: &str = ".cat198x";

/// Get the data directory path
pub fn get_data_dir(custom_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = custom_path {
        return Ok(path);
    }

    // Use platform-appropriate default location
    let base_dirs = directories::BaseDirs::new()
        .context("Could not determine home directory")?;

    Ok(base_dirs.home_dir().join(DATA_DIR_NAME))
}

/// Run the init command
pub fn run(path: Option<PathBuf>, data_dir: Option<PathBuf>) -> Result<()> {
    let target_dir = get_data_dir(data_dir.or(path))?;

    info!("Initializing Cat198x at {:?}", target_dir);

    // Create the directory structure
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("Failed to create directory: {:?}", target_dir))?;

    std::fs::create_dir_all(target_dir.join("objects/plans"))?;
    std::fs::create_dir_all(target_dir.join("objects/logs"))?;
    std::fs::create_dir_all(target_dir.join("cache"))?;

    // Initialize the database
    let db_path = target_dir.join("db.sqlite");
    let _db = Database::open(&db_path)
        .with_context(|| format!("Failed to create database at {:?}", db_path))?;

    // Create default config if it doesn't exist
    let config_path = target_dir.join("config.toml");
    if !config_path.exists() {
        let config = Config::default();
        config.save(&config_path)?;
    }

    println!("Initialized Cat198x at {}", target_dir.display());
    println!();
    println!("Next steps:");
    println!("  cat198x dat add <path>     Add a DAT file");
    println!("  cat198x source add <path>  Add a ROM source directory");
    println!("  cat198x scan               Scan for ROM files");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_get_data_dir_with_custom_path() {
        let custom = PathBuf::from("/custom/path");
        let result = get_data_dir(Some(custom.clone())).unwrap();
        assert_eq!(result, custom);
    }

    #[test]
    fn test_get_data_dir_default() {
        let result = get_data_dir(None).unwrap();
        assert!(result.to_string_lossy().contains(".cat198x"));
    }

    #[test]
    fn test_init_creates_directory_structure() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("cat198x-test");

        run(Some(target.clone()), None).unwrap();

        // Verify directory structure
        assert!(target.exists(), "Root directory should exist");
        assert!(target.join("objects").exists(), "objects/ should exist");
        assert!(target.join("objects/plans").exists(), "objects/plans/ should exist");
        assert!(target.join("objects/logs").exists(), "objects/logs/ should exist");
        assert!(target.join("cache").exists(), "cache/ should exist");
    }

    #[test]
    fn test_init_creates_database() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("cat198x-test");

        run(Some(target.clone()), None).unwrap();

        let db_path = target.join("db.sqlite");
        assert!(db_path.exists(), "Database file should exist");
        assert!(db_path.metadata().unwrap().len() > 0, "Database should not be empty");
    }

    #[test]
    fn test_init_creates_default_config() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("cat198x-test");

        run(Some(target.clone()), None).unwrap();

        let config_path = target.join("config.toml");
        assert!(config_path.exists(), "Config file should exist");

        // Verify config content
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("default_output_format"), "Config should have output format");
        assert!(content.contains("default_merge_mode"), "Config should have merge mode");
    }

    #[test]
    fn test_init_does_not_overwrite_existing_config() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("cat198x-test");
        std::fs::create_dir_all(&target).unwrap();

        // Create existing config with custom content
        let config_path = target.join("config.toml");
        let custom_content = "# Custom config\ndefault_output_format = \"zip\"\ndefault_merge_mode = \"merged\"\n";
        std::fs::write(&config_path, custom_content).unwrap();

        run(Some(target.clone()), None).unwrap();

        // Verify config was not overwritten
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("# Custom config"), "Config should preserve custom content");
        assert!(content.contains("\"zip\""), "Config should preserve zip format");
    }

    #[test]
    fn test_init_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("cat198x-test");

        // Run init twice
        run(Some(target.clone()), None).unwrap();
        run(Some(target.clone()), None).unwrap();

        // Should still work and have valid structure
        assert!(target.join("db.sqlite").exists());
        assert!(target.join("config.toml").exists());
        assert!(target.join("objects/plans").exists());
    }

    #[test]
    fn test_init_with_data_dir_override() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().join("custom-data");

        // path is ignored when data_dir is provided
        run(Some(PathBuf::from("/ignored/path")), Some(data_dir.clone())).unwrap();

        assert!(data_dir.exists(), "Data dir should be used");
        assert!(data_dir.join("db.sqlite").exists());
    }

    #[test]
    fn test_database_has_schema() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("cat198x-test");

        run(Some(target.clone()), None).unwrap();

        // Open database and verify tables exist
        let db_path = target.join("db.sqlite");
        let db = Database::open(&db_path).unwrap();
        let conn = db.conn();

        // Query for tables
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"collections".to_string()), "Should have collections table");
        assert!(tables.contains(&"files".to_string()), "Should have files table");
        assert!(tables.contains(&"sources".to_string()), "Should have sources table");
        assert!(tables.contains(&"dat_games".to_string()), "Should have dat_games table");
        assert!(tables.contains(&"dat_roms".to_string()), "Should have dat_roms table");
    }
}
