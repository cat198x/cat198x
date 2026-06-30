use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::Plan;
use super::archive_game::ArchiveGame;
use super::container_drains::ContainerDrains;
use super::destinations::build_archive_dest_path;
use super::matching::MatchedRom;
use super::placement_planning::PlacementPlanCounts;
use super::source_policy::{dedup_reason, is_in_library, may_delete, may_move};
use crate::db::files::Disposition;

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
    pub(crate) container_drains: &'a mut ContainerDrains,
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

    sinks.container_drains.record_repack_from_container(
        container,
        dest,
        &game.containers[container],
    )
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
