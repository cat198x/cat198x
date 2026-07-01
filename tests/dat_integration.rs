//! Integration tests for DAT command workflows.

use std::fs;

use cat198x::cli;

mod support;
use support::TestEnv;
use support::dats::{
    create_clrmamepro_dat, create_matching_dat, create_test_dat, create_versioned_dat,
};

#[test]
fn dat_import_creates_correct_structure() {
    let env = TestEnv::new();
    env.init();

    let dat_path = create_test_dat(env.temp_dir.path(), "Nintendo - NES");

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat add failed");

    let db = env.db();
    let conn = db.conn();

    let coll = cat198x::db::collections::get_collection_by_name(conn, "Nintendo - NES")
        .expect("collection lookup should succeed")
        .expect("collection should exist");
    let version = cat198x::db::collections::get_active_version(conn, coll.id)
        .expect("active version lookup should succeed")
        .expect("active version should exist");
    assert_eq!(version.version, "20231215");
    assert!(version.is_active);

    let (games, roms) =
        cat198x::db::dats::count_games_and_roms(conn, version.id).expect("counts should load");
    assert_eq!(games, 2);
    assert_eq!(roms, 2);
}

#[test]
fn dat_list_shows_collections() {
    let env = TestEnv::new();
    env.init();

    let dat1 = create_test_dat(env.temp_dir.path(), "Collection A");
    let dat2 = create_test_dat(env.temp_dir.path(), "Collection B");

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat1,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("first dat add failed");
    cli::dat::run(
        DatCommands::Add {
            path: dat2,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("second dat add failed");

    let db = env.db();
    let conn = db.conn();

    let collections =
        cat198x::db::collections::list_collections(conn).expect("collections should list");
    assert_eq!(collections.len(), 2);

    let names: Vec<_> = collections.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"Collection A"));
    assert!(names.contains(&"Collection B"));
}

#[test]
fn dat_activate_version() {
    let env = TestEnv::new();
    env.init();

    let dat_v1_path = env.temp_dir.path().join("test_v1.dat");
    fs::write(
        &dat_v1_path,
        r#"<?xml version="1.0"?>
<datafile>
  <header>
    <name>Test Collection</name>
    <version>v1.0</version>
  </header>
  <game name="Game 1"><rom name="g1.rom" size="100" sha1="0000000000000000000000000000000000000001"/></game>
</datafile>"#,
    )
    .expect("failed to write v1 dat");

    let dat_v2_path = env.temp_dir.path().join("test_v2.dat");
    fs::write(
        &dat_v2_path,
        r#"<?xml version="1.0"?>
<datafile>
  <header>
    <name>Test Collection</name>
    <version>v2.0</version>
  </header>
  <game name="Game 1"><rom name="g1.rom" size="100" sha1="0000000000000000000000000000000000000001"/></game>
  <game name="Game 2"><rom name="g2.rom" size="200" sha1="0000000000000000000000000000000000000002"/></game>
</datafile>"#,
    )
    .expect("failed to write v2 dat");

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_v1_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v1 dat add failed");
    cli::dat::run(
        DatCommands::Add {
            path: dat_v2_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v2 dat add failed");

    let db = env.db();
    let conn = db.conn();

    let coll = cat198x::db::collections::get_collection_by_name(conn, "Test Collection")
        .expect("collection lookup should succeed")
        .expect("collection should exist");
    let active = cat198x::db::collections::get_active_version(conn, coll.id)
        .expect("active version lookup should succeed")
        .expect("active version should exist");
    assert_eq!(active.version, "v2.0");

    cli::dat::run(
        DatCommands::Activate {
            collection: "Test Collection".to_string(),
            version: "v1.0".to_string(),
        },
        env.data_dir_opt(),
    )
    .expect("dat activate failed");

    let active = cat198x::db::collections::get_active_version(conn, coll.id)
        .expect("active version lookup should succeed")
        .expect("active version should exist");
    assert_eq!(active.version, "v1.0");
}

#[test]
fn clrmamepro_dat_import() {
    let env = TestEnv::new();
    env.init();

    let dat_path = create_clrmamepro_dat(env.temp_dir.path(), "CMP Collection");

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("clrmamepro dat import failed");

    let db = env.db();
    let conn = db.conn();

    let coll = cat198x::db::collections::get_collection_by_name(conn, "CMP Collection")
        .expect("collection lookup should succeed")
        .expect("collection should exist");
    let version = cat198x::db::collections::get_active_version(conn, coll.id)
        .expect("active version lookup should succeed")
        .expect("active version should exist");
    assert_eq!(version.version, "20231215");

    let (games, roms) =
        cat198x::db::dats::count_games_and_roms(conn, version.id).expect("counts should load");
    assert_eq!(games, 2);
    assert_eq!(roms, 2);
}

#[test]
fn dat_remove_active_version() {
    let env = TestEnv::new();
    env.init();

    let dat_v1_path = create_versioned_dat(env.temp_dir.path(), "Remove Test", "20240101");
    let dat_v2_path = create_versioned_dat(env.temp_dir.path(), "Remove Test", "20240201");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Remove Test".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v1 dat add failed");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Remove Test".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v2 dat add failed");

    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Remove Test")
        .expect("collection lookup should succeed")
        .expect("collection should exist");
    let versions =
        cat198x::db::collections::list_versions(db.conn(), coll.id).expect("versions should list");
    assert_eq!(versions.len(), 2);
    let active = cat198x::db::collections::get_active_version(db.conn(), coll.id)
        .expect("active version lookup should succeed")
        .expect("active version should exist");
    assert_eq!(active.version, "20240201");
    drop(db);

    cli::dat::run(
        cat198x::DatCommands::Remove {
            target: "Remove Test".to_string(),
            all_versions: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat remove failed");

    let db = env.db();
    let versions =
        cat198x::db::collections::list_versions(db.conn(), coll.id).expect("versions should list");
    assert_eq!(versions.len(), 1, "should have 1 version remaining");
    assert_eq!(versions[0].version, "20240101");

    let active = cat198x::db::collections::get_active_version(db.conn(), coll.id)
        .expect("active version lookup should succeed")
        .expect("active version should exist");
    assert_eq!(active.version, "20240101", "v1 should now be active");
}

#[test]
fn dat_remove_all_versions() {
    let env = TestEnv::new();
    env.init();

    let dat_v1_path = create_versioned_dat(env.temp_dir.path(), "Remove All Test", "20240101");
    let dat_v2_path = create_versioned_dat(env.temp_dir.path(), "Remove All Test", "20240201");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Remove All Test".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v1 dat add failed");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Remove All Test".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v2 dat add failed");

    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Remove All Test")
        .expect("collection lookup should succeed");
    assert!(coll.is_some());
    drop(db);

    cli::dat::run(
        cat198x::DatCommands::Remove {
            target: "Remove All Test".to_string(),
            all_versions: true,
        },
        env.data_dir_opt(),
    )
    .expect("dat remove all failed");

    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Remove All Test")
        .expect("collection lookup should succeed");
    assert!(coll.is_none(), "collection should be removed");
}

