use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

use super::destinations::{build_archive_dest_path, validate_relative_path};
use super::matching::MatchedRom;
use super::placement_planning::{
    PlacementPlanCounts, dedup_reason, is_in_library, may_delete, may_move,
};
use super::{Plan, RebuildEntry, SourceRef};
use crate::db::files::Disposition;

/// Accumulates, across the games that repack from one staging container, the
/// spec to rebuild that container on rollback.
pub(crate) struct ContainerDrain {
    /// Archive format to rebuild the container in (`zip` or `7z`).
    pub(crate) format: String,
    /// A representative destination, for the human-readable drain reason.
    pub(crate) reason_dest: String,
    /// Where each of the container's entries was repacked to.
    pub(crate) entries: Vec<RebuildEntry>,
}

/// Whether a complete source container can be relocated whole to its
/// destination rather than repacked.
pub(crate) fn is_relocatable_archive(entries: &[MatchedRom], src: &str, ext: &str) -> bool {
    !entries.is_empty()
        && entries.iter().all(|m| m.archive_path.is_some())
        && src
            .rsplit('.')
            .next()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

fn container_archive_format(path: &str) -> String {
    if path.to_ascii_lowercase().ends_with(".7z") {
        "7z".to_string()
    } else {
        "zip".to_string()
    }
}

fn source_ref_for(m: &MatchedRom) -> Result<SourceRef> {
    validate_relative_path("ROM entry name", &m.rom_name)?;
    Ok(SourceRef {
        path: format!("{}/{}", m.source_root, m.source_path),
        archive_path: m.archive_path.clone(),
        sha1: m.sha1.clone(),
        entry_name: Some(m.rom_name.clone()),
    })
}

/// Plan archive-format ROM matches: one archive per game at
/// `<dest_root>/<game>.<ext>`.
pub(crate) fn plan_archive_matches(
    matches: Vec<MatchedRom>,
    tag: &str,
    ext: &str,
    dest_root: &str,
    default_dest: Option<&str>,
    shared: &HashSet<String>,
    shared_containers: &HashSet<String>,
    dispositions: &HashMap<String, Disposition>,
    plan: &mut Plan,
    drain_after_repack: &mut BTreeMap<String, ContainerDrain>,
) -> Result<PlacementPlanCounts> {
    let mut counts = PlacementPlanCounts::default();

    let mut games: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
    for m in matches {
        games.entry(m.game_name.clone()).or_default().push(m);
    }

    for (game_name, gmatches) in games {
        let dest = build_archive_dest_path(dest_root, &game_name, ext)?;

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

        let key = |name: &str, sha1: &str| (name.to_string(), sha1.to_ascii_lowercase());
        let expected_keys: Vec<(String, String)> =
            expected.iter().map(|(n, s)| key(n, s)).collect();
        let mut container_keys: HashMap<&str, HashSet<(String, String)>> = HashMap::new();
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
        let is_complete = |path: &str| {
            container_keys
                .get(path)
                .is_some_and(|set| expected_keys.iter().all(|k| set.contains(k)))
        };

        let game_shared = expected.iter().any(|(_, sha1)| shared.contains(sha1));
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

        let staged_complete: Option<String> = match &build_from {
            Some(p) if *p != dest => Some(p.clone()),
            _ => None,
        };

        if complete_at_dest && tag != "torrentzip" {
            counts.already_correct += expected.len();
        } else if let Some(ref src) = staged_complete
            && may_move(dispositions, &containers[src][0].source_root, &dest)
            && !game_shared
            && !shared_containers.contains(src)
            && tag != "torrentzip"
            && is_relocatable_archive(&containers[src], src, ext)
        {
            let size: u64 = containers[src].iter().map(|m| m.size as u64).sum();
            counts.bytes += size;
            plan.add_relocate(src.clone(), dest.clone(), size);
            counts.relocated += 1;
        } else {
            let sources: Vec<SourceRef> = match &build_from {
                Some(p) => containers[p]
                    .iter()
                    .map(source_ref_for)
                    .collect::<Result<Vec<_>>>()
                    .with_context(|| format!("invalid DAT path in {game_name}"))?,
                None => containers
                    .values()
                    .flatten()
                    .map(source_ref_for)
                    .collect::<Result<Vec<_>>>()
                    .with_context(|| format!("invalid DAT path in {game_name}"))?,
            };
            let size: u64 = expected
                .iter()
                .filter_map(|(name, _)| name_size.get(name.as_str()).copied())
                .sum();
            counts.bytes += size;

            let feeders: Vec<&str> = match &build_from {
                Some(p) => containers[p]
                    .iter()
                    .map(|m| m.source_root.as_str())
                    .collect(),
                None => containers
                    .values()
                    .flatten()
                    .map(|m| m.source_root.as_str())
                    .collect(),
            };
            let consume_feeders =
                !feeders.is_empty() && feeders.iter().all(|r| may_delete(dispositions, r, &dest));
            plan.add_repack(
                sources,
                dest.clone(),
                tag.to_string(),
                size,
                consume_feeders && !game_shared,
            );
            counts.to_write += 1;

            if let Some(container) = build_from.as_ref().filter(|c| {
                **c != dest
                    && containers
                        .get(c.as_str())
                        .and_then(|e| e.first())
                        .is_some_and(|m| {
                            m.archive_path.is_some()
                                && may_delete(dispositions, &m.source_root, &dest)
                        })
            }) {
                let drain = drain_after_repack
                    .entry(container.clone())
                    .or_insert_with(|| ContainerDrain {
                        format: container_archive_format(container),
                        reason_dest: dest.clone(),
                        entries: Vec::new(),
                    });
                for m in &containers[container] {
                    if let Some(archive_entry) = &m.archive_path {
                        validate_relative_path("archive entry name", archive_entry)?;
                        validate_relative_path("ROM entry name", &m.rom_name)?;
                        drain.entries.push(RebuildEntry {
                            dest: dest.clone(),
                            dest_entry: m.rom_name.clone(),
                            container_entry: archive_entry.clone(),
                            sha1: m.sha1.clone(),
                        });
                    }
                }
            }
        }

        if !game_shared {
            for (path, entries) in &containers {
                if *path == dest
                    || build_from.as_deref() == Some(path.as_str())
                    || shared_containers.contains(path)
                    || is_in_library(path, default_dest, dest_root)
                {
                    continue;
                }
                if !may_delete(dispositions, &entries[0].source_root, &dest) {
                    continue;
                }
                plan.add_delete(path.clone(), dedup_reason(&dest));
                counts.deduped += 1;
            }
        }
    }

    Ok(counts)
}
