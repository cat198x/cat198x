use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

use super::destinations::{build_archive_dest_path, validate_relative_path};
use super::matching::MatchedRom;
use super::placement_planning::PlacementPlanCounts;
use super::source_policy::{dedup_reason, is_in_library, may_delete, may_move};
use super::{ContainerRebuild, Plan, RebuildEntry, SourceRef};
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

/// Source containers a repack rebuilt from and that are safe to lose afterwards.
///
/// These are recorded during per-collection archive planning and emitted as
/// deletes after every repack, so apply rebuilds destinations first and the
/// verify-before-delete net proves each entry survives before removing the
/// source container. Keyed by container path so a container feeding several
/// games is drained once.
#[derive(Default)]
pub(crate) struct ContainerDrains {
    pending: BTreeMap<String, ContainerDrain>,
}

impl ContainerDrains {
    pub(crate) fn pending_mut(&mut self) -> &mut BTreeMap<String, ContainerDrain> {
        &mut self.pending
    }

    pub(crate) fn emit_into(self, plan: &mut Plan) {
        emit_container_drains(plan, self.pending);
    }
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

pub(crate) struct ArchivePlanInputs<'a> {
    pub(crate) tag: &'a str,
    pub(crate) ext: &'a str,
    pub(crate) dest_root: &'a str,
    pub(crate) default_dest: Option<&'a str>,
    pub(crate) shared: &'a HashSet<String>,
    pub(crate) shared_containers: &'a HashSet<String>,
    pub(crate) dispositions: &'a HashMap<String, Disposition>,
}

pub(crate) struct ArchivePlanSinks<'a> {
    pub(crate) plan: &'a mut Plan,
    pub(crate) drain_after_repack: &'a mut BTreeMap<String, ContainerDrain>,
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
    inputs: ArchivePlanInputs<'_>,
    sinks: ArchivePlanSinks<'_>,
) -> Result<PlacementPlanCounts> {
    let mut counts = PlacementPlanCounts::default();

    let mut games: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
    for m in matches {
        games.entry(m.game_name.clone()).or_default().push(m);
    }

    for (game_name, gmatches) in games {
        let dest = build_archive_dest_path(inputs.dest_root, &game_name, inputs.ext)?;

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

        let game_shared = expected
            .iter()
            .any(|(_, sha1)| inputs.shared.contains(sha1));
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

        if complete_at_dest && inputs.tag != "torrentzip" {
            counts.already_correct += expected.len();
        } else if let Some(ref src) = staged_complete
            && may_move(inputs.dispositions, &containers[src][0].source_root, &dest)
            && !game_shared
            && !inputs.shared_containers.contains(src)
            && inputs.tag != "torrentzip"
            && is_relocatable_archive(&containers[src], src, inputs.ext)
        {
            let size: u64 = containers[src].iter().map(|m| m.size as u64).sum();
            counts.bytes += size;
            sinks.plan.add_relocate(src.clone(), dest.clone(), size);
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
            let consume_feeders = !feeders.is_empty()
                && feeders
                    .iter()
                    .all(|r| may_delete(inputs.dispositions, r, &dest));
            sinks.plan.add_repack(
                sources,
                dest.clone(),
                inputs.tag.to_string(),
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
                                && may_delete(inputs.dispositions, &m.source_root, &dest)
                        })
            }) {
                let drain = sinks
                    .drain_after_repack
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
                    || inputs.shared_containers.contains(path)
                    || is_in_library(path, inputs.default_dest, inputs.dest_root)
                {
                    continue;
                }
                if !may_delete(inputs.dispositions, &entries[0].source_root, &dest) {
                    continue;
                }
                sinks.plan.add_delete(path.clone(), dedup_reason(&dest));
                counts.deduped += 1;
            }
        }
    }

    Ok(counts)
}

/// Emit final deletes for source containers that repacks rebuilt from. These are
/// emitted after all repacks so apply runs the rebuilds first, then relies on
/// verify-before-delete to prove each entry survives at its destination.
fn emit_container_drains(plan: &mut Plan, drain_after_repack: BTreeMap<String, ContainerDrain>) {
    for (container, drain) in drain_after_repack {
        // One entry per in-container name: a name repeated across feeding games
        // is the same content, so either destination can rebuild it on rollback.
        let mut seen = HashSet::new();
        let entries: Vec<RebuildEntry> = drain
            .entries
            .into_iter()
            .filter(|e| seen.insert(e.container_entry.clone()))
            .collect();
        let reason = format!("consolidated into {}", drain.reason_dest);
        plan.add_container_drain(
            container,
            reason,
            ContainerRebuild {
                format: drain.format,
                entries,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_relocatable_archive_requires_matching_archive_format() {
        let archived = |path: &str| MatchedRom {
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
        // A real .zip whose entries are archived -> relocatable.
        assert!(is_relocatable_archive(
            &[archived("Game.zip")],
            "/s/Game.zip",
            "zip"
        ));
        // A loose ROM (no archive_path) -> must be repacked.
        assert!(!is_relocatable_archive(
            &[loose("game.tap")],
            "/s/game.tap",
            "zip"
        ));
        // An archive in a different format (.7z into a zip set) -> repack.
        assert!(!is_relocatable_archive(
            &[archived("Game.7z")],
            "/s/Game.7z",
            "zip"
        ));
        // No entries -> not relocatable.
        assert!(!is_relocatable_archive(&[], "/s/Game.zip", "zip"));
    }
}