#[test]
fn dat_remove_specific_version() {
    let env = TestEnv::new();
    env.init();

    let dat_v1_path = create_versioned_dat(env.temp_dir.path(), "Specific Remove", "20240101");
    let dat_v2_path = create_versioned_dat(env.temp_dir.path(), "Specific Remove", "20240201");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Specific Remove".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v1 dat add failed");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Specific Remove".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v2 dat add failed");

    cli::dat::run(
        cat198x::DatCommands::Remove {
            target: "Specific Remove:20240101".to_string(),
            all_versions: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat remove specific failed");

    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Specific Remove")
        .expect("collection lookup should succeed")
        .expect("collection should exist");
    let versions =
        cat198x::db::collections::list_versions(db.conn(), coll.id).expect("versions should list");
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version, "20240201");
    assert!(versions[0].is_active);
}

#[test]
fn dat_diff_versions() {
    let env = TestEnv::new();
    env.init();

    let dat_v1 = r#"<?xml version="1.0"?>
<!DOCTYPE datafile SYSTEM "datafile.dtd">
<datafile>
    <header>
        <name>Diff Test</name>
        <version>20240101</version>
    </header>
    <game name="Game A">
        <rom name="game_a.rom" size="1024" sha1="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"/>
    </game>
    <game name="Game B">
        <rom name="game_b.rom" size="1024" sha1="BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"/>
    </game>
</datafile>"#;

    let dat_v2 = r#"<?xml version="1.0"?>
<!DOCTYPE datafile SYSTEM "datafile.dtd">
<datafile>
    <header>
        <name>Diff Test</name>
        <version>20240201</version>
    </header>
    <game name="Game A">
        <rom name="game_a.rom" size="1024" sha1="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"/>
    </game>
    <game name="Game C">
        <rom name="game_c.rom" size="1024" sha1="CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"/>
    </game>
    <game name="Game D">
        <rom name="game_d.rom" size="1024" sha1="DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD"/>
    </game>
</datafile>"#;

    let dat_v1_path = env.temp_dir.path().join("diff_v1.dat");
    let dat_v2_path = env.temp_dir.path().join("diff_v2.dat");
    fs::write(&dat_v1_path, dat_v1).expect("failed to write v1 dat");
    fs::write(&dat_v2_path, dat_v2).expect("failed to write v2 dat");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Diff Test".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v1 dat add failed");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Diff Test".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v2 dat add failed");

    let result = cli::dat::run(
        cat198x::DatCommands::Diff {
            collection: "Diff Test".to_string(),
            from: Some("20240101".to_string()),
            to: Some("20240201".to_string()),
        },
        env.data_dir_opt(),
    );

    assert!(result.is_ok(), "dat diff should succeed");

    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Diff Test")
        .expect("collection lookup should succeed")
        .expect("collection should exist");
    let versions =
        cat198x::db::collections::list_versions(db.conn(), coll.id).expect("versions should list");

    let v1 = versions
        .iter()
        .find(|v| v.version == "20240101")
        .expect("v1 should exist");
    let v2 = versions
        .iter()
        .find(|v| v.version == "20240201")
        .expect("v2 should exist");

    let v1_games =
        cat198x::db::dats::get_games_for_version(db.conn(), v1.id).expect("v1 games should load");
    let v2_games =
        cat198x::db::dats::get_games_for_version(db.conn(), v2.id).expect("v2 games should load");

    assert_eq!(v1_games.len(), 2, "v1 should have 2 games");
    assert_eq!(v2_games.len(), 3, "v2 should have 3 games");

    let v1_names: Vec<_> = v1_games.iter().map(|g| g.name.as_str()).collect();
    let v2_names: Vec<_> = v2_games.iter().map(|g| g.name.as_str()).collect();

    assert!(v1_names.contains(&"Game A"));
    assert!(v1_names.contains(&"Game B"));
    assert!(!v1_names.contains(&"Game C"));

    assert!(v2_names.contains(&"Game A"));
    assert!(!v2_names.contains(&"Game B"));
    assert!(v2_names.contains(&"Game C"));
    assert!(v2_names.contains(&"Game D"));
}

