//! Configuration management commands

use anyhow::Result;
use std::path::PathBuf;

use crate::ConfigCommands;
use crate::config::{Config, MergeMode, OutputFormat};
use crate::db::config as db_config;

use super::{get_data_dir, open_database};

/// Run a config subcommand
pub fn run(cmd: ConfigCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        ConfigCommands::Set {
            collection,
            key,
            value,
        } => set_config(&collection, &key, &value, data_dir),
        ConfigCommands::SetDefault { key, value } => set_default(&key, &value, data_dir),
        ConfigCommands::GetDefault { key } => get_default(key.as_deref(), data_dir),
        ConfigCommands::Get { collection, key } => {
            get_config(&collection, key.as_deref(), data_dir)
        }
        ConfigCommands::List { collection } => list_config(collection.as_deref(), data_dir),
    }
}

/// Load the library-wide config from `config.toml`, returning its path and the
/// parsed config (defaults if the file does not exist yet).
fn load_file_config(data_dir: Option<PathBuf>) -> Result<(PathBuf, Config)> {
    let path = get_data_dir(data_dir)?.join("config.toml");
    let config = if path.exists() {
        Config::load(&path)?
    } else {
        Config::default()
    };
    Ok((path, config))
}

/// The quarantine store directory: the configured `quarantine_dir`, or
/// `<data_dir>/quarantine` when unset. Shared by every quarantine operation
/// (move, prune, restore) so the store location stays consistent.
pub fn resolve_quarantine_dir(data_dir: Option<PathBuf>) -> Result<PathBuf> {
    let (_, config) = load_file_config(data_dir.clone())?;
    match config.quarantine_dir {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => Ok(get_data_dir(data_dir)?.join("quarantine")),
    }
}

/// The canonical lowercase string for an output format.
fn output_format_str(f: OutputFormat) -> &'static str {
    match f {
        OutputFormat::Loose => "loose",
        OutputFormat::Zip => "zip",
        OutputFormat::TorrentZip => "torrentzip",
        OutputFormat::SevenZip => "7z",
    }
}

/// The canonical string for a merge mode.
fn merge_mode_str(m: MergeMode) -> &'static str {
    match m {
        MergeMode::NonMerged => "non-merged",
        MergeMode::Merged => "merged",
        MergeMode::Split => "split",
    }
}

/// Print the library-wide defaults (all keys, or one).
fn get_default(key: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
    let (_, config) = load_file_config(data_dir)?;

    let dest = config.default_dest_path.as_deref().unwrap_or("(not set)");
    let quarantine = config
        .quarantine_dir
        .as_deref()
        .unwrap_or("(default: <data_dir>/quarantine)");
    let format = output_format_str(config.default_output_format);
    let mode = merge_mode_str(config.default_merge_mode);

    match key {
        Some("dest_path") => println!("{}", dest),
        Some("quarantine_dir") => println!("{}", quarantine),
        Some("output_format") => println!("{}", format),
        Some("merge_mode") => println!("{}", mode),
        Some(other) => anyhow::bail!(
            "Unknown default key: '{}'\n  Valid keys: dest_path, quarantine_dir, output_format, merge_mode",
            other
        ),
        None => {
            println!("Library-wide defaults:");
            println!("  dest_path:      {}", dest);
            println!("  quarantine_dir: {}", quarantine);
            println!("  output_format:  {}", format);
            println!("  merge_mode:     {}", mode);
        }
    }
    Ok(())
}

/// Apply a library-wide default to the in-memory `Config`, validating the key
/// and value. Pure (no I/O) so the key/value mapping is unit-testable.
fn set_default_field(config: &mut Config, key: &str, value: &str) -> Result<()> {
    match key {
        "dest_path" => config.default_dest_path = Some(value.to_string()),
        "quarantine_dir" => config.quarantine_dir = Some(value.to_string()),
        "output_format" => {
            config.default_output_format = match value.to_lowercase().as_str() {
                "loose" => OutputFormat::Loose,
                "zip" => OutputFormat::Zip,
                "torrentzip" => OutputFormat::TorrentZip,
                "7z" => OutputFormat::SevenZip,
                _ => anyhow::bail!(
                    "Invalid output_format: '{}'\n  Valid options: loose, zip, torrentzip, 7z",
                    value
                ),
            };
        }
        "merge_mode" => {
            config.default_merge_mode = match value.to_lowercase().as_str() {
                "non-merged" => MergeMode::NonMerged,
                "merged" => MergeMode::Merged,
                "split" => MergeMode::Split,
                _ => anyhow::bail!(
                    "Invalid merge_mode: '{}'\n  Valid options: non-merged, merged, split",
                    value
                ),
            };
        }
        _ => anyhow::bail!(
            "Unknown default key: '{}'\n  Valid keys: dest_path, quarantine_dir, output_format, merge_mode",
            key
        ),
    }
    Ok(())
}

