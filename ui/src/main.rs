//! Cat198x desktop UI (Tauri) — the read-only status + plan-diff slice.
//!
//! A thin client over the shared operation surface (`cat198x::ops`): the Tauri
//! commands carry no logic of their own, they invoke the same operations the CLI
//! formats and the `cat198x mcp` server exposes. See
//! `decisions/agent-native-surface-and-ui.md` — one surface, three adapters.
//!
//! This first slice is read-only: collection completeness (`status`) and the
//! saved plan-as-diff (`plan_diff`). Mutating actions (apply, reclaim,
//! clean-superseded) land once the operation surface grows progress events.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::Path;

use cat198x::db::Database;
use cat198x::db::dats::MergeMode;
use cat198x::ops::{self, CollectionStatus};
use cat198x::plan::Plan;

/// Map a merge-mode string from the UI to the enum, defaulting to non-merged —
/// the same mapping the other adapters use.
fn parse_merge_mode(s: Option<&str>) -> MergeMode {
    match s {
        Some("split") => MergeMode::Split,
        Some("merged") => MergeMode::Merged,
        _ => MergeMode::NonMerged,
    }
}

/// Collection completeness under `data_dir`, factored out of the Tauri command
/// so it can be tested against a temporary catalogue.
fn compute_status(data_dir: &Path, mode: MergeMode) -> anyhow::Result<Vec<CollectionStatus>> {
    let db = Database::open(&data_dir.join("db.sqlite"))?;
    ops::collection_status(db.conn(), None, mode)
}

/// The catalogue's default data directory (`~/.cat198x`), shared with the CLI.
fn data_dir() -> anyhow::Result<std::path::PathBuf> {
    cat198x::cli::get_data_dir(None)
}

/// Collection completeness against the active DATs.
#[tauri::command]
fn status(merge_mode: Option<String>) -> Result<Vec<CollectionStatus>, String> {
    let mode = parse_merge_mode(merge_mode.as_deref());
    let dir = data_dir().map_err(|e| e.to_string())?;
    compute_status(&dir, mode).map_err(|e| e.to_string())
}

/// The most recent saved plan — the reorganisation as a diff — or `null`.
#[tauri::command]
fn plan_diff() -> Result<Option<Plan>, String> {
    let dir = data_dir().map_err(|e| e.to_string())?;
    ops::latest_plan(&dir).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![status, plan_diff])
        .run(tauri::generate_context!())
        .expect("error while running the Cat198x UI");
}

#[cfg(test)]
mod tests {
    use super::*;
    use cat198x::db::{collections, dats};

    #[test]
    fn parse_merge_mode_defaults_to_non_merged() {
        assert_eq!(parse_merge_mode(Some("split")), MergeMode::Split);
        assert_eq!(parse_merge_mode(Some("merged")), MergeMode::Merged);
        assert_eq!(parse_merge_mode(None), MergeMode::NonMerged);
        assert_eq!(parse_merge_mode(Some("whatever")), MergeMode::NonMerged);
    }

    #[test]
    fn compute_status_reads_completeness_from_the_catalogue() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let db = Database::open(&tmp.path().join("db.sqlite")).unwrap();
            let conn = db.conn();
            let c = collections::create_collection(conn, "NES", "nointro").unwrap();
            let v = collections::add_version(conn, c, "v1", "/d/nes.dat", true).unwrap();
            let node = dats::create_node(conn, v, None, "NES", "dat", "NES").unwrap();
            let g = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
            dats::create_rom(conn, g, "a.nes", 10, Some("AAA"), None, None, "good", None).unwrap();
            dats::create_rom(conn, g, "b.nes", 10, Some("BBB"), None, None, "good", None).unwrap();
            conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
                .unwrap();
        }

        let statuses = compute_status(tmp.path(), MergeMode::NonMerged).unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].name, "NES");
        assert_eq!(statuses[0].have_roms, 1);
        assert_eq!(statuses[0].missing_roms, 1);
    }
}
