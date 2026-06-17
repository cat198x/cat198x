//! Plan generation logic

use anyhow::Result;
use rusqlite::Connection;
use sha2::{Digest as Sha2Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};

use super::{CollectionPlanStat, Plan, SourceRef};
use crate::config::{MergeMode, OutputFormat};
use crate::db::{collections, config as db_config, dats};
use crate::filter::{RomCandidate, parse_game_name, select_preferred};

/// Above this many match-rows, a collection is skipped rather than planned.
/// `find_matched_roms` materialises every (ROM × held-location) pair, at
/// roughly half a kilobyte each, so tens of millions of rows would need many
/// gigabytes and risk OOM. The only collections that reach this are MAME-style
/// meta-aggregates (e.g. `all_non-zipped_content`) whose "games" list content
/// held across hundreds of files — not real romsets to place. The largest
/// legitimate set seen, FinalBurn Neo - Arcade Games, expands to ~7.9M rows,
/// comfortably under the cap.
const MAX_MATCH_ROWS: i64 = 20_000_000;

/// A version with fewer ROMs than this cannot plausibly reach `MAX_MATCH_ROWS`
/// (it would take an implausible average per-ROM fan-out), so the expansion
/// guard's query is skipped for it. This keeps the guard free for the hundreds
/// of small collections and pays its cost only for the few large ones.
const GUARD_ROM_THRESHOLD: i64 = 50_000;

/// A matched ROM ready for planning
#[derive(Debug, Clone)]
pub struct MatchedRom {
    /// Collection name
    pub collection: String,
    /// Game name
    pub game_name: String,
    /// ROM name (filename within game folder)
    pub rom_name: String,
    /// SHA1 hash
    pub sha1: String,
    /// File size
    pub size: i64,
    /// Source file location
    pub source_path: String,
    /// Source directory root
    pub source_root: String,
    /// Archive path (None for loose files)
    pub archive_path: Option<String>,
    /// True for a `<disk>` (CHD): stored loose in a machine folder as
    /// `<game>/<rom_name>.chd`, never packed into an archive.
    pub is_disk: bool,
}

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
    /// Move files into place (and delete the source) instead of copying — a true
    /// in-place tidy rather than a duplicating copy. Off (copy) by default.
    pub move_files: bool,
}

/// Generate a plan for all configured collections with default options.
pub fn generate_plan(conn: &Connection) -> Result<Plan> {
    generate_plan_filtered(conn, &PlanOptions::default())
}

/// The held-content SHA1s whose content satisfies more than one distinct DAT game
/// across all active versions — genuinely distinct catalogue entries that happen
/// to be byte-identical (multi-disk sets sharing a data disk, re-releases, common
/// loaders, a parent/clone romset). Such content must be *copied* to each
/// destination and never moved or deleted: a single physical file can be the
/// matched source for many destinations, and consuming it to satisfy one strands
/// the rest.
///
/// The key is the **held file's** SHA1 (what `MatchedRom.sha1` carries), and the
/// match mirrors `find_matched_roms` exactly — direct SHA1, headerless SHA1, or
/// CRC32 + size for SHA1-less DAT entries. The CRC32 arm is load-bearing for
/// arcade: MAME/FinalBurn Neo DATs are CRC-only (NULL `sha1`), so a SHA1-only
/// match silently classed their shared romsets as *not* shared, letting the
/// planner relocate or delete a container several games depend on.
fn compute_shared_content(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "WITH active_roms AS (
             SELECT r.sha1 AS rom_sha1, r.crc32 AS rom_crc32, r.size AS rom_size,
                    g.id AS game_id
               FROM dat_roms r
               JOIN dat_games g ON g.id = r.game_id
               JOIN dat_nodes dn ON dn.id = g.node_id
               JOIN collection_versions cv ON cv.id = dn.version_id
              WHERE cv.is_active = 1
         ),
         matched AS (
             SELECT f.sha1 AS file_sha1, ar.game_id
               FROM files f JOIN active_roms ar ON f.sha1 = ar.rom_sha1
              WHERE ar.rom_sha1 IS NOT NULL AND ar.rom_sha1 <> ''
             UNION
             SELECT f.sha1, ar.game_id
               FROM files f JOIN active_roms ar ON f.sha1_no_header = ar.rom_sha1
              WHERE ar.rom_sha1 IS NOT NULL AND ar.rom_sha1 <> ''
             UNION
             SELECT f.sha1, ar.game_id
               FROM files f JOIN active_roms ar
                    ON f.crc32 = ar.rom_crc32 AND f.size = ar.rom_size
              WHERE ar.rom_sha1 IS NULL AND ar.rom_crc32 IS NOT NULL
         )
         SELECT file_sha1
           FROM matched
          WHERE file_sha1 IN (SELECT sha1 FROM file_locations)
          GROUP BY file_sha1
         HAVING COUNT(DISTINCT game_id) > 1",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// The source archive files whose inner entries satisfy more than one distinct
/// DAT game — a single physical container holding ROMs for several games (a
/// multi-program bundle, a romset shared across parent/clone, etc.). Such a
/// container must never be *relocated* whole or deleted to satisfy one game, or
/// the others it also sources are stranded; each game is repacked from it
/// instead (which extracts only its own entries and leaves the container in
/// place). The key is the full source path (`source_root/source_path`), matching
/// the container key used during planning.
///
/// Entries match DAT ROMs the same three ways as `find_matched_roms` — direct
/// SHA1, headerless SHA1, or CRC32 + size for SHA1-less entries — joining through
/// `files` to reach the CRC32/size of each held entry. The CRC32 arm is what
/// makes this correct for CRC-only arcade DATs (MAME/FinalBurn Neo): without it a
/// merged container sourcing a parent and its clones reads as serving one game,
/// so the planner relocates it to the parent and the clones' relocations then
/// race on a vanished source.
fn compute_shared_containers(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "WITH container_games AS (
             SELECT fl.source_id, fl.path, g.id AS game_id
               FROM file_locations fl
               JOIN files f ON f.sha1 = fl.sha1
               JOIN dat_roms r ON r.sha1 = f.sha1
               JOIN dat_games g ON g.id = r.game_id
               JOIN dat_nodes dn ON dn.id = g.node_id
               JOIN collection_versions cv ON cv.id = dn.version_id
              WHERE cv.is_active = 1 AND fl.archive_path IS NOT NULL
                AND r.sha1 IS NOT NULL AND r.sha1 <> ''
             UNION
             SELECT fl.source_id, fl.path, g.id
               FROM file_locations fl
               JOIN files f ON f.sha1 = fl.sha1
               JOIN dat_roms r ON r.sha1 = f.sha1_no_header
               JOIN dat_games g ON g.id = r.game_id
               JOIN dat_nodes dn ON dn.id = g.node_id
               JOIN collection_versions cv ON cv.id = dn.version_id
              WHERE cv.is_active = 1 AND fl.archive_path IS NOT NULL
                AND r.sha1 IS NOT NULL AND r.sha1 <> ''
             UNION
             SELECT fl.source_id, fl.path, g.id
               FROM file_locations fl
               JOIN files f ON f.sha1 = fl.sha1
               JOIN dat_roms r ON r.crc32 = f.crc32 AND r.size = f.size
               JOIN dat_games g ON g.id = r.game_id
               JOIN dat_nodes dn ON dn.id = g.node_id
               JOIN collection_versions cv ON cv.id = dn.version_id
              WHERE cv.is_active = 1 AND fl.archive_path IS NOT NULL
                AND r.sha1 IS NULL AND r.crc32 IS NOT NULL
         )
         SELECT s.path || '/' || cg.path
           FROM container_games cg
           JOIN sources s ON s.id = cg.source_id
          GROUP BY cg.source_id, cg.path
         HAVING COUNT(DISTINCT cg.game_id) > 1",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// Whether a version is disk-only: it has at least one `<disk>` and no `<rom>`.
