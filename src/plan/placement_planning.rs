use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::destinations::{build_dest_path, build_disk_dest_path};
use super::generator::PlanOptions;
use super::matching::MatchedRom;
use super::{Plan, SourceRef};
use crate::db::files::Disposition;

/// Counts produced by planning one placement branch for a collection.
#[derive(Default)]
pub(crate) struct PlacementPlanCounts {
    pub(crate) already_correct: usize,
    pub(crate) to_write: usize,
    pub(crate) deduped: usize,
    pub(crate) bytes: u64,
}

/// Whether `path` is a file already placed under a destination library root, so
/// it must never be removed as a duplicate. A file in the library is the
/// canonical copy for its own game, not a stray staging copy.
pub(crate) fn is_in_library(path: &str, default_dest: Option<&str>, dest_root: &str) -> bool {
    let under = |root: &str| {
        let root = root.trim_end_matches('/');
        path == root || path.starts_with(&format!("{root}/"))
    };
    under(dest_root) || default_dest.is_some_and(under)
}

/// The disposition of the source at `source_root`; unknown roots are treated as
/// `preserve`, the safe default.
fn disposition_of(dispositions: &HashMap<String, Disposition>, source_root: &str) -> Disposition {
    dispositions
        .get(source_root)
        .copied()
        .unwrap_or(Disposition::Preserve)
}

/// Whether `dest` sits at or under `root`.
fn dest_under(root: &str, dest: &str) -> bool {
    let root = root.trim_end_matches('/');
    dest == root || dest.starts_with(&format!("{root}/"))
}

/// May content read from `source_root` be moved to `dest` rather than copied?
pub(crate) fn may_move(
    dispositions: &HashMap<String, Disposition>,
    source_root: &str,
    dest: &str,
) -> bool {
    match disposition_of(dispositions, source_root) {
        Disposition::Consume => true,
        Disposition::Preserve => dest_under(source_root, dest),
    }
}

/// May a file read from `source_root` be deleted as redundant, given a copy of
/// its content survives at `survivor_dest`?
pub(crate) fn may_delete(
    dispositions: &HashMap<String, Disposition>,
    source_root: &str,
    survivor_dest: &str,
) -> bool {
    match disposition_of(dispositions, source_root) {
        Disposition::Consume => true,
        Disposition::Preserve => dest_under(source_root, survivor_dest),
    }
}

/// The reason recorded on a dedup delete: the canonical copy that survives it.
pub(crate) fn dedup_reason(survivor_dest: &str) -> String {
    format!("exact duplicate — kept {survivor_dest}")
}

/// Plan loose ROM matches: one canonical file per destination, with redundant
/// loose copies deleted only when the source disposition permits it.
pub(crate) fn plan_loose_matches(
    matches: Vec<MatchedRom>,
    dest_root: &str,
    default_dest: Option<&str>,
    shared: &HashSet<String>,
    dispositions: &HashMap<String, Disposition>,
    plan: &mut Plan,
) -> Result<PlacementPlanCounts> {
    let mut roms_per_game: HashMap<String, HashSet<String>> = HashMap::new();
    for m in &matches {
        roms_per_game
            .entry(m.game_name.clone())
            .or_default()
            .insert(m.rom_name.clone());
    }

    let mut by_dest: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
    for m in matches {
        let multi_rom = roms_per_game
            .get(&m.game_name)
            .map(|s| s.len())
            .unwrap_or(1)
            > 1;
        let dest = build_dest_path(dest_root, &m.game_name, &m.rom_name, multi_rom)?;
        by_dest.entry(dest).or_default().push(m);
    }

    plan_loose_destinations(by_dest, dest_root, default_dest, shared, dispositions, plan)
}

fn plan_loose_destinations(
    by_dest: BTreeMap<String, Vec<MatchedRom>>,
    dest_root: &str,
    default_dest: Option<&str>,
    shared: &HashSet<String>,
    dispositions: &HashMap<String, Disposition>,
    plan: &mut Plan,
) -> Result<PlacementPlanCounts> {
    let mut counts = PlacementPlanCounts::default();

    for (dest, copies) in by_dest {
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
                if may_move(dispositions, &m.source_root, &dest) && !shared_here {
                    plan.add_move(source, dest.clone(), m.size as u64);
                } else {
                    plan.add_copy(source, dest.clone(), m.size as u64);
                }
                counts.to_write += 1;
                Some(0)
            }
        };

        if !shared_here {
            for (i, m) in copies.iter().enumerate() {
                if Some(i) == keep || m.archive_path.is_some() {
                    continue;
                }
                if !may_delete(dispositions, &m.source_root, &dest) {
                    continue;
                }
                let path = format!("{}/{}", m.source_root, m.source_path);
                if path == dest || is_in_library(&path, default_dest, dest_root) {
                    continue;
                }
                plan.add_delete(path, dedup_reason(&dest));
                counts.deduped += 1;
            }
        }
    }

    Ok(counts)
}

/// Plan CHD (`<disk>`) matches as loose files in a machine folder
/// (`<dest_root>/<game>/<name>.chd`) -- the MAME on-disk convention -- never
/// packed, whatever the set's format.
pub(crate) fn plan_disk_matches(
    matches: Vec<MatchedRom>,
    dest_root: &str,
    opts: &PlanOptions,
    shared: &HashSet<String>,
    dispositions: &HashMap<String, Disposition>,
    plan: &mut Plan,
) -> Result<PlacementPlanCounts> {
    let mut by_dest: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
    for m in matches {
        let dest = build_disk_dest_path(dest_root, &m.game_name, &m.rom_name)?;
        by_dest.entry(dest).or_default().push(m);
    }

    let counts = plan_loose_destinations(
        by_dest,
        dest_root,
        opts.default_dest.as_deref(),
        shared,
        dispositions,
        plan,
    )?;

    println!(
        "  {} CHD(s) already correct, {} to place, {} duplicate(s) to delete",
        counts.already_correct, counts.to_write, counts.deduped
    );

    Ok(counts)
}
