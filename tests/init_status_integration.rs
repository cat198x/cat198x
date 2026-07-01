//! Integration tests for initialization and status workflows.

use std::fs;
use std::path::PathBuf;

use cat198x::cli;

mod support;
use support::{TestEnv, create_test_rom};

fn create_test_dat(dir: &std::path::Path, name: &str) -> PathBuf {
    let dat_path = dir.join(format!("{}.dat", name));
    let content = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
  <header>
    <name>{}</name>
    <description>{} (Test)</description>
    <version>20231215</version>
    <author>Test Author</author>
  </header>
  <game name="Test Game 1">
    <description>Test Game 1</description>
    <rom name="game1.rom" size="1024" crc="12345678" md5="D41D8CD98F00B204E9800998ECF8427E" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"/>
  </game>
  <game name="Test Game 2">
    <description>Test Game 2</description>
    <rom name="game2.rom" size="2048" crc="ABCDEF01" md5="098F6BCD4621D373CADE4E832627B4F6" sha1="A94A8FE5CCB19BA61C4C0873D391E987982FBBD3"/>
  </game>
</datafile>"#,
        name, name
    );
    fs::write(&dat_path, content).expect("failed to write DAT file");
    dat_path
}

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
