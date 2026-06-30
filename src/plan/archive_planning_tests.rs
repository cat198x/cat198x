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

fn archived_match_at(source_root: &str, source_path: &str, sha1: &str) -> MatchedRom {
    MatchedRom {
        source_root: source_root.into(),
        source_path: source_path.into(),
        sha1: sha1.into(),
        ..archived_match(source_path)
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

fn plan_inputs_with_default_dest<'a>(
    default_dest: Option<&'a str>,
    shared: &'a HashSet<String>,
    shared_containers: &'a HashSet<String>,
    dispositions: &'a HashMap<String, Disposition>,
) -> ArchivePlanInputs<'a> {
    ArchivePlanInputs {
        default_dest,
        ..plan_inputs("zip", "zip", shared, shared_containers, dispositions)
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

#[test]
fn archive_dedup_delete_candidates_skip_protected_containers() {
    let game = ArchiveGame::from_matches(vec![
        archived_match_at("/stage", "candidate.zip", "AAA"),
        archived_match_at("/dest", "G.zip", "AAA"),
        archived_match_at("/stage", "build.zip", "AAA"),
        archived_match_at("/stage", "shared.zip", "AAA"),
        archived_match_at("/library", "Other.zip", "AAA"),
        archived_match_at("/preserve", "outside.zip", "AAA"),
    ]);
    let shared = HashSet::new();
    let shared_containers = HashSet::from(["/stage/shared.zip".to_string()]);
    let dispositions = HashMap::from([
        ("/stage".to_string(), Disposition::Consume),
        ("/preserve".to_string(), Disposition::Preserve),
    ]);
    let inputs =
        plan_inputs_with_default_dest(Some("/library"), &shared, &shared_containers, &dispositions);

    assert_eq!(
        archive_dedup_delete_candidates(&game, "/dest/G.zip", Some("/stage/build.zip"), &inputs),
        vec!["/stage/candidate.zip"]
    );
}

#[test]
fn archive_dedup_delete_candidates_allow_preserve_source_with_same_tree_survivor() {
    let game =
        ArchiveGame::from_matches(vec![archived_match_at("/preserve", "staged/G.zip", "AAA")]);
    let shared = HashSet::new();
    let shared_containers = HashSet::new();
    let dispositions = HashMap::from([("/preserve".to_string(), Disposition::Preserve)]);
    let inputs = ArchivePlanInputs {
        tag: "zip",
        ext: "zip",
        dest_root: "/preserve/library",
        default_dest: None,
        shared: &shared,
        shared_containers: &shared_containers,
        dispositions: &dispositions,
    };

    assert_eq!(
        archive_dedup_delete_candidates(&game, "/preserve/library/G.zip", None, &inputs),
        vec!["/preserve/staged/G.zip"]
    );
}

#[test]
fn can_consume_repack_feeders_requires_non_shared_deletable_sources() {
    let game = ArchiveGame::from_matches(vec![archived_match_at("/stage", "G.zip", "AAA")]);
    let shared = HashSet::new();
    let shared_containers = HashSet::new();
    let dispositions = HashMap::from([("/stage".to_string(), Disposition::Consume)]);
    let inputs = plan_inputs("zip", "zip", &shared, &shared_containers, &dispositions);

    assert!(can_consume_repack_feeders(
        &game,
        Some("/stage/G.zip"),
        "/dest/G.zip",
        false,
        &inputs
    ));
    assert!(!can_consume_repack_feeders(
        &game,
        Some("/stage/G.zip"),
        "/dest/G.zip",
        true,
        &inputs
    ));

    let preserve_game =
        ArchiveGame::from_matches(vec![archived_match_at("/preserve", "G.zip", "AAA")]);
    let preserve_dispositions = HashMap::from([("/preserve".to_string(), Disposition::Preserve)]);
    let preserve_inputs = plan_inputs(
        "zip",
        "zip",
        &shared,
        &shared_containers,
        &preserve_dispositions,
    );
    assert!(!can_consume_repack_feeders(
        &preserve_game,
        Some("/preserve/G.zip"),
        "/dest/G.zip",
        false,
        &preserve_inputs
    ));
}

#[test]
fn can_consume_repack_feeders_allows_preserve_source_with_same_tree_survivor() {
    let game =
        ArchiveGame::from_matches(vec![archived_match_at("/preserve", "staged/G.zip", "AAA")]);
    let shared = HashSet::new();
    let shared_containers = HashSet::new();
    let dispositions = HashMap::from([("/preserve".to_string(), Disposition::Preserve)]);
    let inputs = ArchivePlanInputs {
        tag: "zip",
        ext: "zip",
        dest_root: "/preserve/library",
        default_dest: None,
        shared: &shared,
        shared_containers: &shared_containers,
        dispositions: &dispositions,
    };

    assert!(can_consume_repack_feeders(
        &game,
        Some("/preserve/staged/G.zip"),
        "/preserve/library/G.zip",
        false,
        &inputs
    ));
}

#[test]
fn drainable_repack_container_requires_distinct_archived_deletable_container() {
    let game = ArchiveGame::from_matches(vec![
        archived_match_at("/stage", "G.zip", "AAA"),
        MatchedRom {
            archive_path: None,
            ..archived_match_at("/stage", "loose.rom", "AAA")
        },
        archived_match_at("/preserve", "P.zip", "AAA"),
    ]);
    let shared = HashSet::new();
    let shared_containers = HashSet::new();
    let dispositions = HashMap::from([
        ("/stage".to_string(), Disposition::Consume),
        ("/preserve".to_string(), Disposition::Preserve),
    ]);
    let inputs = plan_inputs("zip", "zip", &shared, &shared_containers, &dispositions);

    assert_eq!(
        drainable_repack_container(&game, "/dest/G.zip", Some("/stage/G.zip"), &inputs),
        Some("/stage/G.zip")
    );
    assert_eq!(
        drainable_repack_container(&game, "/stage/G.zip", Some("/stage/G.zip"), &inputs),
        None
    );
    assert_eq!(
        drainable_repack_container(&game, "/dest/G.zip", Some("/stage/loose.rom"), &inputs),
        None
    );
    assert_eq!(
        drainable_repack_container(&game, "/dest/G.zip", Some("/preserve/P.zip"), &inputs),
        None
    );
}
