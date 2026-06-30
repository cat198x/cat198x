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

type ContentKey = (String, String);

struct ArchiveGame {
    expected: Vec<ContentKey>,
    expected_keys: Vec<ContentKey>,
    containers: BTreeMap<String, Vec<MatchedRom>>,
    container_keys: HashMap<String, HashSet<ContentKey>>,
    holders: HashMap<ContentKey, Vec<String>>,
    name_size: HashMap<String, u64>,
}

impl ArchiveGame {
    fn from_matches(matches: Vec<MatchedRom>) -> Self {
        let mut expected = Vec::new();
        let mut seen = HashSet::new();
        let mut containers: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
        let mut container_keys: HashMap<String, HashSet<ContentKey>> = HashMap::new();
        let mut holders: HashMap<ContentKey, Vec<String>> = HashMap::new();
        let mut name_size: HashMap<String, u64> = HashMap::new();

        for m in matches {
            if seen.insert((m.rom_name.clone(), m.sha1.clone())) {
                expected.push((m.rom_name.clone(), m.sha1.clone()));
            }

            let container = format!("{}/{}", m.source_root, m.source_path);
            name_size.entry(m.rom_name.clone()).or_insert(m.size as u64);

            let key = content_key(&m.rom_name, &m.sha1);
            let keys = container_keys.entry(container.clone()).or_default();
            if keys.insert(key.clone()) {
                holders.entry(key).or_default().push(container.clone());
            }
            containers.entry(container).or_default().push(m);
        }

        let expected_keys = expected
            .iter()
            .map(|(name, sha1)| content_key(name, sha1))
            .collect();

        Self {
            expected,
            expected_keys,
            containers,
            container_keys,
            holders,
            name_size,
        }
    }

    fn is_complete(&self, path: &str) -> bool {
        self.container_keys
            .get(path)
            .is_some_and(|set| self.expected_keys.iter().all(|key| set.contains(key)))
    }

    fn build_from(&self, dest: &str) -> Option<String> {
        if self.is_complete(dest) {
            return Some(dest.to_string());
        }

        self.expected_keys
            .iter()
            .map(|key| self.holders.get(key).map_or(&[][..], Vec::as_slice))
            .min_by_key(|paths| paths.len())
            .and_then(|candidates| {
                candidates
                    .iter()
                    .find(|path| self.is_complete(path))
                    .cloned()
            })
    }

    fn has_shared_content(&self, shared: &HashSet<String>) -> bool {
        self.expected.iter().any(|(_, sha1)| shared.contains(sha1))
    }

    fn expected_size(&self) -> u64 {
        self.expected
            .iter()
            .filter_map(|(name, _)| self.name_size.get(name).copied())
            .sum()
    }

    fn source_refs_for(&self, game_name: &str, build_from: Option<&str>) -> Result<Vec<SourceRef>> {
        match build_from {
            Some(path) => self.containers[path]
                .iter()
                .map(source_ref_for)
                .collect::<Result<Vec<_>>>()
                .with_context(|| format!("invalid DAT path in {game_name}")),
            None => self
                .containers
                .values()
                .flatten()
                .map(source_ref_for)
                .collect::<Result<Vec<_>>>()
                .with_context(|| format!("invalid DAT path in {game_name}")),
        }
    }

    fn feeder_roots(&self, build_from: Option<&str>) -> Vec<&str> {
        match build_from {
            Some(path) => self.containers[path]
                .iter()
                .map(|m| m.source_root.as_str())
                .collect(),
            None => self
                .containers
                .values()
                .flatten()
                .map(|m| m.source_root.as_str())
                .collect(),
        }
    }
}

fn content_key(name: &str, sha1: &str) -> ContentKey {
    (name.to_string(), sha1.to_ascii_lowercase())
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
    mut sinks: ArchivePlanSinks<'_>,
) -> Result<PlacementPlanCounts> {
    let mut counts = PlacementPlanCounts::default();

    let mut games: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
    for m in matches {
        games.entry(m.game_name.clone()).or_default().push(m);
    }

    for (game_name, gmatches) in games {
        merge_counts(
            &mut counts,
            plan_archive_game(&game_name, gmatches, &inputs, &mut sinks)?,
        );
    }

    Ok(counts)
}

