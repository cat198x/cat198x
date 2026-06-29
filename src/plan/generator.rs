//! Plan generation logic

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;

use super::archive_planning::{ContainerDrain, emit_container_drains, plan_archive_matches};
use super::collisions::check_unique_destinations;
#[cfg(test)]
use super::collisions::find_destination_collisions;
pub use super::coverage::count_missing_roms;
#[cfg(test)]
use super::desired_state::compute_desired_state;
#[cfg(test)]
use super::destinations::build_dest_path;
use super::destinations::resolve_dest_root;
#[cfg(test)]
use super::matching::count_expansion_capped;
use super::matching::{
    MatchedRom, compute_shared_containers, compute_shared_content, count_match_rows_capped,
    find_matched_roms,
};
use super::placement_planning::{plan_disk_matches, plan_loose_matches};
use super::reporting;
use super::rules::{
    MAX_MATCH_ROWS, apply_one_g_one_r_filter, archive_extension, archive_format_tag,
    effective_format, effective_merge_mode, glob_match,
};
#[cfg(test)]
use super::rules::{resolve_merge_mode, resolve_output_format};
use super::source_policy::load_source_dispositions;
pub use super::state_hash::compute_state_hash;
use super::{CollectionPlanStat, Plan};
use crate::config::{MergeMode, OutputFormat};
#[cfg(test)]
use crate::db::files::{self, Disposition};
use crate::db::{collections, config as db_config, dats};

/// Options controlling plan generation.
#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    /// Glob over collection names; `None` plans every collection.
    pub dat_filter: Option<String>,
    /// Restrict planning to these sets — the top segment of a collection's
    /// library path (e.g. `TOSEC`, `TOSEC-PIX`, `FinalBurn Neo`). `None` plans
    /// every set; useful to scope one set's work (e.g. ingest TOSEC without the
    /// arcade sets) without listing every collection.
    pub set_filter: Option<Vec<String>>,
    /// Library-wide destination root for collections without their own dest_path.
    pub default_dest: Option<String>,
    /// Output format for collections without their own setting.
    pub default_format: OutputFormat,
    /// Merge mode for collections without their own setting. Controls MAME-style
    /// parent/clone placement: `Split` (the implemented target) drops a clone's
    /// merge-tagged inherited ROMs from its placement — they live in the parent —
    /// so the clone's archive/folder holds only its own unique ROMs. `NonMerged`
    /// (the default) places every ROM a game's DAT entry lists, parent or clone.
    pub default_merge_mode: MergeMode,
}

/// Generate a plan for all configured collections with default options.
pub fn generate_plan(conn: &Connection) -> Result<Plan> {
    generate_plan_filtered(conn, &PlanOptions::default())
}

