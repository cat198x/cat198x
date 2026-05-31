//! Configuration management commands

use anyhow::Result;
use std::path::PathBuf;

use crate::db::config as db_config;
use crate::ConfigCommands;

use super::open_database;

/// Run a config subcommand
pub fn run(cmd: ConfigCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        ConfigCommands::Set {
            collection,
            key,
            value,
        } => set_config(&collection, &key, &value, data_dir),
        ConfigCommands::Get { collection, key } => get_config(&collection, key.as_deref(), data_dir),
        ConfigCommands::List { collection } => list_config(collection.as_deref(), data_dir),
    }
}

fn set_config(collection: &str, key: &str, value: &str, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Validate the key
    match key {
        "dest_path" => {
            // Validate path exists (or can be created)
            let path = PathBuf::from(value);
            if !path.exists() {
                println!(
                    "Warning: Path does not exist yet: {}\n\
                     It will be created when running 'romshelf apply'.",
                    path.display()
                );
            }
            db_config::set_dest_path(conn, collection, value)?;
            println!("Set dest_path for '{}' to: {}", collection, value);
        }
        "output_format" => {
            // Validate format
            match value.to_lowercase().as_str() {
                "loose" | "zip" | "torrentzip" => {
                    db_config::set_output_format(conn, collection, value)?;
                    println!("Set output_format for '{}' to: {}", collection, value);
                }
                _ => {
                    anyhow::bail!(
                        "Invalid output_format: '{}'\n\
                         Valid options: loose, zip, torrentzip",
                        value
                    );
                }
            }
        }
        "merge_mode" => {
            // Validate merge mode
            match value.to_lowercase().as_str() {
                "non-merged" | "merged" | "split" => {
                    db_config::set_merge_mode(conn, collection, value)?;
                    println!("Set merge_mode for '{}' to: {}", collection, value);
                }
                _ => {
                    anyhow::bail!(
                        "Invalid merge_mode: '{}'\n\
                         Valid options: non-merged, merged, split",
                        value
                    );
                }
            }
        }
        "1g1r" => {
            // Enable/disable 1G1R filtering
            match value.to_lowercase().as_str() {
                "on" | "true" | "yes" | "1" | "enable" => {
                    db_config::set_one_g_one_r(conn, collection, true)?;
                    println!("Enabled 1G1R filtering for '{}'", collection);
                    println!("  (One Game One ROM - selects best regional variant)");
                }
                "off" | "false" | "no" | "0" | "disable" => {
                    db_config::set_one_g_one_r(conn, collection, false)?;
                    println!("Disabled 1G1R filtering for '{}'", collection);
                }
                _ => {
                    anyhow::bail!(
                        "Invalid 1g1r value: '{}'\n\
                         Valid options: on, off (or true/false, yes/no, enable/disable)",
                        value
                    );
                }
            }
        }
        "regions" => {
            // Set region priority (comma-separated list)
            let regions: Vec<String> = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if regions.is_empty() {
                anyhow::bail!("At least one region is required");
            }
            db_config::set_region_priority(conn, collection, regions.clone())?;
            println!("Set region priority for '{}' to:", collection);
            for (i, region) in regions.iter().enumerate() {
                println!("  {}. {}", i + 1, region);
            }
        }
        "exclude_prereleases" => {
            match value.to_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => {
                    db_config::set_exclude_prereleases(conn, collection, true)?;
                    println!("Enabled prerelease exclusion for '{}' (betas, protos, demos)", collection);
                }
                "off" | "false" | "no" | "0" => {
                    db_config::set_exclude_prereleases(conn, collection, false)?;
                    println!("Disabled prerelease exclusion for '{}'", collection);
                }
                _ => {
                    anyhow::bail!(
                        "Invalid exclude_prereleases value: '{}'\n\
                         Valid options: on, off (or true/false, yes/no)",
                        value
                    );
                }
            }
        }
        _ => {
            anyhow::bail!(
                "Unknown config key: '{}'\n\
                 Valid keys: dest_path, output_format, merge_mode, 1g1r, regions, exclude_prereleases",
                key
            );
        }
    }

    Ok(())
}