/// Such a collection places loose `<game>/<name>.chd` and never a `<game>.zip`,
/// so it can share a destination root with a ROM collection without colliding.
fn version_is_disk_only(conn: &Connection, version_id: i64) -> Result<bool> {
    let has_disk: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dat_roms r
                         JOIN dat_games g ON g.id = r.game_id
                         JOIN dat_nodes n ON n.id = g.node_id
                        WHERE n.version_id = ?1 AND r.is_disk = 1)",
        [version_id],
        |row| row.get(0),
    )?;
    let has_rom: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dat_roms r
                         JOIN dat_games g ON g.id = r.game_id
                         JOIN dat_nodes n ON n.id = g.node_id
                        WHERE n.version_id = ?1 AND r.is_disk = 0)",
        [version_id],
        |row| row.get(0),
    )?;
    Ok(has_disk && !has_rom)
}

/// Refuse to plan when two collections in scope resolve to the same destination
/// root.
///
/// A destination must uniquely identify its source. Two collections sharing a
/// root silently overwrite each other's same-named games — `Arcade/klax` and
/// `Game Gear/klax` both writing `<root>/klax.zip`, last-writer-wins. That is the
/// FinalBurn Neo flat-layout failure that destroyed ~1,600 games before
/// per-machine folders. The error names the colliding collections and their
/// shared root; the remedy is its own message — give each a distinct `dest_path`.
///
/// Scope matches the plan: only collections that pass `dat_filter`/`set_filter`
/// and resolve to a destination are checked, so a narrow plan isn't blocked by a
/// collision outside it.
///
/// Collections are grouped by `(root, output namespace)`, not root alone. A
/// disk-only (CHD) collection writes loose `<game>/<name>.chd`, which never
/// collides with a ROM collection's `<game>.zip` at the same root — so a game's
/// ROMs and its CHD can live together, the standard arcade layout. Only
/// same-namespace collections (two ROM sets, or two CHD sets) collide.
fn check_unique_destinations(
    conn: &Connection,
    opts: &PlanOptions,
    all_collections: &[collections::Collection],
) -> Result<()> {
    // Key: (destination root, is the collection disk-only). Value: collection
    // names sharing that key — a collision when more than one.
    let mut owners: BTreeMap<(String, bool), Vec<String>> = BTreeMap::new();
    for collection in all_collections {
        if let Some(pattern) = opts.dat_filter.as_deref()
            && !glob_match(pattern, &collection.name)
        {
            continue;
        }
        let version = match collections::get_active_version(conn, collection.id)? {
            Some(v) => v,
            None => continue,
        };
        let hierarchy =
            dats::primary_node_path(conn, version.id)?.unwrap_or_else(|| collection.name.clone());
        if let Some(sets) = opts.set_filter.as_ref() {
            let set = hierarchy.split('/').next().unwrap_or(hierarchy.as_str());
            if !sets.iter().any(|s| s == set) {
                continue;
            }
        }
        let cfg = db_config::get_collection_config(conn, &collection.name)?;
        let explicit = cfg.as_ref().and_then(|c| c.dest_path.as_deref());
        if let Some(root) = resolve_dest_root(explicit, opts.default_dest.as_deref(), &hierarchy) {
            let disk_only = version_is_disk_only(conn, version.id)?;
            owners
                .entry((root, disk_only))
                .or_default()
                .push(collection.name.clone());
        }
    }

    let mut collisions: Vec<(&(String, bool), &Vec<String>)> =
        owners.iter().filter(|(_, c)| c.len() > 1).collect();
    if collisions.is_empty() {
        return Ok(());
    }
    collisions.sort_by_key(|((root, disk), _)| (root.clone(), *disk));

    let mut msg = String::from(
        "Refusing to plan: collections share a destination root, so their \
         same-named games would overwrite each other. Give each collection a \
         distinct dest_path (e.g. a per-machine subfolder).\n",
    );
    for ((root, disk_only), colls) in collisions {
        let kind = if *disk_only { "CHD" } else { "ROM" };
        msg.push_str(&format!(
            "  {} ({} outputs) <- {}\n",
            root,
            kind,
            colls.join(", ")
        ));
    }
    anyhow::bail!(msg);
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
        println!(
            "{} shared content(s) span multiple entries — copied to each, not moved.",
            shared.len()
        );
    }

    // Containers (archive files) whose entries serve more than one game must not
    // be relocated whole or deleted — each game repacks its own entries instead.
    let shared_containers = compute_shared_containers(conn)?;
    if !shared_containers.is_empty() {
        println!(
            "{} container(s) source multiple games — repacked per game, not relocated.",
            shared_containers.len()
        );
    }

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

        let dest_root = match resolve_dest_root(explicit, default_dest, &hierarchy) {
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
            println!(
                "Skipping {} — match expansion exceeds the {}-row memory cap \
                 (a meta-aggregate, not a placeable romset).",
                collection.name, MAX_MATCH_ROWS
            );
            plan.skipped_oversized.push(format!(
                "{} (>{} match-rows)",
                collection.name, MAX_MATCH_ROWS
            ));
            continue;
        }

        planned_any = true;
        println!("Planning for: {} (v{})", collection.name, version.version);

        // Effective merge mode (explicit per-collection → per-set rule →
        // library-wide default). Split mode drops a clone's inherited
        // (merge-tagged) ROMs from its placement so they live only in the parent;
        // non-merged places every ROM the DAT lists per game. Merged is not yet
        // wired in the planner. Shared with `compute_desired_state`.
        let merge_mode = effective_merge_mode(conn, opts, cfg.as_ref(), &hierarchy)?;
        if merge_mode == MergeMode::Merged {
            println!(
                "  note: merged mode is not yet implemented in the planner; \
                 planning {} as non-merged.",
                collection.name
            );
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
                    println!(
                        "  1G1R: {} -> {} ROMs (filtered {} variants)",
                        original_count,
                        filtered.len(),
                        original_count - filtered.len()
                    );
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
                // LOOSE: one file per ROM. A single-ROM game stays flat
                // (dest/rom); a multi-ROM game gets a folder (dest/game/rom), so
                // count the *distinct* ROMs per game up front — counting match
                // rows would mistake a ROM held in several locations for a
                // multi-ROM game and wrongly add a folder level.
                let mut roms_per_game: HashMap<String, HashSet<String>> = HashMap::new();
                for m in &matches {
                    roms_per_game
                        .entry(m.game_name.clone())
                        .or_default()
                        .insert(m.rom_name.clone());
                }

                // Group every held copy by its canonical destination. Copies of
                // one ROM (same content from different locations — e.g. a file
                // already placed in the library plus a staged copy under ToSort)
                // share a destination: we keep exactly one canonical copy there
                // and quarantine the rest as duplicates.
                let mut by_dest: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
                for m in matches {
                    let multi_rom = roms_per_game
                        .get(&m.game_name)
                        .map(|s| s.len())
                        .unwrap_or(1)
                        > 1;
                    let dest = build_dest_path(&dest_root, &m.game_name, &m.rom_name, multi_rom);
                    by_dest.entry(dest).or_default().push(m);
                }

                for (dest, copies) in by_dest {
                    // Shared content (the same bytes also belong to another entry)
                    // must never consume its source: a "duplicate" copy here may be
                    // the matched source for a different destination. So copy it
                    // into place even in move mode, and skip the redundancy delete.
                    let shared_here = copies.iter().any(|m| shared.contains(&m.sha1));

                    // A loose copy already sitting at the destination is the
                    // canonical one — an in-memory comparison (the match carries
                    // its location), so no per-file disk stat or catalogue scan.
                    let at_dest = copies.iter().position(|m| {
                        m.archive_path.is_none()
                            && format!("{}/{}", m.source_root, m.source_path) == dest
                    });
                    let keep = match at_dest {
                        Some(i) => {
                            already_correct += 1;
                            Some(i)
                        }
                        None => {
                            // Nothing at dest yet: place the first copy there.
                            let m = &copies[0];
                            bytes += m.size as u64;
                            let source = SourceRef {
                                path: format!("{}/{}", m.source_root, m.source_path),
                                archive_path: m.archive_path.clone(),
                                sha1: m.sha1.clone(),
                                entry_name: None,
                            };
                            if opts.move_files && !shared_here {
                                plan.add_move(source, dest.clone(), m.size as u64);
                            } else {
                                plan.add_copy(source, dest.clone(), m.size as u64);
                            }
                            to_write += 1;
                            Some(0)
                        }
                    };
                    // Every other loose copy is an exact-content duplicate of the
                    // one kept at the destination. In move mode (an in-place tidy)
                    // delete the redundant copy — nothing unique is lost, since the
                    // kept copy preserves the bytes. In copy mode, or for shared
                    // content, leave it be: a copy run must not remove source files,
                    // and a shared copy may be needed by another destination.
                    if opts.move_files && !shared_here {
                        for (i, m) in copies.iter().enumerate() {
                            if Some(i) == keep || m.archive_path.is_some() {
                                continue;
                            }
                            let path = format!("{}/{}", m.source_root, m.source_path);
                            if path == dest || is_in_library(&path, default_dest, &dest_root) {
                                continue;
                            }
                            plan.add_delete(path);
                            deduped += 1;
                        }
                    }
                }
                let verb = if opts.move_files { "move" } else { "copy" };
                println!(
                    "  {} already correct, {} to {}, {} duplicate(s) to delete",
                    already_correct, to_write, verb, deduped
                );
            }
            Some(tag) => {
                // ARCHIVE: one archive per game at <dest_root>/<game>.<ext>. A
                // game's ROMs may be held in several physical archives (the
                // library copy plus staged ToSort copies); one canonical archive
                // belongs at dest, and duplicate whole-archive copies elsewhere
                // are quarantined.
                let ext = archive_extension(tag);
                let mut games: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
                for m in matches {
                    games.entry(m.game_name.clone()).or_default().push(m);
                }

                for (game_name, gmatches) in games {
                    let dest = format!("{}/{}.{}", dest_root.trim_end_matches('/'), game_name, ext);

                    // Distinct expected entries (canonical name + SHA1) and the
                    // source containers that hold them.
                    let mut expected: Vec<(String, String)> = Vec::new();
                    let mut seen = HashSet::new();
                    let mut containers: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
                    for m in gmatches {
                        if seen.insert((m.rom_name.clone(), m.sha1.clone())) {
                            expected.push((m.rom_name.clone(), m.sha1.clone()));
                        }
                        let container = format!("{}/{}", m.source_root, m.source_path);
                        containers.entry(container).or_default().push(m);
                    }
                    // Completeness is a set-membership test, not a scan. Merged
                    // arcade sets share ROMs across thousands of containers (a
                    // Neo-Geo BIOS ROM can sit in 5,000+ files), so a single
                    // game's `containers` map is huge; the old per-container
                    // `expected × entries` scan went quadratic and hung on
                    // FinalBurn Neo. Build, once per game: a per-container set of
                    // the (name, sha1) pairs it holds, the size of each ROM by
                    // name, and an index from each expected entry to the
                    // containers holding it. SHA1s compare case-insensitively, so
                    // normalise to lower-case in the keys.
                    let key =
                        |name: &str, sha1: &str| (name.to_string(), sha1.to_ascii_lowercase());
                    let expected_keys: Vec<(String, String)> =
                        expected.iter().map(|(n, s)| key(n, s)).collect();
                    let mut container_keys: HashMap<&str, HashSet<(String, String)>> =
                        HashMap::new();
                    let mut holders: HashMap<(String, String), Vec<&str>> = HashMap::new();
                    let mut name_size: HashMap<&str, u64> = HashMap::new();
                    for (path, entries) in &containers {
                        let set = container_keys.entry(path.as_str()).or_default();
                        for m in entries {
                            name_size
                                .entry(m.rom_name.as_str())
                                .or_insert(m.size as u64);
                            let k = key(&m.rom_name, &m.sha1);
                            if set.insert(k.clone()) {
                                holders.entry(k).or_default().push(path.as_str());
                            }
                        }
                    }
                    // A container is complete iff it holds every expected entry —
                    // now O(expected) hash lookups, not a nested scan.
                    let is_complete = |path: &str| {
                        container_keys
                            .get(path)
                            .is_some_and(|set| expected_keys.iter().all(|k| set.contains(k)))
                    };

                    // If any of this game's content is shared with another entry,
                    // never consume a source for it — build the archive by copying
                    // (repack without deleting sources) and don't relocate or delete
                    // any container, since those bytes may be needed elsewhere.
                    let game_shared = expected.iter().any(|(_, sha1)| shared.contains(sha1));

                    // The canonical container is the complete one at dest if it
                    // exists, otherwise a complete one elsewhere we build from. A
                    // complete container must hold the *rarest* expected entry, so
                    // only the containers holding it can qualify: in a merged set
                    // the game's own ROM sits in one or two files while shared ROMs
                    // sit in thousands, turning a full scan into a few checks.
                    let complete_at_dest = is_complete(&dest);
                    let build_from = if complete_at_dest {
                        Some(dest.clone())
                    } else {
                        expected_keys
                            .iter()
                            .map(|k| holders.get(k).map(Vec::as_slice).unwrap_or(&[]))
                            .min_by_key(|paths| paths.len())
                            .and_then(|candidates| {
                                candidates
                                    .iter()
                                    .copied()
                                    .find(|&p| is_complete(p))
                                    .map(str::to_string)
                            })
                    };

                    // A complete archive already staged somewhere other than the
                    // destination — relocate the whole file there rather than
                    // rebuilding it (the staged ToSort case: an instant rename
                    // instead of reading and recompressing every entry).
                    let staged_complete: Option<String> = match &build_from {
                        Some(p) if *p != dest => Some(p.clone()),
                        _ => None,
                    };

                    // A content-correct `torrentzip` still needs its deterministic
                    // stamp, which the catalogue doesn't record, so it is rebuilt
                    // rather than read off the network to check — `zip` is correct
                    // on content alone.
                    if complete_at_dest && tag != "torrentzip" {
                        already_correct += expected.len();
                    } else if let Some(ref src) = staged_complete
                        && opts.move_files
                        && !game_shared
                        && !shared_containers.contains(src)
                        && tag != "torrentzip"
                        && is_relocatable_archive(&containers[src], src, ext)
                    {
                        // Relocate the complete staged archive to its destination.
                        let size: u64 = containers[src].iter().map(|m| m.size as u64).sum();
                        bytes += size;
                        plan.add_relocate(src.clone(), dest.clone(), size);
                        relocated += 1;
                    } else {
                        // Build the canonical archive at dest: from one complete
                        // container if there is one, else from whatever entries we
                        // hold (scattered across containers). Used in copy mode, for
                        // torrentzip, or when no single container is complete.
                        let sources: Vec<SourceRef> = match &build_from {
                            Some(p) => containers[p].iter().map(source_ref_for).collect(),
                            None => containers
                                .values()
                                .flat_map(|e| e.iter().map(source_ref_for))
                                .collect(),
                        };
                        let size: u64 = expected
                            .iter()
                            .filter_map(|(name, _)| name_size.get(name.as_str()).copied())
                            .sum();
                        bytes += size;
                        // Consume loose sources only in move mode and only when the
                        // content isn't shared — a shared source may feed another
                        // game's archive too.
                        plan.add_repack(
                            sources,
                            dest.clone(),
                            tag.to_string(),
                            size,
                            opts.move_files && !game_shared,
                        );
                        to_write += 1;
                    }

                    // Delete duplicate whole-archive copies: any container that is
                    // neither the destination nor the one we build from holds an
                    // exact-content copy already preserved at the destination. In
                    // move mode remove it; in copy mode, or when the content is
                    // shared with another entry, leave sources untouched. A
                    // container that also sources other games is never deleted —
                    // those games still need to repack from it.
                    if opts.move_files && !game_shared {
                        for path in containers.keys() {
                            if *path == dest
                                || build_from.as_deref() == Some(path.as_str())
                                || shared_containers.contains(path)
                                || is_in_library(path, default_dest, &dest_root)
                            {
                                continue;
                            }
                            plan.add_delete(path.clone());
                            deduped += 1;
                        }
                    }
                }
                println!(
                    "  {} ROMs already archived, {} to relocate, {} archive(s) to build, {} duplicate(s) to delete",
                    already_correct, relocated, to_write, deduped
                );
            }
        }

        // Plan any CHDs loose, regardless of the set's format. (Disk dedups are
        // reported within the helper, like the other branches' own counts.)
        if !disk_matches.is_empty() {
            let d = plan_disk_matches(disk_matches, &dest_root, opts, &shared, &mut plan);
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

    // Never skip silently: report collections left out because no destination
    // could be resolved, and how to include them. The full list rides on the
    // plan so the caller can write it out for review.
    if !skipped_no_dest.is_empty() {
        println!();
        println!(
            "{} collection(s) skipped — no destination resolved.",
            skipped_no_dest.len()
        );
        println!("  Set one per collection:  cat198x config set <collection> dest_path <path>");
        println!("  or library-wide:         cat198x config set-default dest_path <path>");
    }

    // Report collections left out because their match expansion is too large to
    // plan safely (a meta-aggregate, not a romset). Already named individually
    // above as they were hit; this is the rollup.
    if !plan.skipped_oversized.is_empty() {
        println!();
        println!(
            "{} collection(s) skipped — match expansion over the {}-row memory cap.",
            plan.skipped_oversized.len(),
            MAX_MATCH_ROWS
        );
    }

    if let Some(pattern) = dat_filter
        && !filter_matched_any
    {
        println!("No collections match the filter: {}", pattern);
    } else if !planned_any && skipped_no_dest.is_empty() && plan.skipped_oversized.is_empty() {
        println!("No collections with an active version to plan.");
    }

    plan.skipped_no_dest = skipped_no_dest;
    Ok(plan)
}