/// Generate a plan from the given options.
///
/// `dat_filter` supports glob patterns (`*`, `?`, case-insensitive) over
/// collection names.
pub fn generate_plan_filtered(conn: &Connection, opts: &PlanOptions) -> Result<Plan> {
    let dat_filter = opts.dat_filter.as_deref();
    let default_dest = opts.default_dest.as_deref();

    // Calculate state hash
    let state_hash = compute_state_hash(conn)?;
    let mut plan = Plan::new(state_hash);

    // Content shared across distinct entries is copied to each destination, never
    // moved or deleted (see compute_shared_content). Computed once up front.
    let shared = compute_shared_content(conn)?;
    if !shared.is_empty() {
        reporting::shared_content(shared.len());
    }

    // Containers (archive files) whose entries serve more than one game must not
    // be relocated whole or deleted — each game repacks its own entries instead.
    let shared_containers = compute_shared_containers(conn)?;
    if !shared_containers.is_empty() {
        reporting::shared_containers(shared_containers.len());
    }

    // Each source's disposition decides, per operation, whether content is moved
    // (and its source freed) or copied. Built once; consulted at every placement.
    let dispositions = load_source_dispositions(conn)?;

    // Plan every collection, not only those with an explicit dest_path: a
    // library-wide `default_dest_path` should reach collections that were never
    // individually configured. Each collection's destination is resolved below.
    let all_collections = collections::list_collections(conn)?;

    // A destination must uniquely identify its source. Refuse before doing any
    // work if two collections in scope resolve to the same root — they would
    // silently overwrite each other's same-named games.
    check_unique_destinations(conn, opts, &all_collections)?;

    let mut planned_any = false;
    let mut filter_matched_any = false;
    let mut skipped_no_dest: Vec<String> = Vec::new();

    // Source containers a repack rebuilt from and that are safe to lose afterwards
    // — recorded here and emitted as deletes *after* every repack, so the apply
    // runs the rebuilds first and the verify-before-delete net sees each entry
    // surviving at its destination before removing the container. Draining these
    // is what lets `consume` staging empty for recompressed archive sets (a shared
    // .cue/.sub forces a rebuild over a whole-file relocate). Safety rests on the
    // net, not a plan-time guess: a container still needed elsewhere is refused,
    // sticky.
    //
    // Keyed by container path so a container feeding several games is drained
    // once; the accumulated `entries` gather, across those games, where each of
    // the container's entries was repacked to — the rollback spec that rebuilds
    // the container before those destinations are deleted. `reason_dest` is just a
    // representative destination for the human-readable reason.
    let mut drain_after_repack: BTreeMap<String, ContainerDrain> = BTreeMap::new();

    for collection in &all_collections {
        if let Some(pattern) = dat_filter
            && !glob_match(pattern, &collection.name)
        {
            continue;
        }
        filter_matched_any = true;

        // Only collections with an active version can be planned.
        let version = match collections::get_active_version(conn, collection.id)? {
            Some(v) => v,
            None => continue,
        };

        let cfg = db_config::get_collection_config(conn, &collection.name)?;

        // The collection's library path (set by recursive `dat add`), used when
        // falling back to the library-wide default destination.
        let hierarchy =
            dats::primary_node_path(conn, version.id)?.unwrap_or_else(|| collection.name.clone());

        // Restrict to requested sets (the top segment of the library path), so a
        // phase can target e.g. just TOSEC without the arcade sets. Checked
        // before the match query so excluded collections cost nothing.
        if let Some(sets) = opts.set_filter.as_ref() {
            let set = hierarchy.split('/').next().unwrap_or(hierarchy.as_str());
            if !sets.iter().any(|s| s == set) {
                continue;
            }
        }

        let explicit = cfg.as_ref().and_then(|c| c.dest_path.as_deref());

        let dest_root = match resolve_dest_root(explicit, default_dest, &hierarchy)? {
            Some(root) => root,
            None => {
                // No destination resolved — recorded and reported, never silent.
                skipped_no_dest.push(collection.name.clone());
                continue;
            }
        };

        // Guard against pathological collections before materialising any
        // matches: a MAME-style meta-aggregate expands to tens of millions of
        // match-rows and would exhaust memory. Skip-and-report instead of OOM.
        let match_rows = count_match_rows_capped(conn, version.id, MAX_MATCH_ROWS)?;
        if match_rows > MAX_MATCH_ROWS {
            reporting::oversized_collection(&collection.name);
            plan.skipped_oversized.push(format!(
                "{} (>{} match-rows)",
                collection.name, MAX_MATCH_ROWS
            ));
            continue;
        }

        planned_any = true;
        reporting::planning_collection(&collection.name, &version.version);

        // Effective merge mode (explicit per-collection → per-set rule →
        // library-wide default). Split mode drops a clone's inherited
        // (merge-tagged) ROMs from its placement so they live only in the parent;
        // non-merged places every ROM the DAT lists per game. Merged is not yet
        // wired in the planner. Shared with `compute_desired_state`.
        let merge_mode = effective_merge_mode(conn, opts, cfg.as_ref(), &hierarchy)?;
        if merge_mode == MergeMode::Merged {
            reporting::merged_mode_not_implemented(&collection.name);
        }

        // Find all matched ROMs for this version. In split mode, a clone's
        // merge-tagged inherited ROMs are excluded here (they belong to the
        // parent), so the clone is placed with only its own unique ROMs.
        let matches = find_matched_roms(
            conn,
            version.id,
            &collection.name,
            merge_mode == MergeMode::Split,
        )?;

        // Apply 1G1R filtering if enabled for this collection.
        let matches = match cfg.as_ref().and_then(|c| c.extra_config.as_ref()) {
            Some(extra) if extra.one_g_one_r => {
                let prefs = extra.to_filter_preferences();
                let original_count = matches.len();
                let filtered = apply_one_g_one_r_filter(&matches, &prefs);
                if filtered.len() < original_count {
                    reporting::one_g_one_r(original_count, filtered.len());
                }
                filtered
            }
            _ => matches,
        };

        // Effective output format (explicit per-collection → per-set rule →
        // library-wide default). The per-set tier lets whole sets diverge — TOSEC
        // kept as zip, TOSEC-PIX left loose for later PDF/collateral extraction —
        // without configuring every collection. Loose copies each ROM into place;
        // zip/torrentzip packs each game into one archive. Shared with
        // `compute_desired_state`.
        let format = effective_format(conn, opts, cfg.as_ref(), &hierarchy)?;

        let mut already_correct = 0;
        let mut to_write = 0;
        let mut relocated = 0;
        let mut deduped = 0;
        let mut bytes = 0u64;

        // CHDs (<disk> entries) are always stored loose in a machine folder
        // (<dest>/<game>/<name>.chd) and never packed, even when the set's
        // format is an archive — so plan them on their own path and run the
        // format branch over the remaining <rom> entries only.
        let (disk_matches, matches): (Vec<MatchedRom>, Vec<MatchedRom>) =
            matches.into_iter().partition(|m| m.is_disk);

        match archive_format_tag(format) {
            None => {
                let c = plan_loose_matches(
                    matches,
                    &dest_root,
                    default_dest,
                    &shared,
                    &dispositions,
                    &mut plan,
                )?;
                already_correct += c.already_correct;
                to_write += c.to_write;
                bytes += c.bytes;
                deduped += c.deduped;
                reporting::loose_summary(already_correct, to_write, deduped);
            }
            Some(tag) => {
                let ext = archive_extension(tag);
                let c = plan_archive_matches(
                    matches,
                    tag,
                    ext,
                    &dest_root,
                    default_dest,
                    &shared,
                    &shared_containers,
                    &dispositions,
                    &mut plan,
                    &mut drain_after_repack,
                )?;
                already_correct += c.already_correct;
                relocated += c.relocated;
                to_write += c.to_write;
                bytes += c.bytes;
                deduped += c.deduped;
                reporting::archive_summary(already_correct, relocated, to_write, deduped);
            }
        }

        // Plan any CHDs loose, regardless of the set's format. (Disk dedups are
        // reported within the helper, like the other branches' own counts.)
        if !disk_matches.is_empty() {
            let d = plan_disk_matches(
                disk_matches,
                &dest_root,
                opts,
                &shared,
                &dispositions,
                &mut plan,
            )?;
            already_correct += d.already_correct;
            to_write += d.to_write;
            bytes += d.bytes;
        }

        plan.summary.already_correct += already_correct;
        plan.per_collection.push(CollectionPlanStat {
            name: collection.name.clone(),
            node_path: hierarchy,
            to_write,
            already_correct,
            bytes,
        });
    }

    emit_container_drains(&mut plan, drain_after_repack);

    // Never skip silently: report collections left out because no destination
    // could be resolved, and how to include them. The full list rides on the
    // plan so the caller can write it out for review.
    if !skipped_no_dest.is_empty() {
        reporting::skipped_no_dest(skipped_no_dest.len());
    }

    // Report collections left out because their match expansion is too large to
    // plan safely (a meta-aggregate, not a romset). Already named individually
    // above as they were hit; this is the rollup.
    if !plan.skipped_oversized.is_empty() {
        reporting::skipped_oversized_rollup(plan.skipped_oversized.len());
    }

    if let Some(pattern) = dat_filter
        && !filter_matched_any
    {
        reporting::no_matching_filter(pattern);
    } else if !planned_any && skipped_no_dest.is_empty() && plan.skipped_oversized.is_empty() {
        reporting::no_active_collections();
    }

    plan.skipped_no_dest = skipped_no_dest;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::plan::OperationKind;
    use std::collections::HashSet;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn count_expansion_capped_caps_and_counts_with_rom_multiplicity() {
        let db = setup_db();
        let conn = db.conn();
        // Two distinct ROMs share content AAA, which is held in three locations.
        // The materialised expansion is one row per (matched ROM × location) =
        // 2 ROMs × 3 locations = 6.
        let coll = collections::create_collection(conn, "Agg", "mame").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/agg.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Agg", "dat", "MAME").unwrap();
        let g = dats::create_game(conn, node, "bucket", None, None, false, false, false).unwrap();
        dats::create_rom(conn, g, "a.rom", 10, Some("AAA"), None, None, "good", None).unwrap();
        dats::create_rom(conn, g, "b.rom", 10, Some("AAA"), None, None, "good", None).unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (500, '/src', 0)",
            [],
        )
        .unwrap();
        for i in 0..3 {
            conn.execute(
                &format!(
                    "INSERT INTO file_locations (sha1, source_id, path, archive_path)
                     VALUES ('AAA', 500, 'loc{i}.zip', 'x.rom')"
                ),
                [],
            )
            .unwrap();
        }

        // A generous cap returns the true expansion (6).
        assert_eq!(count_expansion_capped(conn, vid, 100).unwrap(), 6);
        // A cap below the expansion is detected without counting past cap + 1.
        let capped = count_expansion_capped(conn, vid, 4).unwrap();
        assert_eq!(capped, 5, "the inner LIMIT halts at cap + 1");
        assert!(capped > 4, "over-cap is reported as exceeding the cap");
    }

    #[test]
    fn test_build_dest_path_single_rom_is_flat() {
        // A single-ROM game is placed flat, with no redundant game folder.
        assert_eq!(
            build_dest_path("/roms/nes", "Super Mario Bros", "mario.nes", false).unwrap(),
            "/roms/nes/mario.nes"
        );
        // A trailing slash on the root is normalised away.
        assert_eq!(
            build_dest_path("/roms/nes/", "Game", "game.rom", false).unwrap(),
            "/roms/nes/game.rom"
        );
    }

    #[test]
    fn test_build_dest_path_multi_rom_gets_game_folder() {
        assert_eq!(
            build_dest_path("/roms/nes", "Multi Disk Game", "disk1.img", true).unwrap(),
            "/roms/nes/Multi Disk Game/disk1.img"
        );
        assert_eq!(
            build_dest_path("/roms/nes", "Multi Disk Game", "disk2.img", true).unwrap(),
            "/roms/nes/Multi Disk Game/disk2.img"
        );
    }

    #[test]
    fn destination_building_rejects_unsafe_dat_names() {
        for unsafe_name in [
            "../escape.rom",
            "dir/../../escape.rom",
            "/tmp/escape.rom",
            r"dir\escape.rom",
        ] {
            assert!(
                build_dest_path("/roms/nes", "Game", unsafe_name, false).is_err(),
                "unsafe ROM name should be rejected: {unsafe_name}"
            );
            assert!(
                build_dest_path("/roms/nes", unsafe_name, "disk1.img", true).is_err(),
                "unsafe game name should be rejected: {unsafe_name}"
            );
        }
        assert!(
            resolve_dest_root(None, Some("/roms"), "../Collection").is_err(),
            "unsafe hierarchy should be rejected"
        );
    }

    #[test]
    fn test_generate_plan_no_config() {
        let db = setup_db();
        let conn = db.conn();

        let plan = generate_plan(conn).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn plan_records_collections_skipped_for_no_destination() {
        let db = setup_db();
        let conn = db.conn();

        // A collection with an active version but no dest_path and no default.
        let cid = collections::create_collection(conn, "No Dest Coll", "tosec").unwrap();
        let vid = collections::add_version(conn, cid, "1.0", "/tmp/x.dat", true).unwrap();
        dats::create_node(conn, vid, None, "No Dest Coll", "dat", "No Dest Coll").unwrap();

        let plan = generate_plan_filtered(conn, &PlanOptions::default()).unwrap();
        assert!(plan.is_empty(), "no destination → no operations");
        assert_eq!(plan.skipped_no_dest, vec!["No Dest Coll".to_string()]);
    }

    #[test]
    fn refuses_when_two_collections_share_a_destination_root() {
        let db = setup_db();
        let conn = db.conn();
        // Both collections have the flat hierarchy "FBN", so both resolve to
        // <default>/FBN — the flat-namespace trap that overwrites same-named games.
        for name in ["Arcade Games", "Game Gear Games"] {
            let c = collections::create_collection(conn, name, "mame").unwrap();
            let vid =
                collections::add_version(conn, c, "v1", &format!("/d/{name}.dat"), true).unwrap();
            dats::create_node(conn, vid, None, name, "dat", "FBN").unwrap();
        }
        let err = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("share a destination root"), "got: {msg}");
        assert!(
            msg.contains("/lib/ROMs/FBN"),
            "names the shared root: {msg}"
        );
        assert!(
            msg.contains("Arcade Games") && msg.contains("Game Gear Games"),
            "names the colliding collections: {msg}"
        );
    }

    #[test]
    fn allows_collections_with_distinct_destination_roots() {
        let db = setup_db();
        let conn = db.conn();
        // Per-machine hierarchies → distinct roots → no collision, plan proceeds.
        for (name, path) in [
            ("Arcade Games", "FBN/Arcade"),
            ("Game Gear Games", "FBN/Game Gear"),
        ] {
            let c = collections::create_collection(conn, name, "mame").unwrap();
            let vid =
                collections::add_version(conn, c, "v1", &format!("/d/{name}.dat"), true).unwrap();
            dats::create_node(conn, vid, None, name, "dat", path).unwrap();
        }
        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            plan.is_empty(),
            "no held content, so an empty but valid plan"
        );
    }

    #[test]
    fn allows_a_chd_collection_to_share_a_root_with_a_rom_collection() {
        let db = setup_db();
        let conn = db.conn();
        // A ROM collection and a disk-only CHD collection, both at root "Demul".
        // A game's `<game>.zip` and its `<game>/<name>.chd` don't collide.
        let rc = collections::create_collection(conn, "Demul ROMs", "mame").unwrap();
        let rv = collections::add_version(conn, rc, "v1", "/d/r.dat", true).unwrap();
        let rn = dats::create_node(conn, rv, None, "Demul ROMs", "dat", "Demul").unwrap();
        let rg = dats::create_game(conn, rn, "azumanga", None, None, false, false, false).unwrap();
        dats::create_rom(conn, rg, "a.rom", 10, Some("AAA"), None, None, "good", None).unwrap();

        let cc = collections::create_collection(conn, "Demul CHDs", "mame").unwrap();
        let cv = collections::add_version(conn, cc, "v1", "/d/c.dat", true).unwrap();
        let cn = dats::create_node(conn, cv, None, "Demul CHDs", "dat", "Demul").unwrap();
        let cg = dats::create_game(conn, cn, "azumanga", None, None, false, false, false).unwrap();
        dats::create_disk(conn, cg, "gdl-0018", Some("DDD"), None, "good", None).unwrap();

        // The guard must NOT refuse — ROM and CHD are different output namespaces.
        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(plan.is_empty(), "no held content, but an un-refused plan");
    }

    #[test]
    fn find_destination_collisions_groups_colliders_and_flags_explicit_dest() {
        let db = setup_db();
        let conn = db.conn();
        // Two collections share the flat root "FBN"; neither has an explicit dest.
        for name in ["Arcade Games", "Game Gear Games"] {
            let c = collections::create_collection(conn, name, "mame").unwrap();
            let vid =
                collections::add_version(conn, c, "v1", &format!("/d/{name}.dat"), true).unwrap();
            dats::create_node(conn, vid, None, name, "dat", "FBN").unwrap();
        }
        let all = collections::list_collections(conn).unwrap();
        let opts = PlanOptions {
            default_dest: Some("/lib".to_string()),
            ..Default::default()
        };
        let collisions = find_destination_collisions(conn, &opts, &all).unwrap();
        assert_eq!(collisions.len(), 1, "one shared root");
        let c = &collisions[0];
        assert_eq!(c.root, "/lib/FBN");
        assert!(!c.disk_only, "ROM-output namespace");
        assert_eq!(c.collections.len(), 2);
        assert!(
            c.collections.iter().all(|m| !m.has_explicit_dest),
            "neither has an explicit dest"
        );
    }

    #[test]
    fn nesting_colliders_under_their_name_clears_the_collision() {
        let db = setup_db();
        let conn = db.conn();
        for name in ["Arcade Games", "Game Gear Games"] {
            let c = collections::create_collection(conn, name, "mame").unwrap();
            let vid =
                collections::add_version(conn, c, "v1", &format!("/d/{name}.dat"), true).unwrap();
            dats::create_node(conn, vid, None, name, "dat", "FBN").unwrap();
        }
        let all = collections::list_collections(conn).unwrap();
        let opts = PlanOptions {
            default_dest: Some("/lib".to_string()),
            ..Default::default()
        };
        let before = find_destination_collisions(conn, &opts, &all).unwrap();
        assert_eq!(before.len(), 1);
        // The doctor --fix action: nest each non-explicit collider under its name.
        for member in &before[0].collections {
            let new_path = dats::nest_primary_node_under_name(conn, member.version_id)
                .unwrap()
                .expect("a primary node");
            assert!(new_path.starts_with("FBN/"), "nested under FBN: {new_path}");
        }
        let after = find_destination_collisions(conn, &opts, &all).unwrap();
        assert!(after.is_empty(), "each now resolves to a distinct root");
    }

    #[test]
    fn resolve_dest_root_prefers_explicit_path() {
        // An explicit per-collection dest_path wins and is used verbatim,
        // ignoring both the default and the hierarchy.
        assert_eq!(
            resolve_dest_root(Some("/explicit/here"), Some("/lib"), "Acorn/BBC").unwrap(),
            Some("/explicit/here".to_string())
        );
    }

    #[test]
    fn dedup_never_deletes_a_placed_library_copy() {
        let db = setup_db();
        let conn = db.conn();
        // One single-ROM game whose content is held at three places: the canonical
        // destination (already correct), a *second* library path — a sibling
        // placement, as a merged-set clone would have (one DAT game, so not
        // flagged as shared content) — and a stray copy under ToSort.
        let coll = collections::create_collection(conn, "Merge Coll", "mame").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/m.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Merge Coll", "dat", "SET/Sys").unwrap();
        let g = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            g,
            "shared.bin",
            10,
            Some("SSS"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('SSS', 10)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition) VALUES
                (1, '/lib/ROMs/SET/Sys', 0, 'preserve'),
                (2, '/lib/ROMs/SET/Sys/clone', 0, 'preserve'),
                (3, '/lib/ToSort/SET', 0, 'consume')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('SSS', 1, 'shared.bin', NULL),
                ('SSS', 2, 'shared.bin', NULL),
                ('SSS', 3, 'shared.bin', NULL)",
            [],
        )
        .unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();

        // Only the ToSort stray is deleted; both library copies are left in place.
        let deleted: Vec<_> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Delete { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            deleted,
            vec!["/lib/ToSort/SET/shared.bin".to_string()],
            "a placed library copy must never be deleted as a duplicate"
        );
    }

    #[test]
    fn resolve_dest_root_falls_back_to_default_plus_hierarchy() {
        assert_eq!(
            resolve_dest_root(None, Some("/Volumes/Data"), "TOSEC-PIX/Acorn/BBC").unwrap(),
            Some("/Volumes/Data/TOSEC-PIX/Acorn/BBC".to_string())
        );
        // A trailing slash on the default base is normalised away.
        assert_eq!(
            resolve_dest_root(None, Some("/Volumes/Data/"), "TOSEC/Sinclair").unwrap(),
            Some("/Volumes/Data/TOSEC/Sinclair".to_string())
        );
    }

    #[test]
    fn resolve_dest_root_is_none_without_explicit_or_default() {
        // Neither an explicit path nor a default: no destination, caller skips.
        assert_eq!(resolve_dest_root(None, None, "Acorn/BBC").unwrap(), None);
    }

    #[test]
    fn resolve_output_format_prefers_explicit() {
        assert_eq!(
            resolve_output_format(Some("zip"), OutputFormat::Loose),
            OutputFormat::Zip
        );
        assert_eq!(
            resolve_output_format(Some("TorrentZip"), OutputFormat::Loose),
            OutputFormat::TorrentZip
        );
        assert_eq!(
            resolve_output_format(Some("loose"), OutputFormat::Zip),
            OutputFormat::Loose
        );
    }

    #[test]
    fn resolve_output_format_falls_back_to_default() {
        assert_eq!(
            resolve_output_format(None, OutputFormat::TorrentZip),
            OutputFormat::TorrentZip
        );
        // Unrecognised value falls back rather than failing the plan.
        assert_eq!(
            resolve_output_format(Some("rar"), OutputFormat::Zip),
            OutputFormat::Zip
        );
    }

    #[test]
    fn archive_format_tag_maps_formats() {
        assert_eq!(archive_format_tag(OutputFormat::Loose), None);
        assert_eq!(archive_format_tag(OutputFormat::Zip), Some("zip"));
        assert_eq!(
            archive_format_tag(OutputFormat::TorrentZip),
            Some("torrentzip")
        );
        assert_eq!(archive_format_tag(OutputFormat::SevenZip), Some("7z"));
    }

    #[test]
    fn resolve_output_format_and_extension_handle_7z() {
        assert_eq!(
            resolve_output_format(Some("7z"), OutputFormat::Loose),
            OutputFormat::SevenZip
        );
        assert_eq!(archive_extension("7z"), "7z");
        assert_eq!(archive_extension("zip"), "zip");
        assert_eq!(archive_extension("torrentzip"), "zip");
    }

    #[test]
    fn resolve_merge_mode_prefers_explicit_then_default() {
        // The kebab-case strings match the MergeMode serde representation.
        assert_eq!(
            resolve_merge_mode(Some("split"), MergeMode::NonMerged),
            MergeMode::Split
        );
        assert_eq!(
            resolve_merge_mode(Some("merged"), MergeMode::NonMerged),
            MergeMode::Merged
        );
        assert_eq!(
            resolve_merge_mode(Some("non-merged"), MergeMode::Split),
            MergeMode::NonMerged
        );
        // Case-insensitive.
        assert_eq!(
            resolve_merge_mode(Some("Split"), MergeMode::NonMerged),
            MergeMode::Split
        );
        // Absent or unrecognised falls back to the default rather than failing.
        assert_eq!(resolve_merge_mode(None, MergeMode::Split), MergeMode::Split);
        assert_eq!(
            resolve_merge_mode(Some("clone"), MergeMode::NonMerged),
            MergeMode::NonMerged
        );
    }

    /// Build a one-ROM collection whose held file exists in two places: already
    /// at its canonical destination under the library, and a staged duplicate
    /// elsewhere. `archived` controls whether the file is a loose file or an
    /// inner entry of a `.zip` (and sets the per-set format accordingly).
    fn setup_dup_fixture(conn: &Connection, archived: bool) {
        let coll = collections::create_collection(conn, "Test Coll", "tosec").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/test.dat", true).unwrap();
        // Node path "SET/Sys" → set is "SET"; library default + path is the root.
        let node = dats::create_node(conn, vid, None, "Test Coll", "dat", "SET/Sys").unwrap();
        let game = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            game,
            "game.rom",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
            .unwrap();

        // Library copy (already at the canonical destination) and a ToSort dup.
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition) VALUES
                (101, '/lib/ROMs/SET/Sys', 0, 'preserve'),
                (102, '/lib/ToSort/SET', 0, 'consume')",
            [],
        )
        .unwrap();
        if archived {
            // Each copy is a .zip holding the ROM as an inner entry.
            conn.execute(
                "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                    ('AAA', 101, 'Game.zip', 'game.rom'),
                    ('AAA', 102, 'Sys/Game.zip', 'game.rom')",
                [],
            )
            .unwrap();
            db_config::set_output_format(conn, "SET", "zip").unwrap();
        } else {
            conn.execute(
                "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                    ('AAA', 101, 'game.rom', NULL),
                    ('AAA', 102, 'Sys/game.rom', NULL)",
                [],
            )
            .unwrap();
        }
    }

    #[test]
    fn loose_duplicate_is_deleted_canonical_kept_in_place() {
        let db = setup_db();
        let conn = db.conn();
        setup_dup_fixture(conn, false);

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();

        // The library copy at /lib/ROMs/SET/Sys/game.rom is already correct, so
        // no move; the ToSort copy is an exact-content duplicate and is deleted
        // (its bytes are preserved by the canonical copy).
        assert_eq!(
            plan.summary.move_count, 0,
            "canonical copy already in place"
        );
        assert_eq!(plan.summary.copy_count, 0);
        assert_eq!(
            plan.summary.quarantine_count, 0,
            "dups are deleted, not quarantined"
        );
        assert_eq!(plan.summary.delete_count, 1, "ToSort dup deleted");
        let deleted: Vec<_> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Delete { path, reason, .. } => Some((path.clone(), reason.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].0, "/lib/ToSort/SET/Sys/game.rom");
        // The delete records why it is safe: the canonical copy it keeps. Every
        // planner delete is a dedup, so the reason names a surviving path.
        assert!(
            deleted[0].1.starts_with("exact duplicate — kept ")
                && deleted[0].1.contains("game.rom"),
            "reason names the kept copy: {:?}",
            deleted[0].1
        );
    }

    #[test]
    fn loose_duplicate_left_untouched_for_a_preserve_source() {
        let db = setup_db();
        let conn = db.conn();
        setup_dup_fixture(conn, false);
        // A preserve source never loses content, so its exact-content duplicate
        // is left in place rather than deleted.
        files::set_source_disposition(conn, "/lib/ToSort/SET", Disposition::Preserve).unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.summary.delete_count, 0, "copy mode deletes nothing");
        assert_eq!(plan.summary.quarantine_count, 0);
    }

    #[test]
    fn archive_duplicate_container_is_deleted() {
        let db = setup_db();
        let conn = db.conn();
        setup_dup_fixture(conn, true);

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose, // overridden to zip per-set
                ..Default::default()
            },
        )
        .unwrap();

        // The complete archive already sits at /lib/ROMs/SET/Sys/Game.zip, so
        // nothing is built; the ToSort .zip is a duplicate container and deleted.
        assert_eq!(
            plan.summary.repack_count, 0,
            "canonical archive already at dest"
        );
        assert_eq!(plan.summary.delete_count, 1);
        let deleted: Vec<_> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Delete { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deleted, vec!["/lib/ToSort/SET/Sys/Game.zip".to_string()]);
    }

    #[test]
    fn preserve_loose_is_consolidated_into_an_archive_in_the_same_tree() {
        let db = setup_db();
        let conn = db.conn();
        let coll = collections::create_collection(conn, "Lib Coll", "tosec").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/l.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Lib Coll", "dat", "SET/Sys").unwrap();
        let game = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            game,
            "game.rom",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
            .unwrap();

        // The library destination is itself a preserve source, and the loose ROM
        // already lives inside it. Consolidating it into Game.zip keeps the content
        // in the same tree, so the loose original may be consumed.
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition)
             VALUES (101, '/lib/ROMs/SET/Sys', 0, 'preserve')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path)
             VALUES ('AAA', 101, 'game.rom', NULL)",
            [],
        )
        .unwrap();
        db_config::set_output_format(conn, "SET", "zip").unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose, // overridden to zip per-set
                ..Default::default()
            },
        )
        .unwrap();

        // One repack builds the canonical Game.zip, and because the archive lands
        // in the same preserve tree, it consumes the loose source (move_sources) —
        // the loose original is not left behind. No separate delete is emitted.
        assert_eq!(plan.summary.repack_count, 1, "the loose ROM is archived");
        assert_eq!(
            plan.summary.delete_count, 0,
            "consumed by the repack, not deleted"
        );
        let consumes_loose = plan.operations.iter().any(|op| {
            matches!(
                &op.kind,
                OperationKind::Repack { move_sources: true, dest, .. } if dest.ends_with("Game.zip")
            )
        });
        assert!(
            consumes_loose,
            "loose→archive consolidation within a preserve tree consumes the loose source"
        );
    }

    #[test]
    fn shared_content_is_copied_to_each_destination_not_moved() {
        let db = setup_db();
        let conn = db.conn();
        // One physical file's content (BBB) belongs to two distinct games — two
        // destinations. It is held once, in ToSort (at neither destination).
        let coll = collections::create_collection(conn, "Shared Coll", "tosec").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/s.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Shared Coll", "dat", "SET/Sys").unwrap();
        let g1 = dats::create_game(conn, node, "GameA", None, None, false, false, false).unwrap();
        dats::create_rom(conn, g1, "a.rom", 10, Some("BBB"), None, None, "good", None).unwrap();
        let g2 = dats::create_game(conn, node, "GameB", None, None, false, false, false).unwrap();
        dats::create_rom(conn, g2, "b.rom", 10, Some("BBB"), None, None, "good", None).unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('BBB', 10)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition)
             VALUES (200, '/lib/ToSort/SET', 0, 'consume')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path)
             VALUES ('BBB', 200, 'Sys/shared.rom', NULL)",
            [],
        )
        .unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();

        // Both distinct entries get a real copy; the shared source is never moved
        // or deleted, so neither destination can be stranded.
        assert_eq!(
            plan.summary.move_count, 0,
            "shared content is copied, not moved"
        );
        assert_eq!(
            plan.summary.delete_count, 0,
            "a shared source is never deleted"
        );
        assert_eq!(
            plan.summary.copy_count, 2,
            "a real copy for each distinct destination"
        );
    }

    #[test]
    fn disk_is_planned_loose_in_a_machine_folder_even_for_a_zip_set() {
        let db = setup_db();
        let conn = db.conn();
        // A CHD (<disk>) in a zip-format set must still be placed loose at
        // <dest>/<game>/<name>.chd — never packed into an archive.
        let coll = collections::create_collection(conn, "MAME CHDs", "mame").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/chd.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "MAME CHDs", "dat", "MAME").unwrap();
        let g = dats::create_game(conn, node, "azumanga", None, None, false, false, false).unwrap();
        // A disk: name without extension, sha1 = the CHD's internal hash.
        dats::create_disk(conn, g, "gdl-0018", Some("DDD"), None, "good", None).unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('DDD', 4096)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (300, '/lib/ToSort/MAME', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path)
             VALUES ('DDD', 300, 'MAME CHDs (merged)/azumanga/gdl-0018.chd', NULL)",
            [],
        )
        .unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                // Zip is the set format — the disk must ignore it and stay loose.
                default_format: OutputFormat::Zip,
                ..Default::default()
            },
        )
        .unwrap();

        // No archive is built for a disk.
        assert_eq!(plan.summary.repack_count, 0, "a CHD is never packed");
        // It is copied loose to <dest>/MAME/<game>/<name>.chd.
        let copies: Vec<String> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Copy { dest, .. } => Some(dest.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            copies,
            vec!["/lib/ROMs/MAME/azumanga/gdl-0018.chd".to_string()]
        );
    }

    #[test]
    fn shared_detection_matches_crc_only_arcade_content() {
        // Arcade DATs (MAME / FinalBurn Neo) are CRC-only: their ROMs have a NULL
        // sha1 and match held content by CRC32 + size. A SHA1-only shared check
        // missed them, so a container several games depend on read as unshared and
        // became eligible for a whole-archive relocate. Both detectors must see it.
        let db = setup_db();
        let conn = db.conn();
        let coll = collections::create_collection(conn, "Arcade", "mame").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/a.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Arcade", "dat", "ARCADE").unwrap();

        // Two distinct games whose ROM is the same content, declared CRC-only
        // (sha1 = None) as arcade DATs do.
        let parent =
            dats::create_game(conn, node, "2010", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            parent,
            "p.rom",
            100,
            None,
            None,
            Some("AABBCCDD"),
            "good",
            None,
        )
        .unwrap();
        let clone = dats::create_game(
            conn,
            node,
            "2010p1",
            None,
            Some("2010"),
            false,
            false,
            false,
        )
        .unwrap();
        dats::create_rom(
            conn,
            clone,
            "p.rom",
            100,
            None,
            None,
            Some("AABBCCDD"),
            "good",
            None,
        )
        .unwrap();

        // One held file (real sha1) carrying that CRC32/size, inside one archive.
        conn.execute(
            "INSERT INTO files (sha1, crc32, size) VALUES ('FILESHA', 'AABBCCDD', 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (500, '/lib/ToSort/ARCADE', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path)
             VALUES ('FILESHA', 500, '2010.zip', 'p.rom')",
            [],
        )
        .unwrap();

        let shared = compute_shared_content(conn).unwrap();
        assert!(
            shared.contains("FILESHA"),
            "CRC-only content shared across two games must be flagged shared"
        );

        let containers = compute_shared_containers(conn).unwrap();
        assert!(
            containers.contains("/lib/ToSort/ARCADE/2010.zip"),
            "a container sourcing two games by CRC32 must be flagged shared (repack, not relocate)"
        );
    }

    /// A parent/clone pair where the clone holds one inherited (merge-tagged) ROM
    /// shared with the parent plus one of its own. The same fixture drives both
    /// merge modes, asserting only the split filter changes placement.
    fn setup_parent_clone_fixture(conn: &Connection) {
        let coll = collections::create_collection(conn, "Arcade", "mame").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/mame.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Arcade", "dat", "ARCADE").unwrap();

        // Parent: owns shared.rom (AAA), no merge tag.
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

        // Clone of puckman: shared.rom is inherited (merge-tagged → lives in the
        // parent under split); clone.rom (BBB) is its own unique ROM.
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
        // Both ROMs held loose in ToSort.
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('AAA', 400, 'shared.rom', NULL),
                ('BBB', 400, 'clone.rom', NULL)",
            [],
        )
        .unwrap();
    }

    /// Map each game's planned archive to the sorted canonical entry names it
    /// will hold — read from the repack sources' `entry_name`. Zip is the arcade
    /// target, so split/non-merged are compared on archive *contents*.
    fn repack_entries(plan: &Plan) -> BTreeMap<String, Vec<String>> {
        let mut by_dest: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for op in &plan.operations {
            if let OperationKind::Repack { sources, dest, .. } = &op.kind {
                let mut entries: Vec<String> = sources
                    .iter()
                    .filter_map(|s| s.entry_name.clone())
                    .collect();
                entries.sort();
                by_dest.insert(dest.clone(), entries);
            }
        }
        by_dest
    }

    #[test]
    fn split_mode_drops_a_clones_inherited_rom_from_its_archive() {
        let db = setup_db();
        let conn = db.conn();
        setup_parent_clone_fixture(conn);

        // Zip + split — the chosen arcade layout. The clone's archive must hold
        // only its own unique ROM; the inherited (merge-tagged) shared.rom lives
        // in the parent's archive, not the clone's.
        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Zip,
                default_merge_mode: MergeMode::Split,
                ..Default::default()
            },
        )
        .unwrap();

        let entries = repack_entries(&plan);
        assert_eq!(
            entries.get("/lib/ROMs/ARCADE/pacmanm.zip"),
            Some(&vec!["clone.rom".to_string()]),
            "split: the clone archive holds only its own unique ROM"
        );
        assert_eq!(
            entries.get("/lib/ROMs/ARCADE/puckman.zip"),
            Some(&vec!["shared.rom".to_string()]),
            "split: the inherited ROM lives in the parent archive"
        );
    }

    #[test]
    fn non_merged_mode_keeps_a_clones_inherited_rom_in_its_archive() {
        let db = setup_db();
        let conn = db.conn();
        setup_parent_clone_fixture(conn);

        // Default merge mode (NonMerged): every ROM the DAT lists per game is
        // placed, so the clone's archive carries its own copy of the inherited
        // shared.rom alongside its unique ROM.
        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Zip,
                ..Default::default()
            },
        )
        .unwrap();

        let entries = repack_entries(&plan);
        assert_eq!(
            entries.get("/lib/ROMs/ARCADE/pacmanm.zip"),
            Some(&vec!["clone.rom".to_string(), "shared.rom".to_string()]),
            "non-merged: the clone archive carries its own copy of the inherited ROM"
        );
    }

    #[test]
    fn shared_archive_content_is_repacked_to_each_game_not_consumed() {
        let db = setup_db();
        let conn = db.conn();
        // Content CCC belongs to two distinct games in a zip-format set, held once
        // as a loose file in ToSort.
        let coll = collections::create_collection(conn, "Shared Zip", "tosec").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/z.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Shared Zip", "dat", "SET/Sys").unwrap();
        let g1 = dats::create_game(conn, node, "GA", None, None, false, false, false).unwrap();
        dats::create_rom(conn, g1, "r.rom", 10, Some("CCC"), None, None, "good", None).unwrap();
        let g2 = dats::create_game(conn, node, "GB", None, None, false, false, false).unwrap();
        dats::create_rom(conn, g2, "r.rom", 10, Some("CCC"), None, None, "good", None).unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('CCC', 10)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition)
             VALUES (201, '/lib/ToSort/SET', 0, 'consume')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path)
             VALUES ('CCC', 201, 'Sys/shared.rom', NULL)",
            [],
        )
        .unwrap();
        db_config::set_output_format(conn, "SET", "zip").unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();

        // Each game's archive is built by copying; the shared loose source is
        // neither consumed by a repack nor removed as a duplicate container.
        assert_eq!(
            plan.summary.repack_count, 2,
            "an archive built for each game"
        );
        assert_eq!(plan.summary.delete_count, 0, "shared source never deleted");
        let none_consume_source = plan.operations.iter().all(|op| match &op.kind {
            OperationKind::Repack { move_sources, .. } => !*move_sources,
            _ => true,
        });
        assert!(
            none_consume_source,
            "shared repacks must not consume their source"
        );
    }

    #[test]
    fn shared_container_is_repacked_per_game_not_relocated_whole() {
        let db = setup_db();
        let conn = db.conn();
        // One archive (bundle.zip) holds ROMs for two distinct games — a
        // multi-game container. Each game's ROM is a different entry/SHA1.
        let coll = collections::create_collection(conn, "Bundle Coll", "tosec").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/b.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Bundle Coll", "dat", "SET/Sys").unwrap();
        let g1 = dats::create_game(conn, node, "GameOne", None, None, false, false, false).unwrap();
        dats::create_rom(conn, g1, "a.rom", 10, Some("AAA"), None, None, "good", None).unwrap();
        let g2 = dats::create_game(conn, node, "GameTwo", None, None, false, false, false).unwrap();
        dats::create_rom(conn, g2, "b.rom", 10, Some("BBB"), None, None, "good", None).unwrap();
        conn.execute(
            "INSERT INTO files (sha1, size) VALUES ('AAA', 10), ('BBB', 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition)
             VALUES (210, '/lib/ToSort/SET', 0, 'consume')",
            [],
        )
        .unwrap();
        // Both ROMs live as entries inside the SAME archive file.
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('AAA', 210, 'bundle.zip', 'a.rom'),
                ('BBB', 210, 'bundle.zip', 'b.rom')",
            [],
        )
        .unwrap();
        db_config::set_output_format(conn, "SET", "zip").unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();

        // The shared container is repacked per game (extracting each game's own
        // entry), never relocated whole (which would strand the other game).
        let relocates = plan
            .operations
            .iter()
            .filter(|op| matches!(op.kind, OperationKind::Relocate { .. }))
            .count();
        assert_eq!(
            relocates, 0,
            "a multi-game container is never relocated whole"
        );
        assert_eq!(
            plan.summary.repack_count, 2,
            "each game repacks its own entry"
        );
        // Once *both* games are repacked, every entry the container held survives
        // in a game archive, so the consume container is drained — exactly once,
        // despite feeding two games. The verify-before-delete net is the guard at
        // apply time: it removes the container only after confirming each entry
        // survives elsewhere, so the order (drain emitted after all repacks) and
        // the net together make this safe.
        assert_eq!(
            plan.summary.delete_count, 1,
            "the fully-consolidated shared container is drained, once"
        );
        let drained: Vec<_> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Delete { path, reason, .. } => Some((path.clone(), reason.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0, "/lib/ToSort/SET/bundle.zip");
        assert!(
            drained[0].1.starts_with("consolidated into "),
            "reason names where the content went: {:?}",
            drained[0].1
        );
    }

    #[test]
    fn single_game_consume_container_drains_when_a_shared_entry_forces_a_repack() {
        let db = setup_db();
        let conn = db.conn();
        // The real CD-image case: g1.zip, in a CONSUME staging source, holds
        // GameOne in full — its own ROM plus a ROM whose content (CCC) is shared
        // with GameTwo (a common .cue/.sub). The shared entry makes GameOne
        // `game_shared`, which blocks a whole-file relocate and forces a rebuild.
        // The container is then drained — earlier this was the stranded case.
        let coll = collections::create_collection(conn, "ISO Coll", "tosec").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/i.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "ISO Coll", "dat", "SET/Sys").unwrap();
        let g1 = dats::create_game(conn, node, "GameOne", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            g1,
            "own.rom",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        dats::create_rom(
            conn,
            g1,
            "common.rom",
            10,
            Some("CCC"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        let g2 = dats::create_game(conn, node, "GameTwo", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            g2,
            "other.rom",
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
            g2,
            "common.rom",
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
            "INSERT INTO sources (id, path, case_sensitive, disposition)
             VALUES (220, '/lib/ToSort/SET', 0, 'consume')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('AAA', 220, 'g1.zip', 'own.rom'),
                ('CCC', 220, 'g1.zip', 'common.rom'),
                ('BBB', 220, 'g2.zip', 'other.rom'),
                ('CCC', 220, 'g2.zip', 'common.rom')",
            [],
        )
        .unwrap();
        db_config::set_output_format(conn, "SET", "zip").unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();

        // GameOne's source container is drained (its content is now in GameOne's
        // archive). The shared CCC entry — which earlier flagged the container as
        // "shared" and stranded it — no longer blocks the drain, because safety is
        // the verify-before-delete net's job, not a plan-time guess.
        let drained: Vec<&str> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Delete { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            drained.contains(&"/lib/ToSort/SET/g1.zip"),
            "GameOne's single-game consume container is drained: {drained:?}"
        );
    }

    #[test]
    fn complete_container_found_among_many_shared_only_containers() {
        let db = setup_db();
        let conn = db.conn();
        // A game whose BIOS ROM is held in many containers (as a Neo-Geo BIOS
        // would be) but whose clone-specific ROM lives in just one — only that
        // container is complete. The planner must find it by the rarest entry
        // rather than scanning every BIOS-bearing container (the merged-arcade
        // quadratic that hung Q7). The BIOS is single-game here, so it is not
        // "shared content" and the build path is unconstrained.
        let coll = collections::create_collection(conn, "Arcade", "mame").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/a.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Arcade", "dat", "SET/Sys").unwrap();
        let g = dats::create_game(conn, node, "neoclone", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            g,
            "bios.rom",
            10,
            Some("B105"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        dats::create_rom(
            conn,
            g,
            "clone.rom",
            10,
            Some("C10E"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (sha1, size) VALUES ('B105', 10), ('C10E', 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (400, '/lib/ToSort/SET', 0)",
            [],
        )
        .unwrap();
        // The one complete container holds both ROMs.
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('B105', 400, 'Sys/neoclone.zip', 'bios.rom'),
                ('C10E', 400, 'Sys/neoclone.zip', 'clone.rom')",
            [],
        )
        .unwrap();
        // The BIOS ROM is also present in 50 other (BIOS-only) containers.
        for i in 0..50 {
            conn.execute(
                &format!(
                    "INSERT INTO file_locations (sha1, source_id, path, archive_path)
                     VALUES ('B105', 400, 'Sys/other{i}.zip', 'bios.rom')"
                ),
                [],
            )
            .unwrap();
        }
        db_config::set_output_format(conn, "SET", "zip").unwrap();

        // Copy mode: no relocates/deletes, just the build — so the assertion
        // isolates which container the planner chose to build from.
        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();

        // Exactly one archive built, sourced entirely from the one complete
        // container — never from a BIOS-only container (which lacks clone.rom).
        assert_eq!(plan.summary.repack_count, 1);
        let sources: Vec<String> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Repack { sources, .. } => Some(sources.clone()),
                _ => None,
            })
            .flatten()
            .map(|s| s.path)
            .collect();
        assert!(!sources.is_empty());
        assert!(
            sources
                .iter()
                .all(|p| p == "/lib/ToSort/SET/Sys/neoclone.zip"),
            "repack must build from the one complete container, got {sources:?}"
        );
    }

    #[test]
    fn set_filter_restricts_planning_to_requested_sets() {
        let db = setup_db();
        let conn = db.conn();
        setup_dup_fixture(conn, false); // collection whose set (top segment) is "SET"

        let opts = |sets: Option<Vec<String>>| PlanOptions {
            set_filter: sets,
            default_dest: Some("/lib/ROMs".to_string()),
            default_format: OutputFormat::Loose,
            ..Default::default()
        };

        // A non-matching set is skipped entirely — no operations.
        let other = generate_plan_filtered(conn, &opts(Some(vec!["TOSEC".to_string()]))).unwrap();
        assert!(
            other.is_empty(),
            "collection in set 'SET' excluded by --set TOSEC"
        );

        // The matching set is planned.
        let matched = generate_plan_filtered(conn, &opts(Some(vec!["SET".to_string()]))).unwrap();
        assert!(!matched.is_empty(), "set 'SET' is planned when requested");
    }

    #[test]
    fn archive_complete_staged_copy_is_relocated_not_repacked() {
        let db = setup_db();
        let conn = db.conn();
        // Only a staged ToSort copy exists; the library does not hold this game.
        let coll = collections::create_collection(conn, "Test Coll", "tosec").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/test.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Test Coll", "dat", "SET/Sys").unwrap();
        let game = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            game,
            "game.rom",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition)
             VALUES (102, '/lib/ToSort/SET', 0, 'consume')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path)
             VALUES ('AAA', 102, 'Sys/Game.zip', 'game.rom')",
            [],
        )
        .unwrap();
        db_config::set_output_format(conn, "SET", "zip").unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();

        // A complete staged archive is relocated whole to its canonical path —
        // an instant rename — rather than rebuilt by repacking its entries.
        assert_eq!(
            plan.summary.repack_count, 0,
            "the staged zip is moved as-is, not rebuilt"
        );
        let relocates: Vec<_> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Relocate { source, dest, .. } => {
                    Some((source.clone(), dest.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            relocates,
            vec![(
                "/lib/ToSort/SET/Sys/Game.zip".to_string(),
                "/lib/ROMs/SET/Sys/Game.zip".to_string(),
            )]
        );
    }

    #[test]
    fn loose_staged_file_is_repacked_not_renamed_to_archive() {
        let db = setup_db();
        let conn = db.conn();
        // A complete game held only as a loose .tap under ToSort, in a zip set.
        let coll = collections::create_collection(conn, "Test Coll", "tosec").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/test.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "Test Coll", "dat", "SET/Sys").unwrap();
        let game = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            game,
            "game.tap",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition)
             VALUES (102, '/lib/ToSort/SET', 0, 'consume')",
            [],
        )
        .unwrap();
        // Loose file (archive_path NULL): NOT an archive in the target format.
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path)
             VALUES ('AAA', 102, 'Sys/game.tap', NULL)",
            [],
        )
        .unwrap();
        db_config::set_output_format(conn, "SET", "zip").unwrap();

        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                ..Default::default()
            },
        )
        .unwrap();

        // Renaming a loose .tap to .zip would mint a file whose extension lies
        // about its contents — the loose ROM must be repacked into a real zip.
        let relocates = plan
            .operations
            .iter()
            .filter(|op| matches!(op.kind, OperationKind::Relocate { .. }))
            .count();
        assert_eq!(
            relocates, 0,
            "a loose file is never relocated to an archive"
        );
        assert_eq!(
            plan.summary.repack_count, 1,
            "the loose .tap is repacked into Game.zip"
        );
        let dest = plan.operations.iter().find_map(|op| match &op.kind {
            OperationKind::Repack { dest, .. } => Some(dest.clone()),
            _ => None,
        });
        assert_eq!(dest.as_deref(), Some("/lib/ROMs/SET/Sys/Game.zip"));
    }

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("MAME", "MAME"));
        assert!(glob_match("mame", "MAME")); // case insensitive
        assert!(!glob_match("MAME", "MAME 2020"));
    }

    #[test]
    fn test_glob_match_star() {
        // * matches any sequence
        assert!(glob_match("MAME*", "MAME"));
        assert!(glob_match("MAME*", "MAME 2020"));
        assert!(glob_match("*MAME*", "FBNeo MAME 2020"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("Nintendo*", "Nintendo - NES"));
        assert!(glob_match("Nintendo*", "Nintendo - SNES"));
        assert!(!glob_match("Nintendo*", "Sega - Genesis"));
    }

    #[test]
    fn test_glob_match_question() {
        // ? matches exactly one character
        assert!(glob_match("MAME 202?", "MAME 2020"));
        assert!(glob_match("MAME 202?", "MAME 2024"));
        assert!(!glob_match("MAME 202?", "MAME 20"));
        assert!(!glob_match("MAME 202?", "MAME 20245"));
    }

    #[test]
    fn test_glob_match_complex() {
        assert!(glob_match("*NES*", "Nintendo - NES"));
        assert!(glob_match("*NES*", "NES"));
        assert!(glob_match("*-*", "Nintendo - NES"));
        assert!(glob_match("Nintendo - *", "Nintendo - Game Boy"));
        assert!(glob_match("???", "NES"));
        assert!(!glob_match("???", "SNES"));
    }

    #[test]
    fn test_glob_match_empty() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "text"));
        assert!(glob_match("*", ""));
    }

    fn make_test_rom(game_name: &str) -> MatchedRom {
        MatchedRom {
            game_name: game_name.to_string(),
            rom_name: format!("{}.rom", game_name),
            sha1: "abc123".to_string(),
            size: 1024,
            source_path: "/source/test.rom".to_string(),
            source_root: "/source".to_string(),
            archive_path: None,
            is_disk: false,
        }
    }

    #[test]
    fn test_one_g_one_r_selects_usa_over_europe() {
        use crate::filter::FilterPreferences;

        let matches = vec![
            make_test_rom("Super Mario Bros (Europe)"),
            make_test_rom("Super Mario Bros (USA)"),
            make_test_rom("Super Mario Bros (Japan)"),
        ];

        let prefs = FilterPreferences::default();
        let filtered = apply_one_g_one_r_filter(&matches, &prefs);

        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].game_name.contains("USA"));
    }

    #[test]
    fn test_one_g_one_r_excludes_cracks() {
        use crate::filter::FilterPreferences;

        let matches = vec![
            make_test_rom("Game (USA)[cr PDX]"),
            make_test_rom("Game (Europe)"),
        ];

        let prefs = FilterPreferences::default();
        let filtered = apply_one_g_one_r_filter(&matches, &prefs);

        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].game_name.contains("Europe"));
    }

    #[test]
    fn test_one_g_one_r_excludes_bad_dumps() {
        use crate::filter::FilterPreferences;

        let matches = vec![
            make_test_rom("Game (USA)[b]"),
            make_test_rom("Game (Japan)"),
        ];

        let prefs = FilterPreferences::default();
        let filtered = apply_one_g_one_r_filter(&matches, &prefs);

        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].game_name.contains("Japan"));
    }

    #[test]
    fn test_one_g_one_r_different_games_not_merged() {
        use crate::filter::FilterPreferences;

        let matches = vec![
            make_test_rom("Super Mario Bros (USA)"),
            make_test_rom("Tetris (USA)"),
        ];

        let prefs = FilterPreferences::default();
        let filtered = apply_one_g_one_r_filter(&matches, &prefs);

        // Both games should remain (different titles)
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_one_g_one_r_custom_region_priority() {
        use crate::filter::FilterPreferences;

        let matches = vec![make_test_rom("Game (USA)"), make_test_rom("Game (Japan)")];

        // Prefer Japan over USA
        let prefs = FilterPreferences::with_regions(vec!["Japan".to_string(), "USA".to_string()]);
        let filtered = apply_one_g_one_r_filter(&matches, &prefs);

        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].game_name.contains("Japan"));
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

        // Both games' canonical archives are recorded as desired destinations.
        assert!(state.dest_paths.contains("/lib/ROMs/ARCADE/puckman.zip"));
        assert!(state.dest_paths.contains("/lib/ROMs/ARCADE/pacmanm.zip"));

        // The crux: under split, the inherited shared ROM (AAA) belongs in the
        // PARENT's archive, never the clone's — so a loose copy of AAA sitting in
        // the clone's folder is preserved by `puckman.zip`, not `pacmanm.zip`.
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

        // Only BBB is of interest; AAA must not be indexed even though it is a
        // desired archive member — the index stays scoped to the caller's set.
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
        // dest_paths are unconditional, so both archives are still recorded.
        assert!(state.dest_paths.contains("/lib/ROMs/ARCADE/puckman.zip"));
    }

    #[test]
    fn desired_state_records_loose_paths_and_no_archive_homes() {
        let db = setup_db();
        let conn = db.conn();
        // A loose-format collection: a single-ROM game placed flat, a multi-ROM
        // game placed in its own folder. Loose collections hold no archives, so
        // they contribute destination paths but never archive homes.
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

        // Single-ROM game flat; multi-ROM game in its own folder.
        assert!(state.dest_paths.contains("/lib/ROMs/Tapes/solo.tap"));
        assert!(state.dest_paths.contains("/lib/ROMs/Tapes/Duo/duo-a.tap"));
        assert!(state.dest_paths.contains("/lib/ROMs/Tapes/Duo/duo-b.tap"));
        assert!(
            state.archive_homes.is_empty(),
            "a loose collection contributes no archive homes"
        );
    }
}
