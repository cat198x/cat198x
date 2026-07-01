//! DAT node, game, ROM, and completeness operations

mod queries;
mod stats;
mod types;
mod writes;

pub use queries::{
    count_games_and_roms, find_rom_by_sha1, get_game_by_name, get_games_for_node,
    get_games_for_version, get_roms_for_game, get_roms_for_version, get_roms_for_version_grouped,
};
pub use stats::{
    GameRomRequirements, MergeModeStats, RequirementOptions, RomKey, calculate_merge_mode_stats,
    calculate_rom_requirements, calculate_rom_requirements_with_options, present_keys, rom_present,
};
pub use types::{DatGame, DatNode, DatRom, MergeMode};
pub use writes::{
    create_disk, create_game, create_node, create_rom, nest_primary_node_under_name,
    primary_node_path, rename_dat_node,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, collections};
    use rusqlite::{Connection, params};
    use std::collections::HashSet;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    /// Helper to create a collection and version for tests
    fn create_test_collection_version(conn: &Connection) -> (i64, i64) {
        let coll_id = collections::create_collection(conn, "Nintendo - NES", "nointro").unwrap();
        let version_id =
            collections::add_version(conn, coll_id, "20231215", "/path/to.dat", true).unwrap();
        (coll_id, version_id)
    }

    #[test]
    fn test_crc_only_dat_entry_is_required_and_matched_by_crc_size() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        let node_id = create_node(conn, version_id, None, "root", "dat", "root").unwrap();
        let game_id =
            create_game(conn, node_id, "crcgame", None, None, false, false, false).unwrap();
        // A DAT entry with only a CRC32 and size — no SHA1. Previously dropped
        // from requirements entirely, which let a game falsely read "complete".
        create_rom(
            conn,
            game_id,
            "a.rom",
            1024,
            None,
            None,
            Some("DEADBEEF"),
            "good",
            None,
        )
        .unwrap();

        // It is now a requirement, keyed on CRC + size.
        let reqs =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, false).unwrap();
        let game = reqs.iter().find(|r| r.game_name == "crcgame").unwrap();
        assert_eq!(
            game.required_roms,
            vec![RomKey::CrcSize("DEADBEEF".to_string(), 1024)]
        );

        // Not owned yet: counted as required, not as have.
        let stats =
            calculate_merge_mode_stats(conn, version_id, MergeMode::NonMerged, false).unwrap();
        assert_eq!(stats.total_roms, 1);
        assert_eq!(stats.have_roms, 0);
        assert_eq!(stats.complete_games, 0);
        // Byte totals follow the same CRC-only key: required but not yet had.
        // (This is what `stats` reports as GB; the SHA1-only path showed zero.)
        assert_eq!(stats.total_bytes, 1024);
        assert_eq!(stats.have_bytes, 0);

        // A file with a matching CRC + size makes it present (no SHA1 needed).
        crate::db::files::upsert_file(conn, "SHA1_OF_FILE", None, None, Some("DEADBEEF"), 1024)
            .unwrap();
        let stats =
            calculate_merge_mode_stats(conn, version_id, MergeMode::NonMerged, false).unwrap();
        assert_eq!(stats.have_roms, 1);
        assert_eq!(stats.complete_games, 1);
        assert_eq!(stats.have_bytes, 1024);

        // Right CRC but wrong size must NOT match — size guards CRC collisions.
        assert!(!crate::db::files::has_matching_crc_size(conn, "DEADBEEF", 2048).unwrap());
    }

    #[test]
    fn test_create_node() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(
            conn,
            version_id,
            None,
            "Nintendo - NES",
            "root",
            "Nintendo - NES",
        )
        .unwrap();
        assert!(node_id > 0);
    }

    #[test]
    fn test_rename_dat_node_rewrites_flat_path_but_preserves_hierarchy() {
        let db = setup_db();
        let conn = db.conn();

        // Flat add: node path == node name (the header.name fallback). Both the
        // name and the path should be corrected.
        let (_, flat_ver) = create_test_collection_version(conn);
        create_node(
            conn,
            flat_ver,
            None,
            "em Up - [D64]",
            "dat",
            "em Up - [D64]",
        )
        .unwrap();
        rename_dat_node(
            conn,
            flat_ver,
            "Commodore C64 - Games - Shoot'em Up - [D64]",
        )
        .unwrap();
        let (name, path): (String, String) = conn
            .query_row(
                "SELECT name, path FROM dat_nodes WHERE version_id = ?",
                [flat_ver],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Commodore C64 - Games - Shoot'em Up - [D64]");
        assert_eq!(path, "Commodore C64 - Games - Shoot'em Up - [D64]");

        // Hierarchical add: path is a real tree location, not the bare name. The
        // name is corrected; the recorded hierarchy path is left untouched. Use a
        // distinct collection so the fixed test name doesn't collide.
        let tree_coll = collections::create_collection(conn, "Acorn - BBC", "tosec").unwrap();
        let tree_ver =
            collections::add_version(conn, tree_coll, "20240101", "/path/tree.dat", true).unwrap();
        create_node(
            conn,
            tree_ver,
            None,
            "em Up - [D64]",
            "dat",
            "Commodore/C64/Games",
        )
        .unwrap();
        rename_dat_node(
            conn,
            tree_ver,
            "Commodore C64 - Games - Shoot'em Up - [D64]",
        )
        .unwrap();
        let (name, path): (String, String) = conn
            .query_row(
                "SELECT name, path FROM dat_nodes WHERE version_id = ?",
                [tree_ver],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Commodore C64 - Games - Shoot'em Up - [D64]");
        assert_eq!(
            path, "Commodore/C64/Games",
            "hierarchy path must be preserved"
        );
    }

    #[test]
    fn test_create_nested_nodes() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        // Create hierarchy: TOSEC > Commodore > Amiga
        let root = create_node(conn, version_id, None, "TOSEC", "root", "TOSEC").unwrap();
        let manufacturer = create_node(
            conn,
            version_id,
            Some(root),
            "Commodore",
            "manufacturer",
            "TOSEC/Commodore",
        )
        .unwrap();
        let system = create_node(
            conn,
            version_id,
            Some(manufacturer),
            "Amiga",
            "system",
            "TOSEC/Commodore/Amiga",
        )
        .unwrap();

        assert!(root > 0);
        assert!(manufacturer > 0);
        assert!(system > 0);
    }

    #[test]
    fn test_create_game() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();

        let game_id = create_game(
            conn,
            node_id,
            "Super Mario Bros. (World)",
            Some("Super Mario Bros. (World)"),
            None,
            false,
            false,
            false,
        )
        .unwrap();

        assert!(game_id > 0);
    }

    #[test]
    fn test_create_clone_game() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();

        // Parent game
        create_game(
            conn,
            node_id,
            "Super Mario Bros. (World)",
            None,
            None,
            false,
            false,
            false,
        )
        .unwrap();

        // Clone
        let clone_id = create_game(
            conn,
            node_id,
            "Super Mario Bros. (USA)",
            None,
            Some("Super Mario Bros. (World)"),
            false,
            false,
            false,
        )
        .unwrap();

        assert!(clone_id > 0);
    }

    #[test]
    fn nest_primary_node_under_name_appends_the_node_name() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        create_node(conn, version_id, None, "32x", "root", "MAME/Software List").unwrap();

        let new_path = nest_primary_node_under_name(conn, version_id).unwrap();
        assert_eq!(new_path.as_deref(), Some("MAME/Software List/32x"));
        // Reflected in the primary path used for destination resolution.
        assert_eq!(
            primary_node_path(conn, version_id).unwrap().as_deref(),
            Some("MAME/Software List/32x")
        );
    }

    #[test]
    fn nest_primary_node_under_name_is_none_without_a_node() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        assert!(
            nest_primary_node_under_name(conn, version_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_create_game_duplicate_name_is_deduped_not_an_error() {
        // TOSEC ISO DATs contain accidental byte-identical double-listings of a
        // game (e.g. "CPC Games CD, The", "Smickeonn - The Game"). Importing the
        // second listing must not abort on UNIQUE(node_id, name) — the duplicate
        // is skipped and the existing row id returned, so the whole DAT imports.
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        let node_id = create_node(conn, version_id, None, "ISO", "root", "ISO").unwrap();

        let first = create_game(
            conn,
            node_id,
            "CPC Games CD, The (2020-03-20)(ESP Soft)(ES)",
            Some("CPC Games CD, The (2020-03-20)(ESP Soft)(ES)"),
            None,
            false,
            false,
            false,
        )
        .unwrap();
        // The duplicate listing — must not error.
        let second = create_game(
            conn,
            node_id,
            "CPC Games CD, The (2020-03-20)(ESP Soft)(ES)",
            Some("CPC Games CD, The (2020-03-20)(ESP Soft)(ES)"),
            None,
            false,
            false,
            false,
        )
        .unwrap();

        assert_eq!(first, second, "duplicate should return the existing row id");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dat_games WHERE node_id = ? AND name = ?",
                params![node_id, "CPC Games CD, The (2020-03-20)(ESP Soft)(ES)"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "duplicate game name should be stored once");
    }

    #[test]
    fn test_create_bios_game() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        let bios_id = create_game(
            conn,
            node_id,
            "neogeo",
            Some("Neo-Geo BIOS"),
            None,
            true, // is_bios
            false,
            false,
        )
        .unwrap();

        assert!(bios_id > 0);
    }

    #[test]
    fn test_create_rom() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();
        let game_id = create_game(
            conn,
            node_id,
            "Super Mario Bros.",
            None,
            None,
            false,
            false,
            false,
        )
        .unwrap();

        let rom_id = create_rom(
            conn,
            game_id,
            "Super Mario Bros. (World).nes",
            40976,
            Some("FACEE9C577A5262DBE33AC4930BB0B58C8C037F7"),
            Some("811B027EAF99C2DEF7B933C5208636DE"),
            Some("3337EC46"),
            "good",
            None,
        )
        .unwrap();

        assert!(rom_id > 0);
    }

    #[test]
    fn test_create_rom_duplicate_name_is_deduped_not_an_error() {
        // MAME/FBNeo DATs list a shared BIOS/merge ROM twice within a game
        // (same name, size, CRC). Importing it must not abort on the
        // UNIQUE(game_id, name) constraint — the duplicate is skipped and the
        // existing row id returned, so the whole DAT still imports.
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        let node_id = create_node(conn, version_id, None, "MSX", "root", "MSX").unwrap();
        let game_id =
            create_game(conn, node_id, "zoom909k", None, None, false, false, false).unwrap();

        let first = create_rom(
            conn,
            game_id,
            "msx.rom",
            32768,
            None,
            None,
            Some("a317e6b4"),
            "good",
            Some("msx.rom"),
        )
        .unwrap();
        // Same name again (the merge duplicate) — must not error.
        let second = create_rom(
            conn,
            game_id,
            "msx.rom",
            32768,
            None,
            None,
            Some("a317e6b4"),
            "good",
            None,
        )
        .unwrap();

        assert_eq!(first, second, "duplicate should return the existing row id");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dat_roms WHERE game_id = ? AND name = ?",
                params![game_id, "msx.rom"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "duplicate ROM name should be stored once");
    }

    #[test]
    fn test_create_rom_with_merge_tag() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();
        let game_id =
            create_game(conn, node_id, "pacman", None, None, false, false, false).unwrap();

        let rom_id = create_rom(
            conn,
            game_id,
            "pacman.6e",
            4096,
            Some("ABC123"),
            None,
            Some("12345678"),
            "good",
            Some("puckman"), // merge tag for merged sets
        )
        .unwrap();

        assert!(rom_id > 0);
    }

    #[test]
    fn test_get_games_for_node() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();

        create_game(conn, node_id, "Zelda", None, None, false, false, false).unwrap();
        create_game(conn, node_id, "Mario", None, None, false, false, false).unwrap();
        create_game(conn, node_id, "Metroid", None, None, false, false, false).unwrap();

        let games = get_games_for_node(conn, node_id).unwrap();
        assert_eq!(games.len(), 3);

        // Should be sorted by name
        assert_eq!(games[0].name, "Mario");
        assert_eq!(games[1].name, "Metroid");
        assert_eq!(games[2].name, "Zelda");
    }

    #[test]
    fn test_get_roms_for_game() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();
        let game_id =
            create_game(conn, node_id, "pacman", None, None, false, false, false).unwrap();

        create_rom(
            conn,
            game_id,
            "pacman.6e",
            4096,
            Some("SHA1_A"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game_id,
            "pacman.6f",
            4096,
            Some("SHA1_B"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game_id,
            "pacman.6h",
            4096,
            Some("SHA1_C"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        let roms = get_roms_for_game(conn, game_id).unwrap();
        assert_eq!(roms.len(), 3);

        // Should be sorted by name
        assert_eq!(roms[0].name, "pacman.6e");
        assert_eq!(roms[1].name, "pacman.6f");
        assert_eq!(roms[2].name, "pacman.6h");
    }

    #[test]
    fn test_count_games_and_roms() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();

        // 2 games, 3 ROMs total
        let game1 = create_game(conn, node_id, "Game1", None, None, false, false, false).unwrap();
        let game2 = create_game(conn, node_id, "Game2", None, None, false, false, false).unwrap();

        create_rom(
            conn,
            game1,
            "game1.nes",
            1000,
            Some("SHA1"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game2,
            "game2a.nes",
            2000,
            Some("SHA2"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game2,
            "game2b.nes",
            3000,
            Some("SHA3"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        let (games, roms) = count_games_and_roms(conn, version_id).unwrap();
        assert_eq!(games, 2);
        assert_eq!(roms, 3);
    }

    #[test]
    fn test_find_rom_by_sha1() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();
        let game_id = create_game(conn, node_id, "Mario", None, None, false, false, false).unwrap();

        let target_sha1 = "FACEE9C577A5262DBE33AC4930BB0B58C8C037F7";
        create_rom(
            conn,
            game_id,
            "mario.nes",
            40976,
            Some(target_sha1),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        let found = find_rom_by_sha1(conn, version_id, target_sha1).unwrap();
        assert!(found.is_some());

        let rom = found.unwrap();
        assert_eq!(rom.name, "mario.nes");
        assert_eq!(rom.size, 40976);
    }

    #[test]
    fn test_find_rom_by_sha1_not_found() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let found = find_rom_by_sha1(conn, version_id, "NONEXISTENT").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn test_find_rom_wrong_version() {
        let db = setup_db();
        let conn = db.conn();

        // Create two versions
        let coll_id = collections::create_collection(conn, "NES", "nointro").unwrap();
        let version1 = collections::add_version(conn, coll_id, "v1", "/v1.dat", false).unwrap();
        let version2 = collections::add_version(conn, coll_id, "v2", "/v2.dat", true).unwrap();

        // Add ROM only to version1
        let node1 = create_node(conn, version1, None, "NES", "root", "NES").unwrap();
        let game1 = create_game(conn, node1, "Mario", None, None, false, false, false).unwrap();
        let sha1 = "ABC123";
        create_rom(
            conn,
            game1,
            "mario.nes",
            1000,
            Some(sha1),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Should find in version1
        assert!(find_rom_by_sha1(conn, version1, sha1).unwrap().is_some());

        // Should NOT find in version2
        assert!(find_rom_by_sha1(conn, version2, sha1).unwrap().is_none());
    }

    /// Helper to create a MAME-like parent/clone structure for merge mode tests
    fn create_mame_structure(conn: &Connection, version_id: i64) -> (i64, i64) {
        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        // Parent game: pacman
        let parent_id =
            create_game(conn, node_id, "pacman", None, None, false, false, false).unwrap();
        // Parent ROMs
        create_rom(
            conn,
            parent_id,
            "pacman.5e",
            4096,
            Some("SHA1_PACMAN_5E"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            parent_id,
            "pacman.5f",
            4096,
            Some("SHA1_PACMAN_5F"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            parent_id,
            "prom.7f",
            256,
            Some("SHA1_PROM"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Clone game: mspacman (clones from pacman)
        let clone_id = create_game(
            conn,
            node_id,
            "mspacman",
            None,
            Some("pacman"),
            false,
            false,
            false,
        )
        .unwrap();
        // Clone's unique ROMs
        create_rom(
            conn,
            clone_id,
            "mspacman.5e",
            4096,
            Some("SHA1_MSPACMAN_5E"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            clone_id,
            "mspacman.5f",
            4096,
            Some("SHA1_MSPACMAN_5F"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        // Clone's inherited ROM (has merge tag pointing to parent)
        create_rom(
            conn,
            clone_id,
            "prom.7f",
            256,
            Some("SHA1_PROM"),
            None,
            None,
            "good",
            Some("prom.7f"),
        )
        .unwrap();

        (parent_id, clone_id)
    }

    #[test]
    fn test_merge_mode_non_merged_requires_all_roms() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        create_mame_structure(conn, version_id);

        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, false).unwrap();

        // Should have 2 games
        assert_eq!(requirements.len(), 2);

        // Find parent and clone
        let parent = requirements
            .iter()
            .find(|r| r.game_name == "pacman")
            .unwrap();
        let clone = requirements
            .iter()
            .find(|r| r.game_name == "mspacman")
            .unwrap();

        // Parent needs 3 ROMs
        assert_eq!(parent.required_roms.len(), 3);
        assert!(!parent.is_clone);

        // Clone ALSO needs 3 ROMs (including the shared prom.7f)
        assert_eq!(clone.required_roms.len(), 3);
        assert!(clone.is_clone);
        assert!(
            clone
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PROM".to_string()))
        );
    }

    #[test]
    fn test_merge_mode_split_excludes_inherited_roms() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        create_mame_structure(conn, version_id);

        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::Split, false).unwrap();

        // Should have 2 games
        assert_eq!(requirements.len(), 2);

        let parent = requirements
            .iter()
            .find(|r| r.game_name == "pacman")
            .unwrap();
        let clone = requirements
            .iter()
            .find(|r| r.game_name == "mspacman")
            .unwrap();

        // Parent still needs all 3 ROMs
        assert_eq!(parent.required_roms.len(), 3);

        // Clone only needs 2 ROMs (excluding inherited prom.7f with merge_tag)
        assert_eq!(clone.required_roms.len(), 2);
        assert!(
            clone
                .required_roms
                .contains(&RomKey::Sha1("SHA1_MSPACMAN_5E".to_string()))
        );
        assert!(
            clone
                .required_roms
                .contains(&RomKey::Sha1("SHA1_MSPACMAN_5F".to_string()))
        );
        assert!(
            !clone
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PROM".to_string()))
        );
    }

    #[test]
    fn test_merge_mode_merged_only_parents() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        create_mame_structure(conn, version_id);

        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::Merged, false).unwrap();

        // Should only have 1 game (parent only, clone doesn't exist as separate archive)
        assert_eq!(requirements.len(), 1);

        let parent = &requirements[0];
        assert_eq!(parent.game_name, "pacman");

        // Parent needs all ROMs including clone's unique ROMs
        // 3 parent ROMs + 2 unique clone ROMs = 5 (but SHA1_PROM is shared, so still 5)
        assert_eq!(parent.required_roms.len(), 5);
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PACMAN_5E".to_string()))
        );
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PACMAN_5F".to_string()))
        );
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PROM".to_string()))
        );
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_MSPACMAN_5E".to_string()))
        );
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_MSPACMAN_5F".to_string()))
        );
    }

    #[test]
    fn test_merge_mode_excludes_nodump() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();
        let game_id =
            create_game(conn, node_id, "testgame", None, None, false, false, false).unwrap();

        // 2 good ROMs, 1 nodump
        create_rom(
            conn,
            game_id,
            "rom1.bin",
            1000,
            Some("SHA1_ROM1"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game_id,
            "rom2.bin",
            1000,
            Some("SHA1_ROM2"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn, game_id, "pal.bin", 256, None, None, None, "nodump", None,
        )
        .unwrap();

        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, false).unwrap();

        assert_eq!(requirements.len(), 1);
        let req = &requirements[0];

        // Only 2 required ROMs (nodump excluded)
        assert_eq!(req.required_roms.len(), 2);
        assert_eq!(req.nodump_count, 1);
    }

    #[test]
    fn test_merge_mode_excludes_mechanical() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        // Regular game
        let game1 = create_game(conn, node_id, "pacman", None, None, false, false, false).unwrap();
        create_rom(
            conn,
            game1,
            "pacman.bin",
            1000,
            Some("SHA1_PACMAN"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Mechanical game (slot machine)
        let game2 =
            create_game(conn, node_id, "slotmachine", None, None, false, false, true).unwrap();
        create_rom(
            conn,
            game2,
            "slot.bin",
            1000,
            Some("SHA1_SLOT"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // With exclude_mechanical = true
        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, true).unwrap();
        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].game_name, "pacman");

        // With exclude_mechanical = false
        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, false).unwrap();
        assert_eq!(requirements.len(), 2);
    }

    #[test]
    fn test_merge_mode_stats_calculation() {
        use crate::db::files;

        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        create_mame_structure(conn, version_id);

        // Add a source and some files to our inventory
        let source_id = files::add_source(conn, "/roms", false).unwrap();

        // Only add the parent's ROMs to inventory (not the clone's unique ROMs)
        files::upsert_file(conn, "SHA1_PACMAN_5E", None, None, None, 4096).unwrap();
        files::upsert_file(conn, "SHA1_PACMAN_5F", None, None, None, 4096).unwrap();
        files::upsert_file(conn, "SHA1_PROM", None, None, None, 256).unwrap();

        let _ = source_id; // unused, just need files in db

        // Non-merged: need all 6 unique ROMs (3 parent + 3 clone, but SHA1_PROM shared = 5)
        let stats =
            calculate_merge_mode_stats(conn, version_id, MergeMode::NonMerged, false).unwrap();
        assert_eq!(stats.total_games, 2);
        assert_eq!(stats.total_roms, 5); // 5 unique SHA1s
        assert_eq!(stats.have_roms, 3); // We have 3 ROMs
        assert_eq!(stats.complete_games, 1); // Parent is complete
        assert_eq!(stats.partial_games, 1); // Clone has prom but missing unique ROMs

        // Split mode: clone only needs unique ROMs (2)
        let stats = calculate_merge_mode_stats(conn, version_id, MergeMode::Split, false).unwrap();
        assert_eq!(stats.total_games, 2);
        // Total unique required: parent 3 + clone 2 = 5
        assert_eq!(stats.total_roms, 5);
        assert_eq!(stats.have_roms, 3);
        assert_eq!(stats.complete_games, 1); // Parent is complete
        assert_eq!(stats.missing_games, 1); // Clone is missing (0 of its 2 unique)

        // Merged mode: only parent, needs all ROMs
        let stats = calculate_merge_mode_stats(conn, version_id, MergeMode::Merged, false).unwrap();
        assert_eq!(stats.total_games, 1);
        assert_eq!(stats.total_roms, 5); // Parent needs all 5 unique
        assert_eq!(stats.have_roms, 3);
        assert_eq!(stats.partial_games, 1); // Parent is partial (missing clone ROMs)
    }

    #[test]
    fn test_bios_device_tracking_in_stats() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        // Regular game
        let game1 = create_game(conn, node_id, "mslug", None, None, false, false, false).unwrap();
        create_rom(
            conn,
            game1,
            "mslug.bin",
            1000,
            Some("SHA1_MSLUG"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // BIOS set
        let bios = create_game(conn, node_id, "neogeo", None, None, true, false, false).unwrap();
        create_rom(
            conn,
            bios,
            "neogeo.bin",
            2000,
            Some("SHA1_NEOGEO"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Device set
        let device = create_game(conn, node_id, "ymz280b", None, None, false, true, false).unwrap();
        create_rom(
            conn,
            device,
            "ymz.bin",
            500,
            Some("SHA1_YMZ"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        let stats =
            calculate_merge_mode_stats(conn, version_id, MergeMode::NonMerged, false).unwrap();

        assert_eq!(stats.total_games, 3);
        assert_eq!(stats.bios_sets, 1);
        assert_eq!(stats.device_sets, 1);
        // 3 - 1 BIOS - 1 device = 1 regular game
        assert_eq!(stats.total_games - stats.bios_sets - stats.device_sets, 1);
    }

    #[test]
    fn test_exclude_bios_and_devices() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        // Regular game
        let game1 = create_game(conn, node_id, "mslug", None, None, false, false, false).unwrap();
        create_rom(
            conn,
            game1,
            "mslug.bin",
            1000,
            Some("SHA1_MSLUG"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // BIOS set
        let bios = create_game(conn, node_id, "neogeo", None, None, true, false, false).unwrap();
        create_rom(
            conn,
            bios,
            "neogeo.bin",
            2000,
            Some("SHA1_NEOGEO"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Device set
        let device = create_game(conn, node_id, "ymz280b", None, None, false, true, false).unwrap();
        create_rom(
            conn,
            device,
            "ymz.bin",
            500,
            Some("SHA1_YMZ"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Without exclusions: 3 games
        let requirements = calculate_rom_requirements_with_options(
            conn,
            version_id,
            MergeMode::NonMerged,
            &RequirementOptions::default(),
        )
        .unwrap();
        assert_eq!(requirements.len(), 3);

        // With BIOS exclusion: 2 games
        let requirements = calculate_rom_requirements_with_options(
            conn,
            version_id,
            MergeMode::NonMerged,
            &RequirementOptions {
                exclude_bios: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(requirements.len(), 2);
        assert!(!requirements.iter().any(|r| r.game_name == "neogeo"));

        // With device exclusion: 2 games
        let requirements = calculate_rom_requirements_with_options(
            conn,
            version_id,
            MergeMode::NonMerged,
            &RequirementOptions {
                exclude_devices: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(requirements.len(), 2);
        assert!(!requirements.iter().any(|r| r.game_name == "ymz280b"));

        // With both exclusions: 1 game (only regular games)
        let requirements = calculate_rom_requirements_with_options(
            conn,
            version_id,
            MergeMode::NonMerged,
            &RequirementOptions {
                exclude_bios: true,
                exclude_devices: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].game_name, "mslug");
    }

    #[test]
    fn present_keys_matches_rom_present_across_kinds() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        // A headered file (full + headerless hash), an md5-only file, a crc+size file.
        crate::db::files::upsert_file(conn, "FULLSHA", Some("HEADERLESS"), None, None, 100)
            .unwrap();
        crate::db::files::upsert_file(conn, "MD5FILE", None, Some("ABCMD5"), None, 50).unwrap();
        crate::db::files::upsert_file(conn, "CRCFILE", None, None, Some("DEAD"), 200).unwrap();

        let keys: HashSet<RomKey> = [
            RomKey::Sha1("FULLSHA".into()),      // present via full hash
            RomKey::Sha1("HEADERLESS".into()),   // present via headerless hash
            RomKey::Sha1("MISSING".into()),      // absent
            RomKey::Md5("ABCMD5".into()),        // present
            RomKey::Md5("NOPE".into()),          // absent
            RomKey::CrcSize("DEAD".into(), 200), // present
            RomKey::CrcSize("DEAD".into(), 999), // absent — wrong size
            RomKey::CrcSize("BEEF".into(), 200), // absent — wrong crc
        ]
        .into();

        let bulk = present_keys(conn, &keys).unwrap();

        // The batched result equals the per-key oracle, exactly.
        let oracle: HashSet<RomKey> = keys
            .iter()
            .filter(|k| rom_present(conn, k).unwrap())
            .cloned()
            .collect();
        assert_eq!(bulk, oracle);
        assert_eq!(bulk.len(), 4);
        assert!(bulk.contains(&RomKey::Sha1("HEADERLESS".into())));
        assert!(bulk.contains(&RomKey::CrcSize("DEAD".into(), 200)));
    }

    #[test]
    fn present_keys_handles_more_than_one_batch() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        // More keys than the internal batch size, half of them present, to
        // exercise the chunking boundary.
        let mut keys: HashSet<RomKey> = HashSet::new();
        for i in 0..1000 {
            let sha1 = format!("SHA{i:04}");
            if i % 2 == 0 {
                crate::db::files::upsert_file(conn, &sha1, None, None, None, 1).unwrap();
            }
            keys.insert(RomKey::Sha1(sha1));
        }

        let bulk = present_keys(conn, &keys).unwrap();
        assert_eq!(bulk.len(), 500);
        assert!(bulk.contains(&RomKey::Sha1("SHA0000".into())));
        assert!(!bulk.contains(&RomKey::Sha1("SHA0001".into())));
    }
}