/// Count the match-rows a version's plan would materialise, bounded to `cap + 1`.
///
/// Mirrors the joins of [`find_matched_roms`] but only counts rows, and the
/// inner `LIMIT cap + 1` stops the join the moment the cap is reached — so a
/// pathological collection is detected in bounded time without ever producing
/// (or holding) its full expansion. A version with too few ROMs to plausibly
/// reach the cap returns its ROM count directly, skipping the join entirely.
fn count_match_rows_capped(conn: &Connection, version_id: i64, cap: i64) -> Result<i64> {
    let rom_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?1 AND r.status != 'nodump'",
        [version_id],
        |row| row.get(0),
    )?;
    if rom_count < GUARD_ROM_THRESHOLD {
        // A lower bound well under the cap — cheap proof the collection is safe.
        return Ok(rom_count.min(cap));
    }
    count_expansion_capped(conn, version_id, cap)
}

/// The exact match expansion, counted only up to `cap + 1` (the inner `LIMIT`
/// halts the join there). Split out from the ROM-count gate so the bounded count
/// is testable without a fixture large enough to pass the gate.
fn count_expansion_capped(conn: &Connection, version_id: i64, cap: i64) -> Result<i64> {
    // The expansion is one row per (matched ROM × held location), exactly what
    // the planner materialises; keep `rom_id` so a SHA1 matching several ROMs
    // counts once per ROM, matching `find_matched_roms`'s UNION cardinality.
    let count: i64 = conn.query_row(
        "WITH vroms AS (
            SELECT r.id, r.sha1, r.crc32, r.size
            FROM dat_roms r
            JOIN dat_games g ON r.game_id = g.id
            JOIN dat_nodes n ON g.node_id = n.id
            WHERE n.version_id = ?1 AND r.status != 'nodump'
         ),
         matched AS (
            SELECT vr.id AS rom_id, f.sha1 AS msha1
            FROM vroms vr JOIN files f ON f.sha1 = vr.sha1 WHERE vr.sha1 IS NOT NULL
            UNION
            SELECT vr.id, f.sha1
            FROM vroms vr JOIN files f ON f.sha1_no_header = vr.sha1 WHERE vr.sha1 IS NOT NULL
            UNION
            SELECT vr.id, f.sha1
            FROM vroms vr JOIN files f ON f.crc32 = vr.crc32 AND f.size = vr.size
            WHERE vr.sha1 IS NULL AND vr.crc32 IS NOT NULL
         )
         SELECT COUNT(*) FROM (
            SELECT 1 FROM matched m JOIN file_locations fl ON fl.sha1 = m.msha1 LIMIT ?2
         )",
        rusqlite::params![version_id, cap + 1],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Find all ROMs in one collection version that have a matching held file.
///
/// Performance is critical here — this runs once per collection, and a full
/// library is thousands of collections. The match has three modes: a DAT SHA1
/// may be the file's headered or headerless hash, and a SHA1-less DAT entry
/// matches on CRC + size. Expressed as a single `OR` join, SQLite can't drive
/// from this version's ROMs into the file index and instead scans the whole
/// `files` table per call (~13s each on a real library). Splitting the modes
/// into a `UNION` lets each branch use an index (files PK on `sha1`,
/// `idx_files_sha1_no_header`, and `idx_files_crc32_size` for the CRC-only
/// branch that CRC-only DATs like MAME/FinalBurn Neo rely on), so the query
/// starts from the version's ROMs and runs in milliseconds rather than
/// full-scanning the files table per ROM. We select the *file's* sha1 and size
/// (not the DAT's) —
/// that's the true content placed at the destination, which
/// `is_file_correct_at_dest` verifies.
fn find_matched_roms(
    conn: &Connection,
    version_id: i64,
    collection_name: &str,
    split: bool,
) -> Result<Vec<MatchedRom>> {
    let mut stmt = conn.prepare(
        // The split filter (`?2`) drops a clone's inherited ROMs: when split is
        // on, keep a ROM only if its game is a parent (`parent_name IS NULL`) or
        // the ROM carries no merge tag (a clone's own unique ROM). This mirrors
        // the split rule in `calculate_rom_requirements` so placement and
        // completeness agree. When split is off, `?2` is 0 and the term is true
        // for every ROM, leaving non-merged behaviour unchanged.
        "WITH vroms AS (
            SELECT r.id, r.game_id, r.name, r.sha1, r.crc32, r.size, r.is_disk
            FROM dat_roms r
            JOIN dat_games g ON r.game_id = g.id
            JOIN dat_nodes n ON g.node_id = n.id
            WHERE n.version_id = ?1 AND r.status != 'nodump'
              AND (?2 = 0 OR g.parent_name IS NULL OR r.merge_tag IS NULL)
         ),
         matched AS (
            SELECT vr.id AS rom_id, f.sha1, f.size
            FROM vroms vr JOIN files f ON f.sha1 = vr.sha1
            WHERE vr.sha1 IS NOT NULL
            UNION
            SELECT vr.id, f.sha1, f.size
            FROM vroms vr JOIN files f ON f.sha1_no_header = vr.sha1
            WHERE vr.sha1 IS NOT NULL
            UNION
            SELECT vr.id, f.sha1, f.size
            FROM vroms vr JOIN files f ON f.crc32 = vr.crc32 AND f.size = vr.size
            WHERE vr.sha1 IS NULL AND vr.crc32 IS NOT NULL
         )
         SELECT g.name, vr.name, m.sha1, m.size, fl.path, s.path, fl.archive_path, vr.is_disk
         FROM matched m
         JOIN vroms vr ON vr.id = m.rom_id
         JOIN dat_games g ON vr.game_id = g.id
         JOIN file_locations fl ON fl.sha1 = m.sha1
         JOIN sources s ON fl.source_id = s.id
         ORDER BY g.name, vr.name",
    )?;

    let matches = stmt
        .query_map(rusqlite::params![version_id, split], |row| {
            Ok(MatchedRom {
                collection: collection_name.to_string(),
                game_name: row.get(0)?,
                rom_name: row.get(1)?,
                sha1: row.get(2)?,
                size: row.get(3)?,
                source_path: row.get(4)?,
                source_root: row.get(5)?,
                archive_path: row.get(6)?,
                is_disk: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(matches)
}

/// Build destination path for a ROM
/// The effective output format: an explicit per-collection setting wins,
/// otherwise the library-wide default. An unrecognised string falls back to the
/// default rather than failing the whole plan.
fn resolve_output_format(explicit: Option<&str>, default: OutputFormat) -> OutputFormat {
    match explicit.map(str::to_ascii_lowercase).as_deref() {
        Some("loose") => OutputFormat::Loose,
        Some("zip") => OutputFormat::Zip,
        Some("torrentzip") => OutputFormat::TorrentZip,
        Some("7z") => OutputFormat::SevenZip,
        _ => default,
    }
}

/// The effective merge mode for a collection: an explicit setting (per-collection
/// or per-set) wins, otherwise the library-wide default. An unrecognised string
/// falls back to the default rather than failing the whole plan. The kebab-case
/// strings match the `MergeMode` serde representation in `config::types`.
fn resolve_merge_mode(explicit: Option<&str>, default: MergeMode) -> MergeMode {
    match explicit.map(str::to_ascii_lowercase).as_deref() {
        Some("non-merged") => MergeMode::NonMerged,
        Some("merged") => MergeMode::Merged,
        Some("split") => MergeMode::Split,
        _ => default,
    }
}

/// The repack format tag for an archive format, or `None` for loose (which is
/// copied, not repacked).
fn archive_format_tag(format: OutputFormat) -> Option<&'static str> {
    match format {
        OutputFormat::Loose => None,
        OutputFormat::Zip => Some("zip"),
        OutputFormat::TorrentZip => Some("torrentzip"),
        OutputFormat::SevenZip => Some("7z"),
    }
}

/// The archive file extension for a repack format tag.
fn archive_extension(tag: &str) -> &'static str {
    if tag == "7z" { "7z" } else { "zip" }
}

/// Whether a complete source container can be relocated whole to its
/// destination rather than repacked. A relocate is a rename, so it only
/// preserves the set's format when the source is *already* an archive in that
/// exact format: every entry must live inside an archive (not be a loose ROM),
/// and the source file's extension must match the target's. Renaming a loose
/// `.tap`/`.cue`/`.z80`, or a `.7z` into a zip set, would mint a file whose
/// extension lies about its contents — those must be repacked instead.
fn is_relocatable_archive(entries: &[MatchedRom], src: &str, ext: &str) -> bool {
    !entries.is_empty()
        && entries.iter().all(|m| m.archive_path.is_some())
        && src
            .rsplit('.')
            .next()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Build a repack source reference from a matched ROM, carrying its canonical
/// ROM name as the archive entry name.
fn source_ref_for(m: &MatchedRom) -> SourceRef {
    SourceRef {
        path: format!("{}/{}", m.source_root, m.source_path),
        archive_path: m.archive_path.clone(),
        sha1: m.sha1.clone(),
        entry_name: Some(m.rom_name.clone()),
    }
}

/// Resolve a collection's destination root, in order of precedence:
///   1. an explicit per-collection `dest_path`, used as-is;
///   2. otherwise the library-wide `default_dest` joined with the collection's
///      library path (`hierarchy`), so a whole set is tidied from one setting;
///   3. otherwise `None` — no destination, and the caller skips the collection.
fn resolve_dest_root(
    explicit: Option<&str>,
    default_dest: Option<&str>,
    hierarchy: &str,
) -> Option<String> {
    match explicit {
        Some(p) => Some(p.to_string()),
        None => default_dest.map(|base| format!("{}/{}", base.trim_end_matches('/'), hierarchy)),
    }
}

/// Build the on-disk destination for one ROM under its collection's root.
///
/// Loose layout: a single-ROM game is placed flat as `dest_root/rom_name` — the
/// common TOSEC case, where one "game" is one file and a wrapping folder would
/// just be noise. A multi-ROM game gets its own folder,
/// `dest_root/game_name/rom_name`, so its parts stay together and don't collide
/// with other games' files.
fn build_dest_path(dest_root: &str, game_name: &str, rom_name: &str, multi_rom: bool) -> String {
    let root = dest_root.trim_end_matches('/');
    if multi_rom {
        format!("{}/{}/{}", root, game_name, rom_name)
    } else {
        format!("{}/{}", root, rom_name)
    }
}

/// Counts from planning a batch of CHD (disk) matches.
#[derive(Default)]
struct DiskPlanCounts {
    already_correct: usize,
    to_write: usize,
    deduped: usize,
    bytes: u64,
}

/// Plan CHD (`<disk>`) matches as loose files in a machine folder
/// (`<dest_root>/<game>/<name>.chd`) — the MAME on-disk convention — never
/// packed, whatever the set's format. Mirrors loose-ROM planning: one canonical
/// copy per destination, the rest treated as exact-content duplicates (deleted
/// only in move mode, and never when the content is shared with another entry).
///
/// The DAT disk name has no extension; `.chd` is appended here so the
/// destination matches the on-disk file.
/// Whether `path` is a file already placed under a destination library root, so
/// it must never be removed as a "duplicate". A file in the library is the
/// canonical copy for its own game, not a stray staging copy. Without this guard
/// the move-mode dedup would delete one game's placement while deduping another's
/// — notably a merged-set ROM shared across a parent and its clones, which is a
/// single DAT game (so [`compute_shared_content`] does not flag it as shared) yet
/// is legitimately placed at every clone's destination. Staging copies (under
/// `ToSort/`, outside any destination root) are still removed as before.
fn is_in_library(path: &str, default_dest: Option<&str>, dest_root: &str) -> bool {
    let under = |root: &str| {
        let root = root.trim_end_matches('/');
        path == root || path.starts_with(&format!("{root}/"))
    };
    under(dest_root) || default_dest.is_some_and(under)
}

fn plan_disk_matches(
    matches: Vec<MatchedRom>,
    dest_root: &str,
    opts: &PlanOptions,
    shared: &HashSet<String>,
    plan: &mut Plan,
) -> DiskPlanCounts {
    let mut counts = DiskPlanCounts::default();
    let root = dest_root.trim_end_matches('/');

    // Group every held copy by its canonical destination.
    let mut by_dest: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
    for m in matches {
        let dest = format!("{}/{}/{}.chd", root, m.game_name, m.rom_name);
        by_dest.entry(dest).or_default().push(m);
    }

    for (dest, copies) in by_dest {
        // Shared content must never consume its source — copy it into place even
        // in move mode, and skip the redundancy delete.
        let shared_here = copies.iter().any(|m| shared.contains(&m.sha1));

        let at_dest = copies.iter().position(|m| {
            m.archive_path.is_none() && format!("{}/{}", m.source_root, m.source_path) == dest
        });
        let keep = match at_dest {
            Some(i) => {
                counts.already_correct += 1;
                Some(i)
            }
            None => {
                let m = &copies[0];
                counts.bytes += m.size as u64;
                let source = SourceRef {
                    path: format!("{}/{}", m.source_root, m.source_path),
                    archive_path: m.archive_path.clone(),
                    sha1: m.sha1.clone(),
                    entry_name: None,
                };
                if opts.move_files && !shared_here {
                    plan.add_move(source, dest.clone(), m.size as u64);
                } else {
                    plan.add_copy(source, dest.clone(), m.size as u64);
                }
                counts.to_write += 1;
                Some(0)
            }
        };

        // Every other loose copy is an exact-content duplicate of the kept one.
        if opts.move_files && !shared_here {
            for (i, m) in copies.iter().enumerate() {
                if Some(i) == keep || m.archive_path.is_some() {
                    continue;
                }
                let path = format!("{}/{}", m.source_root, m.source_path);
                if path == dest || is_in_library(&path, opts.default_dest.as_deref(), dest_root) {
                    continue;
                }
                plan.add_delete(path);
                counts.deduped += 1;
            }
        }
    }

    let verb = if opts.move_files { "move" } else { "copy" };
    println!(
        "  {} CHD(s) already correct, {} to {}, {} duplicate(s) to delete",
        counts.already_correct, counts.to_write, verb, counts.deduped
    );

    counts
}

/// The effective merge mode for a collection, in precedence order: an explicit
/// per-collection setting, then a per-set rule (a config row keyed on the set —
/// the top segment of the library path), then the library-wide default. Shared
/// by the planner and [`compute_desired_state`] so the two never disagree on
/// which ROMs a game places — a disagreement would let the cleanup mis-identify a
/// canonical archive.
fn effective_merge_mode(
    conn: &Connection,
    opts: &PlanOptions,
    cfg: Option<&db_config::CollectionConfig>,
    hierarchy: &str,
) -> Result<MergeMode> {
    let explicit_merge = cfg.and_then(|c| c.merge_mode.clone());
    let set_merge = match explicit_merge {
        Some(_) => None,
        None => {
            let set = hierarchy.split('/').next().unwrap_or(hierarchy);
            if set != hierarchy {
                db_config::get_collection_config(conn, set)?.and_then(|c| c.merge_mode)
            } else {
                None
            }
        }
    };
    Ok(resolve_merge_mode(
        explicit_merge.as_deref().or(set_merge.as_deref()),
        opts.default_merge_mode,
    ))
}

/// The effective output format for a collection, in the same precedence order as
/// [`effective_merge_mode`]: explicit per-collection, then per-set rule, then the
/// library-wide default. Shared by the planner and [`compute_desired_state`].
fn effective_format(
    conn: &Connection,
    opts: &PlanOptions,
    cfg: Option<&db_config::CollectionConfig>,
    hierarchy: &str,
) -> Result<OutputFormat> {
    let explicit_format = cfg.and_then(|c| c.output_format.clone());
    let set_format = match explicit_format {
        Some(_) => None,
        None => {
            let set = hierarchy.split('/').next().unwrap_or(hierarchy);
            if set != hierarchy {
                db_config::get_collection_config(conn, set)?.and_then(|c| c.output_format)
            } else {
                None
            }
        }
    };
    Ok(resolve_output_format(
        explicit_format.as_deref().or(set_format.as_deref()),
        opts.default_format,
    ))
}

/// The library's desired state, derived from the active DATs exactly as the
/// planner derives placement.
///
/// Used by `clean-superseded` to decide which loose files under the library are
/// safe to remove: a loose file may go only when its content is preserved in the
/// canonical archive the active DAT assigns it to, and the file is not itself a
/// desired placement of any collection. This captures both facts without running
/// (and saving) a whole plan.
pub struct DesiredState {
    /// Content SHA1 → the canonical archive destination paths the active DATs
    /// assign that content to (absolute). Populated only for the SHA1s the caller
    /// passes in `interesting_sha1s`, so the index stays small on a large library.
    pub archive_homes: HashMap<String, HashSet<String>>,
    /// Every canonical destination path the active DATs designate — archive,
    /// loose, or disk (absolute). A file sitting at one of these is itself a
    /// desired-state member and must never be removed.
    pub dest_paths: HashSet<String>,
}

/// Compute the desired state across every active collection in scope.
///
/// Mirrors [`generate_plan_filtered`]'s per-collection resolution (active
/// version, library path, destination root, merge mode, 1G1R filter, output
/// format, matched ROMs) but records *placements* rather than operations:
///
/// - for an archive-format collection, each game's canonical archive
///   `<dest_root>/<game>.<ext>` and — for the content the caller cares about —
///   the archive it belongs in;
/// - for a loose-format collection, each ROM's canonical loose path;
/// - for any `<disk>`, the loose `<dest_root>/<game>/<name>.chd` path.
///
/// Oversized meta-aggregate collections are skipped exactly as the planner skips
/// them — they place nothing real.
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
        let dest_root = match resolve_dest_root(explicit, opts.default_dest.as_deref(), &hierarchy)
        {
            Some(root) => root,
            None => continue,
        };

        // A meta-aggregate places nothing real, so it contributes no desired
        // state — skip it on the same cap the planner uses.
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
        // Mirror the planner's 1G1R filter, so a variant it would not place is
        // not recorded as a desired placement here either.
        let matches = match cfg.as_ref().and_then(|c| c.extra_config.as_ref()) {
            Some(extra) if extra.one_g_one_r => {
                apply_one_g_one_r_filter(&matches, &extra.to_filter_preferences())
            }
            _ => matches,
        };
        let format = effective_format(conn, opts, cfg.as_ref(), &hierarchy)?;
        let root = dest_root.trim_end_matches('/').to_string();

        // CHDs are placed loose as `<game>/<name>.chd`, whatever the format.
        let (disk_matches, rom_matches): (Vec<MatchedRom>, Vec<MatchedRom>) =
            matches.into_iter().partition(|m| m.is_disk);
        for m in &disk_matches {
            state
                .dest_paths
                .insert(format!("{}/{}/{}.chd", root, m.game_name, m.rom_name));
        }

        match archive_format_tag(format) {
            Some(tag) => {
                // One canonical archive per game; record it and, for the content
                // the caller cares about, which archive it belongs in.
                let ext = archive_extension(tag);
                let mut by_game: BTreeMap<&str, Vec<&MatchedRom>> = BTreeMap::new();
                for m in &rom_matches {
                    by_game.entry(m.game_name.as_str()).or_default().push(m);
                }
                for (game_name, gmatches) in by_game {
                    let dest = format!("{}/{}.{}", root, game_name, ext);
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
                // Loose layout: a single-ROM game is placed flat, a multi-ROM game
                // in its own folder — so count distinct ROMs per game first.
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
                        &root,
                        &m.game_name,
                        &m.rom_name,
                        multi,
                    ));
                }
            }
        }
    }

    Ok(state)
}