fn plan_archive_game(
    game_name: &str,
    matches: Vec<MatchedRom>,
    inputs: &ArchivePlanInputs<'_>,
    sinks: &mut ArchivePlanSinks<'_>,
) -> Result<PlacementPlanCounts> {
    let mut counts = PlacementPlanCounts::default();
    let dest = build_archive_dest_path(inputs.dest_root, game_name, inputs.ext)?;
    let game = ArchiveGame::from_matches(matches);
    let game_shared = game.has_shared_content(inputs.shared);
    let build_from = game.build_from(&dest);

    if game.is_complete(&dest) && inputs.tag != "torrentzip" {
        counts.already_correct += game.expected.len();
    } else if let Some(src) =
        relocatable_source(build_from.as_deref(), &dest, &game, game_shared, inputs)
    {
        let size: u64 = game.containers[src].iter().map(|m| m.size as u64).sum();
        counts.bytes += size;
        sinks.plan.add_relocate(src.to_string(), dest.clone(), size);
        counts.relocated += 1;
    } else {
        merge_counts(
            &mut counts,
            plan_repack(
                game_name,
                &dest,
                build_from.as_deref(),
                &game,
                game_shared,
                inputs,
                sinks,
            )?,
        );
    }

    if !game_shared {
        add_dedup_deletes(
            &game,
            &dest,
            build_from.as_deref(),
            inputs,
            sinks,
            &mut counts,
        );
    }

    Ok(counts)
}

fn merge_counts(total: &mut PlacementPlanCounts, delta: PlacementPlanCounts) {
    total.already_correct += delta.already_correct;
    total.to_write += delta.to_write;
    total.relocated += delta.relocated;
    total.deduped += delta.deduped;
    total.bytes += delta.bytes;
}

fn relocatable_source<'a>(
    build_from: Option<&'a str>,
    dest: &str,
    game: &ArchiveGame,
    game_shared: bool,
    inputs: &ArchivePlanInputs<'_>,
) -> Option<&'a str> {
    let src = build_from.filter(|src| *src != dest)?;
    let entries = &game.containers[src];
    let source_root = &entries[0].source_root;

    (may_move(inputs.dispositions, source_root, dest)
        && !game_shared
        && !inputs.shared_containers.contains(src)
        && inputs.tag != "torrentzip"
        && is_relocatable_archive(entries, src, inputs.ext))
    .then_some(src)
}

fn plan_repack(
    game_name: &str,
    dest: &str,
    build_from: Option<&str>,
    game: &ArchiveGame,
    game_shared: bool,
    inputs: &ArchivePlanInputs<'_>,
    sinks: &mut ArchivePlanSinks<'_>,
) -> Result<PlacementPlanCounts> {
    let mut counts = PlacementPlanCounts::default();
    let sources = game.source_refs_for(game_name, build_from)?;
    let size = game.expected_size();
    counts.bytes += size;

    let feeders = game.feeder_roots(build_from);
    let consume_feeders = !feeders.is_empty()
        && feeders
            .iter()
            .all(|root| may_delete(inputs.dispositions, root, dest));
    sinks.plan.add_repack(
        sources,
        dest.to_string(),
        inputs.tag.to_string(),
        size,
        consume_feeders && !game_shared,
    );
    counts.to_write += 1;

    record_container_drain(game, dest, build_from, inputs, sinks)?;

    Ok(counts)
}

fn record_container_drain(
    game: &ArchiveGame,
    dest: &str,
    build_from: Option<&str>,
    inputs: &ArchivePlanInputs<'_>,
    sinks: &mut ArchivePlanSinks<'_>,
) -> Result<()> {
    let Some(container) = build_from.filter(|container| {
        *container != dest
            && game
                .containers
                .get(*container)
                .and_then(|entries| entries.first())
                .is_some_and(|m| {
                    m.archive_path.is_some()
                        && may_delete(inputs.dispositions, &m.source_root, dest)
                })
    }) else {
        return Ok(());
    };

    let drain = sinks
        .drain_after_repack
        .entry(container.to_string())
        .or_insert_with(|| ContainerDrain {
            format: container_archive_format(container),
            reason_dest: dest.to_string(),
            entries: Vec::new(),
        });
    for m in &game.containers[container] {
        if let Some(archive_entry) = &m.archive_path {
            validate_relative_path("archive entry name", archive_entry)?;
            validate_relative_path("ROM entry name", &m.rom_name)?;
            drain.entries.push(RebuildEntry {
                dest: dest.to_string(),
                dest_entry: m.rom_name.clone(),
                container_entry: archive_entry.clone(),
                sha1: m.sha1.clone(),
            });
        }
    }

    Ok(())
}

fn add_dedup_deletes(
    game: &ArchiveGame,
    dest: &str,
    build_from: Option<&str>,
    inputs: &ArchivePlanInputs<'_>,
    sinks: &mut ArchivePlanSinks<'_>,
    counts: &mut PlacementPlanCounts,
) {
    for (path, entries) in &game.containers {
        if path == dest
            || build_from == Some(path.as_str())
            || inputs.shared_containers.contains(path)
            || is_in_library(path, inputs.default_dest, inputs.dest_root)
        {
            continue;
        }
        if !may_delete(inputs.dispositions, &entries[0].source_root, dest) {
            continue;
        }
        sinks.plan.add_delete(path.clone(), dedup_reason(dest));
        counts.deduped += 1;
    }
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
