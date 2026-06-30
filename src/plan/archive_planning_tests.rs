use std::collections::{HashMap, HashSet};

use super::*;

fn archived_match(path: &str) -> MatchedRom {
    MatchedRom {
        game_name: "G".into(),
        rom_name: "r".into(),
        sha1: "AAA".into(),
        size: 1,
        source_root: "/s".into(),
        source_path: path.into(),
        archive_path: Some("r".into()),
        is_disk: false,
    }
}

fn plan_inputs<'a>(
    tag: &'a str,
    ext: &'a str,
    shared: &'a HashSet<String>,
    shared_containers: &'a HashSet<String>,
    dispositions: &'a HashMap<String, Disposition>,
) -> ArchivePlanInputs<'a> {
    ArchivePlanInputs {
        tag,
        ext,
        dest_root: "/dest",
        default_dest: None,
        shared,
        shared_containers,
        dispositions,
    }
}

#[test]
fn is_relocatable_archive_requires_matching_archive_format() {
    let loose = |path: &str| MatchedRom {
        archive_path: None,
        ..archived_match(path)
    };
    // A real .zip whose entries are archived -> relocatable.
    assert!(is_relocatable_archive(
        &[archived_match("Game.zip")],
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
        &[archived_match("Game.7z")],
        "/s/Game.7z",
        "zip"
    ));
    // No entries -> not relocatable.
    assert!(!is_relocatable_archive(&[], "/s/Game.zip", "zip"));
}

#[test]
fn archive_game_action_keeps_complete_destination_when_not_torrentzip() {
    let game = ArchiveGame::from_matches(vec![MatchedRom {
        source_root: "/dest".into(),
        source_path: "G.zip".into(),
        ..archived_match("ignored.zip")
    }]);
    let shared = HashSet::new();
    let shared_containers = HashSet::new();
    let dispositions = HashMap::new();
    let inputs = plan_inputs("zip", "zip", &shared, &shared_containers, &dispositions);

    assert_eq!(
        choose_archive_game_action(Some("/dest/G.zip"), "/dest/G.zip", &game, false, &inputs),
        ArchiveGameAction::AlreadyCorrect
    );
}

#[test]
fn archive_game_action_relocates_complete_consumable_archive() {
    let game = ArchiveGame::from_matches(vec![archived_match("G.zip")]);
    let shared = HashSet::new();
    let shared_containers = HashSet::new();
    let dispositions = HashMap::from([("/s".to_string(), Disposition::Consume)]);
    let inputs = plan_inputs("zip", "zip", &shared, &shared_containers, &dispositions);

    assert_eq!(
        choose_archive_game_action(Some("/s/G.zip"), "/dest/G.zip", &game, false, &inputs),
        ArchiveGameAction::Relocate { src: "/s/G.zip" }
    );
}

#[test]
fn archive_game_action_repacks_shared_or_torrentzip_content() {
    let game = ArchiveGame::from_matches(vec![archived_match("G.zip")]);
    let shared = HashSet::new();
    let shared_containers = HashSet::new();
    let dispositions = HashMap::from([("/s".to_string(), Disposition::Consume)]);

    let shared_inputs = plan_inputs("zip", "zip", &shared, &shared_containers, &dispositions);
    assert_eq!(
        choose_archive_game_action(Some("/s/G.zip"), "/dest/G.zip", &game, true, &shared_inputs),
        ArchiveGameAction::Repack
    );

    let torrentzip_inputs = plan_inputs(
        "torrentzip",
        "zip",
        &shared,
        &shared_containers,
        &dispositions,
    );
    assert_eq!(
        choose_archive_game_action(
            Some("/s/G.zip"),
            "/dest/G.zip",
            &game,
            false,
            &torrentzip_inputs
        ),
        ArchiveGameAction::Repack
    );
}
