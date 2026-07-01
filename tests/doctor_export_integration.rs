//! Integration tests for doctor and export command workflows.

use std::fs;

use cat198x::cli;

mod support;
use support::dats::{create_matching_dat, create_test_dat};
use support::{TestEnv, create_test_rom};

#[test]
fn doctor_healthy_database() {
    let env = TestEnv::new();
    env.init();

    let dat_path = create_test_dat(env.temp_dir.path(), "Doctor Test");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat import failed");

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("source add failed");

    let result = cli::doctor::run(false, env.data_dir_opt());
    assert!(result.is_ok(), "doctor should succeed on healthy database");
}

#[test]
fn doctor_fix_orphaned_collection() {
    let env = TestEnv::new();
    env.init();

    let dat_path = create_test_dat(env.temp_dir.path(), "Orphan Test");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat import failed");

    let db = env.db();
    db.conn()
        .execute("UPDATE collection_versions SET is_active = 0", [])
        .expect("failed to deactivate versions");
    drop(db);

    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Orphan Test")
        .expect("collection lookup should succeed")
        .expect("collection should exist");
    let active = cat198x::db::collections::get_active_version(db.conn(), coll.id)
        .expect("active version lookup should succeed");
    assert!(active.is_none(), "should have no active version before fix");
    drop(db);

    cli::doctor::run(true, env.data_dir_opt()).expect("doctor --fix should succeed");

    let db = env.db();
    let active = cat198x::db::collections::get_active_version(db.conn(), coll.id)
        .expect("active version lookup should succeed");
    assert!(active.is_some(), "should have an active version after fix");
}

#[test]
fn export_formats() {
    let env = TestEnv::new();
    env.init();

    let dat_path = create_test_dat(env.temp_dir.path(), "Export Test");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat import failed");

    let txt_path = env.temp_dir.path().join("export.txt");
    let result = cli::export::run(
        "Export Test",
        Some(txt_path.clone()),
        Some("txt"),
        false,
        false,
        env.data_dir_opt(),
    );
    assert!(result.is_ok(), "text export should succeed");
    assert!(txt_path.exists(), "text file should be created");

    let txt_content = fs::read_to_string(&txt_path).expect("text export should read");
    assert!(
        txt_content.contains("Export Test"),
        "should contain collection name"
    );
    assert!(txt_content.contains("ROMs:"), "should contain ROM stats");

    let csv_path = env.temp_dir.path().join("export.csv");
    let result = cli::export::run(
        "Export Test",
        Some(csv_path.clone()),
        Some("csv"),
        false,
        false,
        env.data_dir_opt(),
    );
    assert!(result.is_ok(), "csv export should succeed");
    assert!(csv_path.exists(), "csv file should be created");

    let csv_content = fs::read_to_string(&csv_path).expect("csv export should read");
    assert!(
        csv_content.contains("game,rom,sha1"),
        "should contain CSV header"
    );

    let json_path = env.temp_dir.path().join("export.json");
    let result = cli::export::run(
        "Export Test",
        Some(json_path.clone()),
        Some("json"),
        false,
        false,
        env.data_dir_opt(),
    );
    assert!(result.is_ok(), "json export should succeed");
    assert!(json_path.exists(), "json file should be created");

    let json_content = fs::read_to_string(&json_path).expect("json export should read");
    let json: serde_json::Value =
        serde_json::from_str(&json_content).expect("json export should parse");
    assert_eq!(json["collection"], "Export Test");
    assert!(json["roms"].is_array());
}

#[test]
fn export_filters() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let test_content = b"have this rom";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(test_content));
    let dat_path = create_matching_dat(env.temp_dir.path(), "Filter Test", &sha1_hash);

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat import failed");

    create_test_rom(&env.roms_dir, "source.rom", test_content);

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("source add failed");

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let have_path = env.temp_dir.path().join("have.json");
    cli::export::run(
        "Filter Test",
        Some(have_path.clone()),
        Some("json"),
        true,
        false,
        env.data_dir_opt(),
    )
    .expect("have export failed");

    let have_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&have_path).expect("have export should read"))
            .expect("have export should parse");
    let have_roms = have_json["roms"]
        .as_array()
        .expect("have export roms should be an array");
    assert_eq!(have_roms.len(), 1, "should have 1 ROM with --have filter");
    assert!(
        have_roms[0]["have"]
            .as_bool()
            .expect("have flag should be boolean"),
        "ROM should be marked as have"
    );

    let missing_path = env.temp_dir.path().join("missing.json");
    cli::export::run(
        "Filter Test",
        Some(missing_path.clone()),
        Some("json"),
        false,
        true,
        env.data_dir_opt(),
    )
    .expect("missing export failed");

    let missing_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&missing_path).expect("missing export should read"),
    )
    .expect("missing export should parse");
    let missing_roms = missing_json["roms"]
        .as_array()
        .expect("missing export roms should be an array");
    assert_eq!(
        missing_roms.len(),
        0,
        "should have 0 ROMs with --missing filter when all are found"
    );
}
