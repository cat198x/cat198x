use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::collection_matches::{CollectionMatchInputs, load_collection_matches};
use super::collection_scope::{ScopedCollectionResolution, resolve_scoped_collection};
use super::destinations::{build_archive_dest_path, build_dest_path, build_disk_dest_path};
use super::matching::{MatchedRom, count_match_rows_capped};
use super::options::PlanOptions;
use super::rules::{
    MAX_MATCH_ROWS, archive_extension, archive_format_tag, effective_format, effective_merge_mode,
};
use super::scope::collection_name_matches;
use crate::config::OutputFormat;
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

        let merge_mode = effective_merge_mode(conn, opts, scoped.cfg.as_ref(), &scoped.hierarchy)?;
        let matches = load_collection_matches(CollectionMatchInputs {
            conn,
            version_id: scoped.version.id,
            collection_name: &scoped.name,
            merge_mode,
            cfg: scoped.cfg.as_ref(),
        })?;
        record_collection_desired_state(
            &mut state,
            &dest_root,
            matches.matches,
            effective_format(conn, opts, scoped.cfg.as_ref(), &scoped.hierarchy)?,
            interesting_sha1s,
        )?;
    }

    Ok(state)
}

fn record_collection_desired_state(
    state: &mut DesiredState,
    dest_root: &str,
    matches: Vec<MatchedRom>,
    format: OutputFormat,
    interesting_sha1s: &HashSet<String>,
) -> Result<()> {
    let (disk_matches, rom_matches): (Vec<MatchedRom>, Vec<MatchedRom>) =
        matches.into_iter().partition(|m| m.is_disk);

    record_disk_destinations(state, dest_root, &disk_matches)?;

    match archive_format_tag(format) {
        Some(tag) => record_archive_destinations(
            state,
            dest_root,
            &rom_matches,
            archive_extension(tag),
            interesting_sha1s,
        ),
        None => record_loose_destinations(state, dest_root, &rom_matches),
    }
}

fn record_disk_destinations(
    state: &mut DesiredState,
    dest_root: &str,
    disk_matches: &[MatchedRom],
) -> Result<()> {
    for m in disk_matches {
        state
            .dest_paths
            .insert(build_disk_dest_path(dest_root, &m.game_name, &m.rom_name)?);
    }
    Ok(())
}

