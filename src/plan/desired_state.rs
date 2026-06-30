use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use super::collection_matches::{CollectionMatchInputs, load_collection_matches};
use super::collection_scope::{ScopedCollectionResolution, resolve_scoped_collection};
use super::collection_settings::resolve_collection_settings;
use super::desired_state_recording::record_collection_desired_state;
use super::matching::count_match_rows_capped;
use super::options::PlanOptions;
use super::rules::MAX_MATCH_ROWS;
use super::scope::collection_name_matches;
use crate::db::collections;

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
        if !collection_name_matches(&collection.name, opts) {
            continue;
        }
        let scoped =
            match resolve_scoped_collection(conn, opts, opts.default_dest.as_deref(), &collection)?
            {
                ScopedCollectionResolution::Resolved(scoped) => *scoped,
                ScopedCollectionResolution::NoActiveVersion
                | ScopedCollectionResolution::ExcludedBySet => continue,
            };
        let Some(dest_root) = scoped.dest_root else {
            continue;
        };

        if count_match_rows_capped(conn, scoped.version.id, MAX_MATCH_ROWS)? > MAX_MATCH_ROWS {
            continue;
        }

        let settings =
            resolve_collection_settings(conn, opts, scoped.cfg.as_ref(), &scoped.hierarchy)?;
        let matches = load_collection_matches(CollectionMatchInputs {
            conn,
            version_id: scoped.version.id,
            collection_name: &scoped.name,
            merge_mode: settings.merge_mode,
            cfg: scoped.cfg.as_ref(),
        })?;
        record_collection_desired_state(
            &mut state,
            &dest_root,
            matches.matches,
            settings.format,
            interesting_sha1s,
        )?;
    }

    Ok(state)
}

#[cfg(test)]
#[path = "desired_state_tests.rs"]
mod tests;