/// Compute state hash for plan validation
pub fn compute_state_hash(conn: &Connection) -> Result<String> {
    let mut hasher = Sha256::new();

    // 1. Active version IDs (sorted)
    let mut active_ids: Vec<i64> = Vec::new();
    let colls = collections::list_collections(conn)?;
    for coll in &colls {
        if let Some(ver) = collections::get_active_version(conn, coll.id)? {
            active_ids.push(ver.id);
        }
    }
    active_ids.sort();
    for id in &active_ids {
        hasher.update(id.to_le_bytes());
    }

    // 2. File catalog fingerprint (row count + max last_seen)
    let (file_count, max_last_seen): (i64, Option<String>) = conn.query_row(
        "SELECT COUNT(*), MAX(last_seen) FROM file_locations",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    hasher.update(file_count.to_le_bytes());
    if let Some(ts) = max_last_seen {
        hasher.update(ts.as_bytes());
    }

    // 3. Destination config hash
    let configs = db_config::list_all_configs(conn)?;
    for cfg in &configs {
        hasher.update(cfg.path_pattern.as_bytes());
        if let Some(ref dest) = cfg.dest_path {
            hasher.update(dest.as_bytes());
        }
    }

    let result = hasher.finalize();
    Ok(crate::util::hex_lower(result))
}

/// Simple glob pattern matching (case-insensitive)
///
/// Supports:
/// - `*` matches any sequence of characters (including empty)
/// - `?` matches exactly one character
fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_impl(
        pattern.to_lowercase().as_bytes(),
        text.to_lowercase().as_bytes(),
    )
}

