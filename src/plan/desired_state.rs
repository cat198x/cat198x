use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::destinations::{
    build_archive_dest_path, build_dest_path, build_disk_dest_path, resolve_dest_root,
};
use super::generator::PlanOptions;
use super::matching::{MatchedRom, count_match_rows_capped, find_matched_roms};
use super::rules::{
    MAX_MATCH_ROWS, apply_one_g_one_r_filter, archive_extension, archive_format_tag,
    effective_format, effective_merge_mode, glob_match,
};
use crate::config::MergeMode;
use crate::db::{collections, config as db_config, dats};

/// The library's desired state, derived from the active DATs exactly as the
/// planner derives placement.
///
/// Used by `clean-superseded` to decide which loose files under the library are
/// safe to remove: a loose file may go only when its content is preserved in the
/// canonical archive the active DAT assigns it to, and the file is not itself a
/// desired placement of any collection. This captures both facts without
/// running (and saving) a whole plan.
pub struct DesiredState {
    /// Content SHA1 -> the canonical archive destination paths the active DATs
    /// assign that content to (absolute). Populated only for the SHA1s the caller
    /// passes in `interesting_sha1s`, so the index stays small on a large
    /// library.
    pub archive_homes: HashMap<String, HashSet<String>>,
    /// Every canonical destination path the active DATs designate -- archive,
    /// loose, or disk (absolute). A file sitting at one of these is itself a
    /// desired-state member and must never be removed.
    pub dest_paths: HashSet<String>,
}

/// Compute the desired state across every active collection in scope.
///
/// Mirrors `generate_plan_filtered`'s per-collection resolution (active version,
/// library path, destination root, merge mode, 1G1R filter, output format,
/// matched ROMs) but records *placements* rather than operations:
///
/// - for an archive-format collection, each game's canonical archive
///   `<dest_root>/<game>.<ext>` and -- for the content the caller cares about --
///   the archive it belongs in;
/// - for a loose-format collection, each ROM's canonical loose path;
/// - for any `<disk>`, the loose `<dest_root>/<game>/<name>.chd` path.
///
/// Oversized meta-aggregate collections are skipped exactly as the planner skips
/// them -- they place nothing real.
pub fn compute_desired_state(
    conn: &Connection,
    opts: &PlanOptions,
    interesting_sha1s: &HashSet<String>,
) -> Result<DesiredState> {
    let mut state = DesiredState {
        archive_homes: HashMap::new(),
        dest_paths: HashSet::new(),
    };

    for collection in collections::list_collections(conn)? {
        if let Some(pattern) = opts.dat_filter.as_deref()
            && !glob_match(pattern, &collection.name)
        {
            continue;
        }
        let version = match collections::get_active_version(conn, collection.id)? {
            Some(v) => v,
            None => continue,
        };
        let cfg = db_config::get_collection_config(conn, &collection.name)?;
        let hierarchy =
            dats::primary_node_path(conn, version.id)?.unwrap_or_else(|| collection.name.clone());

        if let Some(sets) = opts.set_filter.as_ref() {
            let set = hierarchy.split('/').next().unwrap_or(hierarchy.as_str());
            if !sets.iter().any(|s| s == set) {
                continue;
            }
        }

        let explicit = cfg.as_ref().and_then(|c| c.dest_path.as_deref());
        let dest_root = match resolve_dest_root(explicit, opts.default_dest.as_deref(), &hierarchy)?
        {
            Some(root) => root,
            None => continue,
        };

        if count_match_rows_capped(conn, version.id, MAX_MATCH_ROWS)? > MAX_MATCH_ROWS {
            continue;
        }

        let merge_mode = effective_merge_mode(conn, opts, cfg.as_ref(), &hierarchy)?;
        let matches = find_matched_roms(
            conn,
            version.id,
            &collection.name,
            merge_mode == MergeMode::Split,
        )?;
        let matches = match cfg.as_ref().and_then(|c| c.extra_config.as_ref()) {
            Some(extra) if extra.one_g_one_r => {
                apply_one_g_one_r_filter(&matches, &extra.to_filter_preferences())
            }
            _ => matches,
        };
        let format = effective_format(conn, opts, cfg.as_ref(), &hierarchy)?;
        let (disk_matches, rom_matches): (Vec<MatchedRom>, Vec<MatchedRom>) =
            matches.into_iter().partition(|m| m.is_disk);
        for m in &disk_matches {
            state
                .dest_paths
                .insert(build_disk_dest_path(&dest_root, &m.game_name, &m.rom_name)?);
        }

        match archive_format_tag(format) {
            Some(tag) => {
                let ext = archive_extension(tag);
                let mut by_game: BTreeMap<&str, Vec<&MatchedRom>> = BTreeMap::new();
                for m in &rom_matches {
                    by_game.entry(m.game_name.as_str()).or_default().push(m);
                }
                for (game_name, gmatches) in by_game {
                    let dest = build_archive_dest_path(&dest_root, game_name, ext)?;
                    let mut seen = HashSet::new();
                    for m in gmatches {
                        if seen.insert((m.rom_name.as_str(), m.sha1.as_str()))
                            && interesting_sha1s.contains(&m.sha1)
                        {
                            state
                                .archive_homes
                                .entry(m.sha1.clone())
                                .or_default()
                                .insert(dest.clone());
                        }
                    }
                    state.dest_paths.insert(dest);
                }
            }
            None => {
                let mut roms_per_game: HashMap<&str, HashSet<&str>> = HashMap::new();
                for m in &rom_matches {
                    roms_per_game
                        .entry(m.game_name.as_str())
                        .or_default()
                        .insert(m.rom_name.as_str());
                }
                for m in &rom_matches {
                    let multi = roms_per_game
                        .get(m.game_name.as_str())
                        .map(|s| s.len())
                        .unwrap_or(1)
                        > 1;
                    state.dest_paths.insert(build_dest_path(
                        &dest_root,
                        &m.game_name,
                        &m.rom_name,
                        multi,
                    )?);
                }
            }
        }
    }

    Ok(state)
}
