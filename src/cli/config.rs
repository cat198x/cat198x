//! Configuration management commands

mod defaults;
mod get;
mod list;
mod set;

use anyhow::Result;
use std::path::PathBuf;

use crate::cli::args::ConfigCommands;

use super::get_data_dir;

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
        ConfigCommands::List { collection } => list::run(collection.as_deref(), data_dir),
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