fn glob_match_impl(pattern: &[u8], text: &[u8]) -> bool {
    let mut p = 0;
    let mut t = 0;
    let mut star_p = None;
    let mut star_t = 0;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            // Match single character or ?
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            // Match * - remember position for backtracking
            star_p = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(sp) = star_p {
            // Backtrack: * matches one more character
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    // Check remaining pattern is all *
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

/// Count missing ROMs (ROMs in DAT but not in file catalog)
pub fn count_missing_roms(conn: &Connection, version_id: i64) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?
           AND r.status != 'nodump'
           AND (r.sha1 IS NOT NULL OR r.crc32 IS NOT NULL)
           AND NOT EXISTS (
               SELECT 1 FROM files f
               WHERE (r.sha1 IS NOT NULL AND (f.sha1 = r.sha1 OR f.sha1_no_header = r.sha1))
                  OR (r.sha1 IS NULL AND f.crc32 = r.crc32 AND f.size = r.size)
           )",
        [version_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Apply 1G1R filtering to a list of matched ROMs
///
/// Groups ROMs by their base title (extracted from game_name) and selects
/// the preferred variant based on region priority and dump quality.
fn apply_one_g_one_r_filter(
    matches: &[MatchedRom],
    prefs: &crate::filter::FilterPreferences,
) -> Vec<MatchedRom> {
    // Group matches by parsed title
    let mut groups: HashMap<String, Vec<&MatchedRom>> = HashMap::new();

    for m in matches {
        let parsed = parse_game_name(&m.game_name);
        groups.entry(parsed.title).or_default().push(m);
    }

    // Select best from each group
    let mut result = Vec::new();

    for (_title, group) in groups {
        if group.len() == 1 {
            // Only one variant, keep it (if not excluded)
            let m = group[0];
            let parsed = parse_game_name(&m.game_name);
            if !prefs.should_exclude(&parsed) {
                result.push(m.clone());
            }
        } else {
            // Multiple variants - select the preferred one
            let candidates: Vec<_> = group
                .iter()
                .map(|m| RomCandidate::new(&m.game_name))
                .collect();

            if let Some(preferred_name) = select_preferred(&candidates, prefs) {
                // Find and clone the matching ROM
                if let Some(m) = group.iter().find(|m| m.game_name == preferred_name) {
                    result.push((*m).clone());
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::plan::OperationKind;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_compute_state_hash_empty() {
        let db = setup_db();
        let conn = db.conn();

        let hash = compute_state_hash(conn).unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 hex = 64 chars
    }

    #[test]
    fn test_compute_state_hash_deterministic() {
        let db = setup_db();
        let conn = db.conn();

        let hash1 = compute_state_hash(conn).unwrap();
        let hash2 = compute_state_hash(conn).unwrap();
        assert_eq!(hash1, hash2);
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
            build_dest_path("/roms/nes", "Super Mario Bros", "mario.nes", false),
            "/roms/nes/mario.nes"
        );
        // A trailing slash on the root is normalised away.
        assert_eq!(
            build_dest_path("/roms/nes/", "Game", "game.rom", false),
            "/roms/nes/game.rom"
        );
    }

    #[test]
    fn test_build_dest_path_multi_rom_gets_game_folder() {
        assert_eq!(
            build_dest_path("/roms/nes", "Multi Disk Game", "disk1.img", true),
            "/roms/nes/Multi Disk Game/disk1.img"
        );
        assert_eq!(
            build_dest_path("/roms/nes", "Multi Disk Game", "disk2.img", true),
            "/roms/nes/Multi Disk Game/disk2.img"
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
    fn resolve_dest_root_prefers_explicit_path() {
        // An explicit per-collection dest_path wins and is used verbatim,
        // ignoring both the default and the hierarchy.
        assert_eq!(
            resolve_dest_root(Some("/explicit/here"), Some("/lib"), "Acorn/BBC"),
            Some("/explicit/here".to_string())
        );
    }

    #[test]
    fn is_in_library_protects_destination_files_not_staging() {
        let dd = Some("/lib/ROMs");
        // Any file under the library root is a placement — protected, even one
        // belonging to a different collection than the one being planned.
        assert!(is_in_library(
            "/lib/ROMs/MAME/g/r.bin",
            dd,
            "/lib/ROMs/FBNeo"
        ));
        assert!(is_in_library(
            "/lib/ROMs/FBNeo/g/r.bin",
            dd,
            "/lib/ROMs/FBNeo"
        ));
        // A staging copy outside every destination root is still removable.
        assert!(!is_in_library(
            "/Volumes/ToSort/x.zip",
            dd,
            "/lib/ROMs/FBNeo"
        ));
        // With no library-wide default, the collection's own dest_root still
        // protects its placements.
        assert!(is_in_library(
            "/lib/ROMs/FBNeo/g/r.bin",
            None,
            "/lib/ROMs/FBNeo"
        ));
        // A sibling path that merely shares a prefix is not "under" the root.
        assert!(!is_in_library("/lib/ROMs2/x", dd, "/lib/ROMs/FBNeo"));
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
            "INSERT INTO sources (id, path, case_sensitive) VALUES
                (1, '/lib/ROMs/SET/Sys', 0),
                (2, '/lib/ROMs/SET/Sys/clone', 0),
                (3, '/lib/ToSort/SET', 0)",
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
                move_files: true,
                ..Default::default()
            },
        )
        .unwrap();

        // Only the ToSort stray is deleted; both library copies are left in place.
        let deleted: Vec<_> = plan
            .operations
            .iter()
            .filter_map(|op| match &op.kind {
                OperationKind::Delete { path } => Some(path.clone()),
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
            resolve_dest_root(None, Some("/Volumes/Data"), "TOSEC-PIX/Acorn/BBC"),
            Some("/Volumes/Data/TOSEC-PIX/Acorn/BBC".to_string())
        );
        // A trailing slash on the default base is normalised away.
        assert_eq!(
            resolve_dest_root(None, Some("/Volumes/Data/"), "TOSEC/Sinclair"),
            Some("/Volumes/Data/TOSEC/Sinclair".to_string())
        );
    }

    #[test]
    fn resolve_dest_root_is_none_without_explicit_or_default() {
        // Neither an explicit path nor a default: no destination, caller skips.
        assert_eq!(resolve_dest_root(None, None, "Acorn/BBC"), None);
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
            "INSERT INTO sources (id, path, case_sensitive) VALUES
                (101, '/lib/ROMs/SET/Sys', 0), (102, '/lib/ToSort/SET', 0)",
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
                move_files: true,
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
                OperationKind::Delete { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deleted, vec!["/lib/ToSort/SET/Sys/game.rom".to_string()]);
    }

    #[test]
    fn loose_duplicate_left_untouched_in_copy_mode() {
        let db = setup_db();
        let conn = db.conn();
        setup_dup_fixture(conn, false);

        // Copy mode must not remove source files: the duplicate is left in place.
        let plan = generate_plan_filtered(
            conn,
            &PlanOptions {
                default_dest: Some("/lib/ROMs".to_string()),
                default_format: OutputFormat::Loose,
                move_files: false,
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
                move_files: true,
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
                OperationKind::Delete { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deleted, vec!["/lib/ToSort/SET/Sys/Game.zip".to_string()]);
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
            "INSERT INTO sources (id, path, case_sensitive) VALUES (200, '/lib/ToSort/SET', 0)",
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
                move_files: true,
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
                move_files: false,
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
            "INSERT INTO sources (id, path, case_sensitive) VALUES (201, '/lib/ToSort/SET', 0)",
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
                move_files: true,
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
            "INSERT INTO sources (id, path, case_sensitive) VALUES (210, '/lib/ToSort/SET', 0)",
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
                move_files: true,
                ..Default::default()
            },
        )
        .unwrap();

        // The shared container is repacked per game (extracting each game's own
        // entry), never relocated whole (which would strand the other game) and
        // never deleted (the other game still needs it).
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
        assert_eq!(
            plan.summary.delete_count, 0,
            "the shared container is never deleted"
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
                move_files: false,
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
            move_files: true,
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
            "INSERT INTO sources (id, path, case_sensitive) VALUES (102, '/lib/ToSort/SET', 0)",
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
                move_files: true,
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
            "INSERT INTO sources (id, path, case_sensitive) VALUES (102, '/lib/ToSort/SET', 0)",
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
                move_files: true,
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
    fn is_relocatable_archive_requires_matching_archive_format() {
        let archived = |path: &str| MatchedRom {
            collection: "C".into(),
            game_name: "G".into(),
            rom_name: "r".into(),
            sha1: "AAA".into(),
            size: 1,
            source_root: "/s".into(),
            source_path: path.into(),
            archive_path: Some("r".into()),
            is_disk: false,
        };
        let loose = |path: &str| MatchedRom {
            archive_path: None,
            ..archived(path)
        };
        // A real .zip whose entries are archived → relocatable.
        assert!(is_relocatable_archive(
            &[archived("Game.zip")],
            "/s/Game.zip",
            "zip"
        ));
        // A loose ROM (no archive_path) → must be repacked.
        assert!(!is_relocatable_archive(
            &[loose("game.tap")],
            "/s/game.tap",
            "zip"
        ));
        // An archive in a different format (.7z into a zip set) → repack.
        assert!(!is_relocatable_archive(
            &[archived("Game.7z")],
            "/s/Game.7z",
            "zip"
        ));
        // No entries → not relocatable.
        assert!(!is_relocatable_archive(&[], "/s/Game.zip", "zip"));
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
            collection: "Test".to_string(),
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
