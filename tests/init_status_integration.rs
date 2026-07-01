//! Integration tests for initialization and status workflows.

use std::fs;

use cat198x::cli;

mod support;
use support::dats::create_test_dat;
use support::{TestEnv, create_test_rom};

#[test]
fn full_workflow_init_to_status() {
    let env = TestEnv::new();
    env.init();

    assert!(env.data_dir.join("db.sqlite").exists());
    assert!(env.data_dir.join("config.toml").exists());
    assert!(env.data_dir.join("objects/plans").exists());

    let dat_path = create_test_dat(env.temp_dir.path(), "Test Collection");

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
    let collections =
        cat198x::db::collections::list_collections(conn).expect("collections should list");
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].name, "Test Collection");

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

    let sources = cat198x::db::files::list_sources(conn).expect("sources should list");
    assert_eq!(sources.len(), 1);

    create_test_rom(&env.roms_dir, "game1.rom", b"");

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let file_count = cat198x::db::files::count_files_in_source(conn, sources[0].id)
        .expect("source file count should load");
    assert_eq!(file_count, 1);

    cli::status::run(None, false, None, env.data_dir_opt()).expect("status failed");
}

#[test]
fn init_is_idempotent() {
    let env = TestEnv::new();

    env.init();
    env.init();

    assert!(env.data_dir.join("db.sqlite").exists());
    let _db = env.db();
}

#[test]
fn init_preserves_existing_config() {
    let env = TestEnv::new();
    env.init();

    let config_path = env.data_dir.join("config.toml");
    let custom_config = r#"# Custom config
default_output_format = "zip"
default_merge_mode = "merged"
"#;
    fs::write(&config_path, custom_config).expect("failed to write custom config");

    env.init();

    let content = fs::read_to_string(&config_path).expect("config should read");
    assert!(content.contains("# Custom config"));
    assert!(content.contains("\"zip\""));
}
