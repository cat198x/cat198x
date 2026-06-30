//! Configuration management commands

mod defaults;
mod set;

use anyhow::Result;
use std::path::PathBuf;

use crate::cli::args::ConfigCommands;
use crate::db::config as db_config;

use super::{get_data_dir, open_database};

/// Run a config subcommand
pub fn run(cmd: ConfigCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        ConfigCommands::Set {
            collection,
            key,
            value,
        } => set::run(&collection, &key, &value, data_dir),
        ConfigCommands::SetDefault { key, value } => defaults::set(&key, &value, data_dir),
        ConfigCommands::GetDefault { key } => defaults::get(key.as_deref(), data_dir),
        ConfigCommands::Get { collection, key } => {
            get_config(&collection, key.as_deref(), data_dir)
        }
        ConfigCommands::List { collection } => list_config(collection.as_deref(), data_dir),
    }
}

/// The quarantine store directory: the configured `quarantine_dir`, or
/// `<data_dir>/quarantine` when unset. Shared by every quarantine operation
/// (move, prune, restore) so the store location stays consistent. Delegates to
/// the library resolver [`crate::config::resolve_quarantine_dir`] — the apply
/// engine resolves the same store there — so the CLI and the library never drift.
pub fn resolve_quarantine_dir(data_dir: Option<PathBuf>) -> Result<PathBuf> {
    crate::config::resolve_quarantine_dir(&get_data_dir(data_dir)?)
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
                        let enabled = cfg
                            .extra_config
                            .as_ref()
                            .is_some_and(|e| e.exclude_prereleases);
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
                    println!(
                        "    1g1r:               {}",
                        if extra.one_g_one_r { "on" } else { "off" }
                    );
                    if !extra.region_priority.is_empty() {
                        println!(
                            "    regions:            {}",
                            extra.region_priority.join(", ")
                        );
                    }
                    println!(
                        "    exclude_modified:   {}",
                        if extra.exclude_modified { "on" } else { "off" }
                    );
                    println!(
                        "    exclude_bad_dumps:  {}",
                        if extra.exclude_bad_dumps { "on" } else { "off" }
                    );
                    println!(
                        "    exclude_prereleases:{}",
                        if extra.exclude_prereleases {
                            "on"
                        } else {
                            "off"
                        }
                    );
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
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();

    if let Some(coll) = collection {
        // Show config for specific collection
        get_config(coll, None, None)?;
    } else {
        // Lead with the library-wide defaults, then the per-collection overrides.
        defaults::get(None, data_dir)?;
        println!();

        // Show all configured collections
        let configs = db_config::list_all_configs(conn)?;

        if configs.is_empty() {
            println!("No collections configured yet.");
            println!();
            println!("Set destination path for a collection with:");
            println!("  cat198x config set <collection> dest_path <path>");
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
            if let Some(ref extra) = cfg.extra_config
                && extra.one_g_one_r
            {
                print!("  1g1r:          on");
                if !extra.region_priority.is_empty() {
                    print!(" ({})", extra.region_priority.join(", "));
                }
                println!();
            }
            println!();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    // Most config behaviour is covered by integration tests (they need a DB).
    // These tests keep the CLI quarantine resolver aligned with config.toml.

    #[test]
    fn resolve_quarantine_dir_defaults_to_data_dir_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        // No config.toml written → unset → falls back to <data_dir>/quarantine.
        let dir = resolve_quarantine_dir(Some(data_dir.clone())).unwrap();
        assert_eq!(dir, data_dir.join("quarantine"));
    }

    #[test]
    fn resolve_quarantine_dir_uses_configured_path() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let config = Config {
            quarantine_dir: Some("/Volumes/Data/Library/Quarantine".to_string()),
            ..Config::default()
        };
        config.save(&data_dir.join("config.toml")).unwrap();

        let dir = resolve_quarantine_dir(Some(data_dir)).unwrap();
        assert_eq!(dir, PathBuf::from("/Volumes/Data/Library/Quarantine"));
    }
}
