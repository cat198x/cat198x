use anyhow::Result;
use std::path::PathBuf;

use crate::config::{Config, MergeMode, OutputFormat};

use super::get_data_dir;

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

/// Print the library-wide defaults (all keys, or one).
pub(super) fn get(key: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
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

/// Set a library-wide default in `config.toml`, creating it if absent.
pub(super) fn set(key: &str, value: &str, data_dir: Option<PathBuf>) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
