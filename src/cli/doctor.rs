//! Doctor command - health checks for Cat198x installation

mod report;

use anyhow::Result;
use std::path::PathBuf;

use crate::config::Config;
use crate::db::collections::{list_collections, list_versions};
use crate::db::dats;
use crate::db::files::list_sources;
use crate::plan::{PlanOptions, find_destination_collisions};

use super::{get_data_dir, open_database};
use report::{Check, print_report};

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
            let listed = if missing_dats.len() > 3 {
                format!(
                    "{}, ... and {} more",
                    missing_dats[..3].join(", "),
                    missing_dats.len() - 3
                )
            } else {
                missing_dats.join(", ")
            };
            checks.push(Check::warning(
                "DAT files accessible",
                &format!(
                    "{} DAT file(s) not found: {}\n         \
                     Re-point them with 'cat198x dat relink <dir>' \
                     (searches <dir> for same-named DATs).",
                    missing_dats.len(),
                    listed
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

        // Check 9: Collections colliding on a destination root. Sibling DATs
        // imported from one directory share a node path, so they resolve to the
        // same destination and a plan refuses (it would overwrite same-named
        // games). --fix nests each non-explicit collider under its own name.
        let config_path = get_data_dir(data_dir.clone())
            .ok()
            .map(|d| d.join("config.toml"));
        let file_config = match &config_path {
            Some(p) if p.exists() => Config::load(p).unwrap_or_default(),
            _ => Config::default(),
        };
        let opts = PlanOptions {
            dat_filter: None,
            set_filter: None,
            default_dest: file_config.default_dest_path,
            default_format: file_config.default_output_format,
            default_merge_mode: file_config.default_merge_mode,
        };
        let collisions = find_destination_collisions(conn, &opts, &collections)?;

        if collisions.is_empty() {
            checks.push(Check::ok("No destination-root collisions"));
        } else {
            let nestable = collisions
                .iter()
                .flat_map(|c| &c.collections)
                .filter(|m| !m.has_explicit_dest)
                .count();
            // A group where every member has an explicit dest can't be nested
            // away (the explicit dest wins) — it needs a manual config change.
            let manual_groups = collisions
                .iter()
                .filter(|c| c.collections.iter().all(|m| m.has_explicit_dest))
                .count();

            let mut detail = format!(
                "{} destination root(s) shared by multiple collections — a plan will refuse.\n",
                collisions.len()
            );
            for c in collisions.iter().take(5) {
                let names: Vec<&str> = c.collections.iter().map(|m| m.name.as_str()).collect();
                let shown = if names.len() > 6 {
                    format!("{}, ... (+{} more)", names[..6].join(", "), names.len() - 6)
                } else {
                    names.join(", ")
                };
                detail.push_str(&format!("         {} <- {}\n", c.root, shown));
            }
            if collisions.len() > 5 {
                detail.push_str(&format!("         ... and {} more\n", collisions.len() - 5));
            }
            detail.push_str(&format!(
                "         {nestable} collection(s) auto-nestable; run with --fix"
            ));
            if manual_groups > 0 {
                detail.push_str(&format!(
                    ". {manual_groups} group(s) need a manual dest_path change \
                     (all members have explicit dests)"
                ));
            }
            checks.push(Check::warning("No destination-root collisions", &detail));

            if fix {
                let mut nested = 0usize;
                for c in &collisions {
                    for member in &c.collections {
                        if !member.has_explicit_dest
                            && let Some(new_path) =
                                dats::nest_primary_node_under_name(conn, member.version_id)?
                        {
                            println!("  Fixed: nested '{}' -> {}", member.name, new_path);
                            nested += 1;
                        }
                    }
                }
                println!(
                    "  Fixed: nested {nested} collection(s) under their own name \
                     (re-run 'cat198x doctor' to confirm)"
                );
            }
        }
    }

    print_report(&checks, fix);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

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
