//! Doctor command - health checks for Cat198x installation

use anyhow::Result;
use std::path::PathBuf;

use crate::db::collections::{list_collections, list_versions};
use crate::db::files::list_sources;

use super::{get_data_dir, open_database};

/// Health check result
#[derive(Debug)]
struct Check {
    name: String,
    status: CheckStatus,
    details: Option<String>,
}

#[derive(Debug, PartialEq)]
enum CheckStatus {
    Ok,
    Warning,
    Error,
}

impl Check {
    fn ok(name: &str) -> Self {
        Self {
            name: name.to_string(),
            status: CheckStatus::Ok,
            details: None,
        }
    }

    fn warning(name: &str, details: &str) -> Self {
        Self {
            name: name.to_string(),
            status: CheckStatus::Warning,
            details: Some(details.to_string()),
        }
    }

    fn error(name: &str, details: &str) -> Self {
        Self {
            name: name.to_string(),
            status: CheckStatus::Error,
            details: Some(details.to_string()),
        }
    }

    fn status_icon(&self) -> &str {
        match self.status {
            CheckStatus::Ok => "[OK]",
            CheckStatus::Warning => "[WARN]",
            CheckStatus::Error => "[ERR]",
        }
    }
}

/// Run doctor checks
pub fn run(fix: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let mut checks = Vec::new();

    // Check 1: Data directory exists
    let data_dir_result = get_data_dir(data_dir.clone());
    match &data_dir_result {
        Ok(dir) => {
            if dir.exists() {
                checks.push(Check::ok("Data directory exists"));
            } else {
                checks.push(Check::error(
                    "Data directory exists",
                    &format!("Not found: {}", dir.display()),
                ));
            }
        }
        Err(e) => {
            checks.push(Check::error(
                "Data directory exists",
                &format!("Could not determine: {}", e),
            ));
        }
    }

    // Check 2: Database can be opened
    let db_result = open_database(data_dir.clone());
    match &db_result {
        Ok(_) => {
            checks.push(Check::ok("Database accessible"));
        }
        Err(e) => {
            checks.push(Check::error("Database accessible", &e.to_string()));
        }
    }

    // Only continue with database checks if we have a connection
    if let Ok(db) = &db_result {
        let conn = db.conn();

        // Check 3: Database integrity
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap_or_else(|_| "error".to_string());

        if integrity == "ok" {
            checks.push(Check::ok("Database integrity"));
        } else {
            checks.push(Check::error("Database integrity", &integrity));
        }

        // Check 4: Collections have active versions
        let collections = list_collections(conn)?;
        let mut orphaned_collections = Vec::new();

        for collection in &collections {
            let versions = list_versions(conn, collection.id)?;
            let has_active = versions.iter().any(|v| v.is_active);

            if !versions.is_empty() && !has_active {
                orphaned_collections.push(collection.name.clone());
            }
        }

        if orphaned_collections.is_empty() {
            checks.push(Check::ok("All collections have active versions"));
        } else {
            checks.push(Check::warning(
                "All collections have active versions",
                &format!(
                    "{} collection(s) without active version: {}",
                    orphaned_collections.len(),
                    orphaned_collections.join(", ")
                ),
            ));

            // Fix if requested
            if fix {
                for collection in &collections {
                    let versions = list_versions(conn, collection.id)?;
                    let has_active = versions.iter().any(|v| v.is_active);

                    if !versions.is_empty() && !has_active {
                        // Activate the most recent version
                        if let Some(latest) = versions.first() {
                            crate::db::collections::activate_version(
                                conn,
                                collection.id,
                                &latest.version,
                            )?;
                            println!(
                                "  Fixed: Activated version '{}' for '{}'",
                                latest.version, collection.name
                            );
                        }
                    }
                }
            }
        }

        // Check 5: Source directories exist
        let sources = list_sources(conn)?;
        let mut missing_sources = Vec::new();

        for source in &sources {
            if !std::path::Path::new(&source.path).exists() {
                missing_sources.push(source.path.clone());
            }
        }

        if missing_sources.is_empty() {
            if sources.is_empty() {
                checks.push(Check::warning(
                    "Source directories exist",
                    "No source directories configured",
                ));
            } else {
                checks.push(Check::ok("Source directories exist"));
            }
        } else {
            checks.push(Check::warning(
                "Source directories exist",
                &format!(
                    "{} source(s) not found: {}",
                    missing_sources.len(),
                    missing_sources.join(", ")
                ),
            ));
        }

        // Check 6: DAT file paths are accessible
        let mut missing_dats = Vec::new();
        for collection in &collections {
            let versions = list_versions(conn, collection.id)?;
            for version in versions {
                if !std::path::Path::new(&version.dat_path).exists() {
                    missing_dats.push(format!("{}:{}", collection.name, version.version));
                }
            }
        }

        if missing_dats.is_empty() {
            checks.push(Check::ok("DAT files accessible"));
        } else {
            checks.push(Check::warning(
                "DAT files accessible",
                &format!(
                    "{} DAT file(s) not found: {}",
                    missing_dats.len(),
                    if missing_dats.len() > 3 {
                        format!(
                            "{}, ... and {} more",
                            missing_dats[..3].join(", "),
                            missing_dats.len() - 3
                        )
                    } else {
                        missing_dats.join(", ")
                    }
                ),
            ));
        }

        // Check 7: No orphaned games (games without a version)
        let orphaned_games: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dat_games WHERE version_id NOT IN
                 (SELECT id FROM collection_versions)",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if orphaned_games == 0 {
            checks.push(Check::ok("No orphaned game records"));
        } else {
            checks.push(Check::warning(
                "No orphaned game records",
                &format!("{} game(s) without valid version reference", orphaned_games),
            ));

            if fix {
                conn.execute(
                    "DELETE FROM dat_games WHERE version_id NOT IN
                     (SELECT id FROM collection_versions)",
                    [],
                )?;
                println!("  Fixed: Removed {} orphaned game records", orphaned_games);
            }
        }

        // Check 8: No orphaned ROMs (ROMs without a game)
        let orphaned_roms: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dat_roms WHERE game_id NOT IN
                 (SELECT id FROM dat_games)",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if orphaned_roms == 0 {
            checks.push(Check::ok("No orphaned ROM records"));
        } else {
            checks.push(Check::warning(
                "No orphaned ROM records",
                &format!("{} ROM(s) without valid game reference", orphaned_roms),
            ));

            if fix {
                conn.execute(
                    "DELETE FROM dat_roms WHERE game_id NOT IN
                     (SELECT id FROM dat_games)",
                    [],
                )?;
                println!("  Fixed: Removed {} orphaned ROM records", orphaned_roms);
            }
        }
    }

    // Print results
    println!("Cat198x Health Check");
    println!("=====================\n");

    let mut errors = 0;
    let mut warnings = 0;

    for check in &checks {
        let status_str = check.status_icon();
        print!("{} {}", status_str, check.name);

        if let Some(details) = &check.details {
            print!(": {}", details);
        }
        println!();

        match check.status {
            CheckStatus::Error => errors += 1,
            CheckStatus::Warning => warnings += 1,
            CheckStatus::Ok => {}
        }
    }

    println!();

    if errors > 0 {
        println!("Found {} error(s) and {} warning(s)", errors, warnings);
        if !fix {
            println!("Run with --fix to attempt automatic repairs");
        }
    } else if warnings > 0 {
        println!("Found {} warning(s)", warnings);
        if !fix {
            println!("Run with --fix to attempt automatic repairs");
        }
    } else {
        println!("All checks passed!");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn test_check_ok() {
        let check = Check::ok("Test check");
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.details.is_none());
    }

    #[test]
    fn test_check_warning() {
        let check = Check::warning("Test check", "Some warning");
        assert_eq!(check.status, CheckStatus::Warning);
        assert_eq!(check.details.as_deref(), Some("Some warning"));
    }

    #[test]
    fn test_check_error() {
        let check = Check::error("Test check", "Some error");
        assert_eq!(check.status, CheckStatus::Error);
        assert_eq!(check.details.as_deref(), Some("Some error"));
    }

    #[test]
    fn test_status_icons() {
        assert_eq!(Check::ok("").status_icon(), "[OK]");
        assert_eq!(Check::warning("", "").status_icon(), "[WARN]");
        assert_eq!(Check::error("", "").status_icon(), "[ERR]");
    }

    #[test]
    fn test_database_integrity_check() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();

        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }
}
