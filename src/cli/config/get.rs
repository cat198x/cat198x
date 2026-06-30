use anyhow::Result;
use std::path::PathBuf;

use crate::db::config as db_config;

use super::open_database;

pub(super) fn run(collection: &str, key: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let config = db_config::get_collection_config(conn, collection)?;

    match config {
        Some(cfg) => {
            if let Some(k) = key {
                print_key(&cfg, k)?;
            } else {
                print_collection(collection, &cfg);
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

fn print_key(cfg: &db_config::CollectionConfig, key: &str) -> Result<()> {
    match key {
        "dest_path" => {
            if let Some(ref v) = cfg.dest_path {
                println!("{}", v);
            } else {
                println!("(not set)");
            }
        }
        "output_format" => {
            if let Some(ref v) = cfg.output_format {
                println!("{}", v);
            } else {
                println!("(not set)");
            }
        }
        "merge_mode" => {
            if let Some(ref v) = cfg.merge_mode {
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
        _ => anyhow::bail!("Unknown config key: '{}'", key),
    }
    Ok(())
}

fn print_collection(collection: &str, cfg: &db_config::CollectionConfig) {
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