#[test]
fn dat_diff_requires_two_versions() {
    let env = TestEnv::new();
    env.init();

    let dat_path = create_versioned_dat(env.temp_dir.path(), "Single Version", "20240101");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: Some("Single Version".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat add failed");

    let result = cli::dat::run(
        cat198x::DatCommands::Diff {
            collection: "Single Version".to_string(),
            from: None,
            to: None,
        },
        env.data_dir_opt(),
    );

    assert!(
        result.is_err(),
        "dat diff should fail with only one version"
    );
}

#[test]
fn dat_versions_lists_all() {
    let env = TestEnv::new();
    env.init();

    let dat_v1_path = create_versioned_dat(env.temp_dir.path(), "Versions Test", "20240101");
    let dat_v2_path = create_versioned_dat(env.temp_dir.path(), "Versions Test", "20240201");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Versions Test".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v1 dat add failed");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Versions Test".to_string()),
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("v2 dat add failed");

    let result = cli::dat::run(
        cat198x::DatCommands::Versions {
            collection: "Versions Test".to_string(),
        },
        env.data_dir_opt(),
    );
    assert!(result.is_ok(), "dat versions should succeed");

    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Versions Test")
        .expect("collection lookup should succeed")
        .expect("collection should exist");
    let versions =
        cat198x::db::collections::list_versions(db.conn(), coll.id).expect("versions should list");
    assert_eq!(versions.len(), 2, "should have 2 versions");
}

#[test]
fn dat_fetch_list() {
    let mame = cat198x::cli::fetch::KNOWN_SOURCES
        .iter()
        .find(|s| s.name == "mame");

    assert!(!cat198x::cli::fetch::KNOWN_SOURCES.is_empty());
    assert!(mame.is_some(), "mame source should be available");
}

#[test]
fn dat_relink_repoints_moved_dat() {
    use cat198x::DatCommands;
    use cat198x::db::collections;

    let env = TestEnv::new();
    env.init();

    let orig_dir = env.temp_dir.path().join("orig");
    fs::create_dir_all(&orig_dir).expect("failed to create original dat dir");
    let dat = create_matching_dat(&orig_dir, "Relink Test", "ABC123");
    cli::dat::run(
        DatCommands::Add {
            path: dat.clone(),
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat add failed");

    let moved_dir = env.temp_dir.path().join("moved/sub");
    fs::create_dir_all(&moved_dir).expect("failed to create moved dat dir");
    let new_dat = moved_dir.join("Relink Test.dat");
    fs::rename(&dat, &new_dat).expect("failed to move dat");

    cli::dat::run(
        DatCommands::Relink {
            dir: env.temp_dir.path().join("moved"),
        },
        env.data_dir_opt(),
    )
    .expect("dat relink failed");

    let db = env.db();
    let conn = db.conn();
    let coll = collections::get_collection_by_name(conn, "Relink Test")
        .expect("collection lookup should succeed")
        .expect("collection exists");
    let version = collections::get_active_version(conn, coll.id)
        .expect("active version lookup should succeed")
        .expect("active version");
    assert!(
        std::path::Path::new(&version.dat_path).is_file(),
        "relinked dat_path should point at an existing file: {}",
        version.dat_path
    );
    assert!(
        version.dat_path.ends_with("Relink Test.dat") && version.dat_path.contains("moved"),
        "dat_path should be the moved file, was: {}",
        version.dat_path
    );
}
