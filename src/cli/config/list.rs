use anyhow::Result;
use std::path::PathBuf;

use crate::cli::open_database;
use crate::db::config as db_config;

use super::{defaults, get};

pub(super) fn run(collection: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
    if let Some(coll) = collection {
        return get::run(coll, None, data_dir);
    }

    let db = open_database(data_dir.clone())?;
    let conn = db.conn();

    // Lead with the library-wide defaults, then the per-collection overrides.
    defaults::get(None, data_dir)?;
    println!();

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

    Ok(())
}
