//! Configuration management commands

mod defaults;
mod get;
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
        ConfigCommands::Get { collection, key } => get::run(&collection, key.as_deref(), data_dir),
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

fn list_config(collection: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
    if let Some(coll) = collection {
        get::run(coll, None, data_dir)?;
    } else {
        let db = open_database(data_dir.clone())?;
        let conn = db.conn();

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