fn record_archive_destinations(
    state: &mut DesiredState,
    dest_root: &str,
    rom_matches: &[MatchedRom],
    ext: &str,
    interesting_sha1s: &HashSet<String>,
) -> Result<()> {
    let mut by_game: BTreeMap<&str, Vec<&MatchedRom>> = BTreeMap::new();
    for m in rom_matches {
        by_game.entry(m.game_name.as_str()).or_default().push(m);
    }

    for (game_name, gmatches) in by_game {
        let dest = build_archive_dest_path(dest_root, game_name, ext)?;
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

    Ok(())
}

fn record_loose_destinations(
    state: &mut DesiredState,
    dest_root: &str,
    rom_matches: &[MatchedRom],
) -> Result<()> {
    let mut roms_per_game: HashMap<&str, HashSet<&str>> = HashMap::new();
    for m in rom_matches {
        roms_per_game
            .entry(m.game_name.as_str())
            .or_default()
            .insert(m.rom_name.as_str());
    }

    for m in rom_matches {
        let multi = roms_per_game
            .get(m.game_name.as_str())
            .map(|s| s.len())
            .unwrap_or(1)
            > 1;
        state.dest_paths.insert(build_dest_path(
            dest_root,
            &m.game_name,
            &m.rom_name,
            multi,
        )?);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MergeMode;
    use crate::config::OutputFormat;
    use crate::db::Database;
    use crate::db::dats;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn setup_parent_clone_fixture(conn: &Connection) {
        let coll = collections::create_collection(conn, "Arcade", "mame").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/mame.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Arcade", "dat", "ARCADE").unwrap();

        let parent =
            dats::create_game(conn, node, "puckman", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            parent,
            "shared.rom",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        let clone = dats::create_game(
            conn,
            node,
            "pacmanm",
            None,
            Some("puckman"),
            false,
            false,
            false,
        )
        .unwrap();
        dats::create_rom(
            conn,
            clone,
            "shared.rom",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            Some("shared.rom"),
        )
        .unwrap();
        dats::create_rom(
            conn,
            clone,
            "clone.rom",
            10,
            Some("BBB"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO files (sha1, size) VALUES ('AAA', 10), ('BBB', 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (400, '/lib/ToSort/ARCADE', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('AAA', 400, 'shared.rom', NULL),
                ('BBB', 400, 'clone.rom', NULL)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn desired_state_assigns_a_split_inherited_rom_to_the_parent_archive() {
        let db = setup_db();
        let conn = db.conn();
        setup_parent_clone_fixture(conn);

        let interesting: HashSet<String> = ["AAA".to_string(), "BBB".to_string()].into();
        let state = compute_desired_state(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Zip,
                default_merge_mode: MergeMode::Split,
                ..Default::default()
            },
            &interesting,
        )
        .unwrap();

        assert!(state.dest_paths.contains("/lib/ROMs/ARCADE/puckman.zip"));
        assert!(state.dest_paths.contains("/lib/ROMs/ARCADE/pacmanm.zip"));
        assert_eq!(
            state.archive_homes.get("AAA"),
            Some(&["/lib/ROMs/ARCADE/puckman.zip".to_string()].into()),
            "inherited content's canonical archive is the parent's"
        );
        assert_eq!(
            state.archive_homes.get("BBB"),
            Some(&["/lib/ROMs/ARCADE/pacmanm.zip".to_string()].into()),
            "the clone's own ROM belongs in the clone's archive"
        );
    }

    #[test]
    fn desired_state_archive_homes_only_for_interesting_content() {
        let db = setup_db();
        let conn = db.conn();
        setup_parent_clone_fixture(conn);

        let interesting: HashSet<String> = ["BBB".to_string()].into();
        let state = compute_desired_state(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Zip,
                default_merge_mode: MergeMode::Split,
                ..Default::default()
            },
            &interesting,
        )
        .unwrap();

        assert!(state.archive_homes.contains_key("BBB"));
        assert!(
            !state.archive_homes.contains_key("AAA"),
            "uninteresting content is not indexed"
        );
        assert!(state.dest_paths.contains("/lib/ROMs/ARCADE/puckman.zip"));
    }

    #[test]
    fn desired_state_records_loose_paths_and_no_archive_homes() {
        let db = setup_db();
        let conn = db.conn();
        let coll = collections::create_collection(conn, "Tapes", "tosec").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/tapes.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Tapes", "dat", "Tapes").unwrap();
        let solo = dats::create_game(conn, node, "Solo", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            solo,
            "solo.tap",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        let duo = dats::create_game(conn, node, "Duo", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            duo,
            "duo-a.tap",
            10,
            Some("BBB"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        dats::create_rom(
            conn,
            duo,
            "duo-b.tap",
            10,
            Some("CCC"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (sha1, size) VALUES ('AAA',10),('BBB',10),('CCC',10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (500, '/lib/ToSort/Tapes', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('AAA',500,'solo.tap',NULL),('BBB',500,'duo-a.tap',NULL),('CCC',500,'duo-b.tap',NULL)",
            [],
        )
        .unwrap();

        let interesting: HashSet<String> =
            ["AAA".to_string(), "BBB".to_string(), "CCC".to_string()].into();
        let state = compute_desired_state(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
            &interesting,
        )
        .unwrap();

        assert!(state.dest_paths.contains("/lib/ROMs/Tapes/solo.tap"));
        assert!(state.dest_paths.contains("/lib/ROMs/Tapes/Duo/duo-a.tap"));
        assert!(state.dest_paths.contains("/lib/ROMs/Tapes/Duo/duo-b.tap"));
        assert!(
            state.archive_homes.is_empty(),
            "a loose collection contributes no archive homes"
        );
    }
}
