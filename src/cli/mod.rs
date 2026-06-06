//! CLI command implementations

pub mod apply;
pub mod config;
pub mod dat;
pub mod doctor;
pub mod export;
pub mod fetch;
pub mod init;
pub mod plan;
pub mod quarantine;
pub mod scan;
pub mod source;
pub mod stats;
pub mod status;
pub mod torrent;
pub mod unknowns;
pub mod update;

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::db::Database;

/// Get the data directory, checking in order:
/// 1. Explicit --data-dir argument
/// 2. Default location (~/.cat198x)
pub fn get_data_dir(data_dir: Option<PathBuf>) -> Result<PathBuf> {
    init::get_data_dir(data_dir)
}

/// Open the database from the data directory
pub fn open_database(data_dir: Option<PathBuf>) -> Result<Database> {
    let dir = get_data_dir(data_dir)?;
    let db_path = dir.join("db.sqlite");

    if !db_path.exists() {
        anyhow::bail!(
            "Cat198x not initialized. Run 'cat198x init' first.\n\
             Expected database at: {}",
            db_path.display()
        );
    }

    Database::open(&db_path).with_context(|| format!("Failed to open database at {:?}", db_path))
}
