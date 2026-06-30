//! DAT file management commands

mod add;
mod catalogue;
mod maintenance;
mod sort;
mod upgrade;

use anyhow::Result;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::cli::args::DatCommands;
use crate::db::dats;

use super::fetch;
use add::{add_dat, add_dats_recursive};
use catalogue::{activate_version, diff_versions, list_dats, list_versions, remove_dat};
use maintenance::{relink_dats, repair_names};
use sort::sort_dats;
use upgrade::upgrade_dat;

#[cfg(test)]
use add::{ImportOutcome, import_dat_file, relative_hierarchy};

#[cfg(test)]
use crate::db::Database;

#[cfg(test)]
use crate::db::collections;

#[cfg(test)]
use sort::sort_segments;

/// Insert one parsed DAT entry, routing a `<disk>` (CHD) to `create_disk` and a
/// `<rom>` to `create_rom`. A disk has no size/crc and its sha1 is the CHD's
/// internal hash, so it takes a distinct insert path.
pub(super) fn insert_dat_entry(
    conn: &rusqlite::Connection,
    game_id: i64,
    rom: &crate::dat::DatRomEntry,
) -> Result<i64> {
    if rom.is_disk {
        dats::create_disk(
            conn,
            game_id,
            &rom.name,
            rom.sha1.as_deref(),
            rom.md5.as_deref(),
            rom.status.as_str(),
            rom.merge.as_deref(),
        )
    } else {
        dats::create_rom(
            conn,
            game_id,
            &rom.name,
            rom.size as i64,
            rom.sha1.as_deref(),
            rom.md5.as_deref(),
            rom.crc32.as_deref(),
            rom.status.as_str(),
            rom.merge.as_deref(),
        )
    }
}

/// Run a DAT subcommand
pub fn run(cmd: DatCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        DatCommands::Add {
            path,
            collection,
            recursive,
        } => {
            if recursive {
                add_dats_recursive(&path, collection.as_deref(), data_dir)
            } else {
                add_dat(&path, collection.as_deref(), data_dir)
            }
        }
        DatCommands::Remove {
            target,
            all_versions,
        } => remove_dat(&target, all_versions, data_dir),
        DatCommands::Relink { dir } => relink_dats(&dir, data_dir),
        DatCommands::RepairNames => repair_names(data_dir),
        DatCommands::Sort { pack, dest } => sort_dats(&pack, &dest),
        DatCommands::List { all } => list_dats(all, data_dir),
        DatCommands::Activate {
            collection,
            version,
        } => activate_version(&collection, &version, data_dir),
        DatCommands::Diff {
            collection,
            from,
            to,
        } => diff_versions(&collection, from.as_deref(), to.as_deref(), data_dir),
        DatCommands::Versions { collection } => list_versions(&collection, data_dir),
        DatCommands::Fetch {
            source,
            url,
            output,
            list,
        } => fetch::run(source.as_deref(), url.as_deref(), output, list, data_dir),
        DatCommands::Upgrade { path, collection } => {
            upgrade_dat(&path, collection.as_deref(), data_dir)
        }
    }
}

/// Collect every `.dat`/`.xml` file under `dir`, sorted for stable output.
fn collect_dat_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("dat") || e.eq_ignore_ascii_case("xml"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}

/// Generate a simple version string from current date (YYYYMMDD)
fn chrono_lite_version() -> String {
    use chrono::{Datelike, Local};
    let now = Local::now();
    format!("{:04}{:02}{:02}", now.year(), now.month(), now.day())
}

#[cfg(test)]
#[path = "dat_tests.rs"]
mod tests;