/// Set a library-wide default in `config.toml`, creating it if absent.
fn set_default(key: &str, value: &str, data_dir: Option<PathBuf>) -> Result<()> {
    let (config_path, mut config) = load_file_config(data_dir)?;

    set_default_field(&mut config, key, value)?;

    // A not-yet-existing destination is fine: `apply` creates it.
    if key == "dest_path" && !PathBuf::from(value).exists() {
        println!(
            "Warning: Path does not exist yet: {}\n  It will be created when running 'cat198x apply'.",
            value
        );
    }

    config.save(&config_path)?;
    println!("Set default {} to: {}", key, value);
    Ok(())
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
                     It will be created when running 'cat198x apply'.",
                    path.display()
                );
            }
            db_config::set_dest_path(conn, collection, value)?;
            println!("Set dest_path for '{}' to: {}", collection, value);
        }
        "output_format" => {
            // Validate format
            match value.to_lowercase().as_str() {
                "loose" | "zip" | "torrentzip" | "7z" => {
                    db_config::set_output_format(conn, collection, value)?;
                    println!("Set output_format for '{}' to: {}", collection, value);
                }
                _ => {
                    anyhow::bail!(
                        "Invalid output_format: '{}'\n\
                         Valid options: loose, zip, torrentzip, 7z",
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
        "exclude_prereleases" => match value.to_lowercase().as_str() {
            "on" | "true" | "yes" | "1" => {
                db_config::set_exclude_prereleases(conn, collection, true)?;
                println!(
                    "Enabled prerelease exclusion for '{}' (betas, protos, demos)",
                    collection
                );
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
        },
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
        get_default(None, data_dir)?;
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

    // Most config behaviour is covered by integration tests (they need a DB).
    // The library-wide default mapping is pure, so it is unit-tested here.

    #[test]
    fn set_default_field_sets_dest_path() {
        let mut config = Config::default();
        set_default_field(&mut config, "dest_path", "/Volumes/Data").unwrap();
        assert_eq!(config.default_dest_path.as_deref(), Some("/Volumes/Data"));
    }

    #[test]
    fn set_default_field_sets_quarantine_dir() {
        let mut config = Config::default();
        assert_eq!(config.quarantine_dir, None);
        set_default_field(
            &mut config,
            "quarantine_dir",
            "/Volumes/Data/Library/Quarantine",
        )
        .unwrap();
        assert_eq!(
            config.quarantine_dir.as_deref(),
            Some("/Volumes/Data/Library/Quarantine")
        );
    }

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
        let mut config = Config::default();
        set_default_field(
            &mut config,
            "quarantine_dir",
            "/Volumes/Data/Library/Quarantine",
        )
        .unwrap();
        config.save(&data_dir.join("config.toml")).unwrap();

        let dir = resolve_quarantine_dir(Some(data_dir)).unwrap();
        assert_eq!(dir, PathBuf::from("/Volumes/Data/Library/Quarantine"));
    }

    #[test]
    fn set_default_field_parses_output_format_and_merge_mode() {
        let mut config = Config::default();
        set_default_field(&mut config, "output_format", "torrentzip").unwrap();
        assert_eq!(config.default_output_format, OutputFormat::TorrentZip);

        set_default_field(&mut config, "merge_mode", "split").unwrap();
        assert_eq!(config.default_merge_mode, MergeMode::Split);
    }

    #[test]
    fn set_default_field_rejects_unknown_key_and_bad_value() {
        let mut config = Config::default();
        assert!(set_default_field(&mut config, "nonsense", "x").is_err());
        assert!(set_default_field(&mut config, "output_format", "rar").is_err());
        assert!(set_default_field(&mut config, "merge_mode", "fused").is_err());
    }

    #[test]
    fn format_strings_round_trip_with_the_setter() {
        // The display strings match what set_default_field accepts, so
        // get-default output can be fed back to set-default.
        let mut config = Config::default();
        for v in ["loose", "zip", "torrentzip"] {
            set_default_field(&mut config, "output_format", v).unwrap();
            assert_eq!(output_format_str(config.default_output_format), v);
        }
        for v in ["non-merged", "merged", "split"] {
            set_default_field(&mut config, "merge_mode", v).unwrap();
            assert_eq!(merge_mode_str(config.default_merge_mode), v);
        }
    }
}