fn get_config(collection: &str, key: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let config = db_config::get_collection_config(conn, collection)?;

    match config {
        Some(cfg) => {
            if let Some(k) = key {
                // Show specific key
                match k {
                    "dest_path" => {
                        if let Some(v) = cfg.dest_path {
                            println!("{}", v);
                        } else {
                            println!("(not set)");
                        }
                    }
                    "output_format" => {
                        if let Some(v) = cfg.output_format {
                            println!("{}", v);
                        } else {
                            println!("(not set)");
                        }
                    }
                    "merge_mode" => {
                        if let Some(v) = cfg.merge_mode {
                            println!("{}", v);
                        } else {
                            println!("(not set)");
                        }
                    }
                    "1g1r" => {
                        let enabled = cfg.extra_config.as_ref().is_some_and(|e| e.one_g_one_r);
                        println!("{}", if enabled { "on" } else { "off" });
                    }
                    "regions" => {
                        if let Some(ref extra) = cfg.extra_config {
                            if !extra.region_priority.is_empty() {
                                println!("{}", extra.region_priority.join(", "));
                            } else {
                                println!("(default)");
                            }
                        } else {
                            println!("(default)");
                        }
                    }
                    "exclude_prereleases" => {
                        let enabled = cfg.extra_config.as_ref().is_some_and(|e| e.exclude_prereleases);
                        println!("{}", if enabled { "on" } else { "off" });
                    }
                    _ => anyhow::bail!("Unknown config key: '{}'", k),
                }
            } else {
                // Show all keys for collection
                println!("Configuration for '{}':", collection);
                println!(
                    "  dest_path:     {}",
                    cfg.dest_path.as_deref().unwrap_or("(not set)")
                );
                println!(
                    "  output_format: {}",
                    cfg.output_format.as_deref().unwrap_or("(not set)")
                );
                println!(
                    "  merge_mode:    {}",
                    cfg.merge_mode.as_deref().unwrap_or("(not set)")
                );

                // Show filter settings if any are configured
                if let Some(ref extra) = cfg.extra_config {
                    println!();
                    println!("  Filtering:");
                    println!("    1g1r:               {}", if extra.one_g_one_r { "on" } else { "off" });
                    if !extra.region_priority.is_empty() {
                        println!("    regions:            {}", extra.region_priority.join(", "));
                    }
                    println!("    exclude_modified:   {}", if extra.exclude_modified { "on" } else { "off" });
                    println!("    exclude_bad_dumps:  {}", if extra.exclude_bad_dumps { "on" } else { "off" });
                    println!("    exclude_prereleases:{}", if extra.exclude_prereleases { "on" } else { "off" });
                }
            }
        }
        None => {
            if key.is_some() {
                println!("(not set)");
            } else {
                println!("No configuration set for '{}'", collection);
            }
        }
    }

    Ok(())
}

fn list_config(collection: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    if let Some(coll) = collection {
        // Show config for specific collection
        get_config(coll, None, None)?;
    } else {
        // Show all configured collections
        let configs = db_config::list_all_configs(conn)?;

        if configs.is_empty() {
            println!("No collections configured yet.");
            println!();
            println!("Set destination path for a collection with:");
            println!("  romshelf config set <collection> dest_path <path>");
            return Ok(());
        }

        println!("Configured collections:");
        println!();

        for cfg in &configs {
            println!("{}:", cfg.path_pattern);
            if let Some(ref dest) = cfg.dest_path {
                println!("  dest_path:     {}", dest);
            }
            if let Some(ref format) = cfg.output_format {
                println!("  output_format: {}", format);
            }
            if let Some(ref mode) = cfg.merge_mode {
                println!("  merge_mode:    {}", mode);
            }
            if let Some(ref extra) = cfg.extra_config {
                if extra.one_g_one_r {
                    print!("  1g1r:          on");
                    if !extra.region_priority.is_empty() {
                        print!(" ({})", extra.region_priority.join(", "));
                    }
                    println!();
                }
            }
            println!();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    // Integration tests cover most of the config functionality
    // Unit tests would require mocking the database
}
