use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::desired_state::DesiredState;
use super::destinations::{build_archive_dest_path, build_dest_path, build_disk_dest_path};
use super::matching::MatchedRom;
use super::rules::{archive_extension, archive_format_tag};
use crate::config::OutputFormat;

pub(crate) fn record_collection_desired_state(
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
