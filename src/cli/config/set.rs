use anyhow::Result;
use std::path::PathBuf;

use crate::cli::open_database;
use crate::db::config as db_config;

pub(super) fn run(
    collection: &str,
    key: &str,
    value: &str,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    match key {
        "dest_path" => {
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
        "output_format" => match value.to_lowercase().as_str() {
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
        },
        "merge_mode" => match value.to_lowercase().as_str() {
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
        },
        "1g1r" => match value.to_lowercase().as_str() {
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
        },
        "regions" => {
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
