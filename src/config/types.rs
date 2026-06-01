//! Configuration types

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Main configuration for Cat198x
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Default output format for organized ROMs
    #[serde(default)]
    pub default_output_format: OutputFormat,

    /// Default merge mode for MAME sets
    #[serde(default)]
    pub default_merge_mode: MergeMode,
}

/// Output format for organized ROM files
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Individual files (not archived)
    #[default]
    Loose,
    /// Standard ZIP archives
    Zip,
    /// TorrentZip format (deterministic)
    TorrentZip,
}

/// Merge mode for MAME-style ROM sets
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MergeMode {
    /// Each game is standalone with all required ROMs
    #[default]
    NonMerged,
    /// Parent contains all clone data
    Merged,
    /// Clones only contain unique files
    Split,
}

impl Config {
    /// Load configuration from a TOML file
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Save configuration to a TOML file
    pub fn save(&self, path: &PathBuf) -> anyhow::Result<()> {
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_config_default() {
        let config = Config::default();

        assert_eq!(config.default_output_format, OutputFormat::Loose);
        assert_eq!(config.default_merge_mode, MergeMode::NonMerged);
    }

    #[test]
    fn test_output_format_default() {
        let format = OutputFormat::default();
        assert_eq!(format, OutputFormat::Loose);
    }

    #[test]
    fn test_merge_mode_default() {
        let mode = MergeMode::default();
        assert_eq!(mode, MergeMode::NonMerged);
    }

    #[test]
    fn test_config_serialize_default() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();

        // Should serialize with lowercase format names
        assert!(toml_str.contains("default_output_format = \"loose\""));
        // Should serialize with kebab-case merge mode
        assert!(toml_str.contains("default_merge_mode = \"non-merged\""));
    }

    #[test]
    fn test_config_serialize_zip_format() {
        let config = Config {
            default_output_format: OutputFormat::Zip,
            default_merge_mode: MergeMode::NonMerged,
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();

        assert!(toml_str.contains("default_output_format = \"zip\""));
    }

    #[test]
    fn test_config_serialize_torrentzip_format() {
        let config = Config {
            default_output_format: OutputFormat::TorrentZip,
            default_merge_mode: MergeMode::NonMerged,
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();

        assert!(toml_str.contains("default_output_format = \"torrentzip\""));
    }

    #[test]
    fn test_config_serialize_merged_mode() {
        let config = Config {
            default_output_format: OutputFormat::Loose,
            default_merge_mode: MergeMode::Merged,
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();

        assert!(toml_str.contains("default_merge_mode = \"merged\""));
    }

    #[test]
    fn test_config_serialize_split_mode() {
        let config = Config {
            default_output_format: OutputFormat::Loose,
            default_merge_mode: MergeMode::Split,
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();

        assert!(toml_str.contains("default_merge_mode = \"split\""));
    }

    #[test]
    fn test_config_deserialize_all_formats() {
        let toml_str = r#"
            default_output_format = "loose"
            default_merge_mode = "non-merged"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_output_format, OutputFormat::Loose);
        assert_eq!(config.default_merge_mode, MergeMode::NonMerged);

        let toml_str = r#"
            default_output_format = "zip"
            default_merge_mode = "merged"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_output_format, OutputFormat::Zip);
        assert_eq!(config.default_merge_mode, MergeMode::Merged);

        let toml_str = r#"
            default_output_format = "torrentzip"
            default_merge_mode = "split"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_output_format, OutputFormat::TorrentZip);
        assert_eq!(config.default_merge_mode, MergeMode::Split);
    }

    #[test]
    fn test_config_deserialize_missing_fields_uses_defaults() {
        // Empty config should use defaults
        let toml_str = "";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_output_format, OutputFormat::Loose);
        assert_eq!(config.default_merge_mode, MergeMode::NonMerged);

        // Partial config should use defaults for missing fields
        let toml_str = r#"default_output_format = "zip""#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_output_format, OutputFormat::Zip);
        assert_eq!(config.default_merge_mode, MergeMode::NonMerged);
    }

    #[test]
    fn test_config_roundtrip() {
        let original = Config {
            default_output_format: OutputFormat::TorrentZip,
            default_merge_mode: MergeMode::Split,
        };

        let toml_str = toml::to_string_pretty(&original).unwrap();
        let deserialized: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(original.default_output_format, deserialized.default_output_format);
        assert_eq!(original.default_merge_mode, deserialized.default_merge_mode);
    }

    #[test]
    fn test_config_save_and_load() {
        let config = Config {
            default_output_format: OutputFormat::Zip,
            default_merge_mode: MergeMode::Merged,
        };

        // Create temp file and save
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        config.save(&path).unwrap();

        // Load and verify
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.default_output_format, OutputFormat::Zip);
        assert_eq!(loaded.default_merge_mode, MergeMode::Merged);
    }

    #[test]
    fn test_config_load_from_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, r#"default_output_format = "torrentzip""#).unwrap();
        writeln!(temp_file, r#"default_merge_mode = "split""#).unwrap();

        let path = temp_file.path().to_path_buf();
        let config = Config::load(&path).unwrap();

        assert_eq!(config.default_output_format, OutputFormat::TorrentZip);
        assert_eq!(config.default_merge_mode, MergeMode::Split);
    }

    #[test]
    fn test_config_load_nonexistent_file() {
        let path = PathBuf::from("/nonexistent/path/config.toml");
        let result = Config::load(&path);

        assert!(result.is_err());
    }

    #[test]
    fn test_config_deserialize_invalid_format() {
        let toml_str = r#"default_output_format = "invalid""#;
        let result: Result<Config, _> = toml::from_str(toml_str);

        assert!(result.is_err());
    }

    #[test]
    fn test_config_deserialize_invalid_merge_mode() {
        let toml_str = r#"default_merge_mode = "invalid""#;
        let result: Result<Config, _> = toml::from_str(toml_str);

        assert!(result.is_err());
    }

    #[test]
    fn test_output_format_copy() {
        // OutputFormat implements Copy
        let format = OutputFormat::Zip;
        let copied = format;
        assert_eq!(format, copied);
    }

    #[test]
    fn test_merge_mode_copy() {
        // MergeMode implements Copy
        let mode = MergeMode::Split;
        let copied = mode;
        assert_eq!(mode, copied);
    }

    #[test]
    fn test_config_clone() {
        // Config implements Clone
        let config = Config {
            default_output_format: OutputFormat::TorrentZip,
            default_merge_mode: MergeMode::Merged,
        };
        let cloned = config.clone();

        assert_eq!(config.default_output_format, cloned.default_output_format);
        assert_eq!(config.default_merge_mode, cloned.default_merge_mode);
    }
}
