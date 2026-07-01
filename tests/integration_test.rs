//! Integration tests for Cat198x CLI workflow
//!
//! These tests exercise the full Phase 1 workflow:
//! init → dat add → source add → scan → status

use std::fs;
use std::path::PathBuf;

// Import the library crate
use cat198x::cli;

mod support;
use support::{TestEnv, create_test_rom};

/// Create a sample DAT file for testing
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
    fs::write(&dat_path, content).expect("Failed to write DAT file");
    dat_path
}

// ============================================================================
// Integration Tests
// ============================================================================

#[test]
fn test_full_workflow_init_to_status() {
    let env = TestEnv::new();

    // Step 1: Initialize
    env.init();

    // Verify database exists
    assert!(env.data_dir.join("db.sqlite").exists());
    assert!(env.data_dir.join("config.toml").exists());
    assert!(env.data_dir.join("objects/plans").exists());

    // Step 2: Add a DAT file
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
    .expect("DAT add failed");

    // Verify collection was created
    let db = env.db();
    let conn = db.conn();
    let collections = cat198x::db::collections::list_collections(conn).unwrap();
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].name, "Test Collection");

    // Step 3: Add source directory
    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("Source add failed");

    // Verify source was registered
    let sources = cat198x::db::files::list_sources(conn).unwrap();
    assert_eq!(sources.len(), 1);

    // Step 4: Create some ROM files and scan
    // Create an empty file (matches one of our test DAT entries)
    create_test_rom(&env.roms_dir, "game1.rom", b"");

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("Scan failed");

    // Verify file was indexed
    let file_count = cat198x::db::files::count_files_in_source(conn, sources[0].id).unwrap();
    assert_eq!(file_count, 1);

    // Step 5: Check status
    // This should show 50% complete (1 of 2 ROMs found)
    cli::status::run(None, false, None, env.data_dir_opt()).expect("Status failed");
}

#[test]
fn test_init_is_idempotent() {
    let env = TestEnv::new();

    // Initialize twice
    env.init();
    env.init();

    // Should still work
    assert!(env.data_dir.join("db.sqlite").exists());

    // Database should be openable
    let _db = env.db();
}

#[test]
fn test_init_preserves_existing_config() {
    let env = TestEnv::new();
    env.init();

    // Modify config
    let config_path = env.data_dir.join("config.toml");
    let custom_config = r#"# Custom config
default_output_format = "zip"
default_merge_mode = "merged"
"#;
    fs::write(&config_path, custom_config).unwrap();

    // Re-init
    env.init();

    // Config should be preserved
    let content = fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("# Custom config"));
    assert!(content.contains("\"zip\""));
}

#[test]
fn test_file_hashing_correctness() {
    let env = TestEnv::new();
    env.init();

    // Create file with known content - empty file has well-known hashes
    create_test_rom(&env.roms_dir, "empty.rom", b"");

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, None, env.data_dir_opt()).unwrap();

    let db = env.db();
    let conn = db.conn();

    // Query for the file with known empty hash
    let file =
        cat198x::db::files::get_file_by_sha1(conn, "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709")
            .unwrap();

    assert!(
        file.is_some(),
        "Empty file should be indexed with correct SHA1"
    );
    let file = file.unwrap();
    assert_eq!(
        file.md5,
        Some("D41D8CD98F00B204E9800998ECF8427E".to_string())
    );
    assert_eq!(file.crc32, Some("00000000".to_string()));
    assert_eq!(file.size, 0);
}

/// Test torrent create and verify commands
#[test]
fn test_torrent_create_and_verify() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).unwrap();

    // Create some test files
    fs::write(content_dir.join("file1.bin"), b"test content one").unwrap();
    fs::write(content_dir.join("file2.bin"), b"test content two").unwrap();

    let torrent_path = temp_dir.path().join("test.torrent");

    // Create torrent
    use cat198x::TorrentCommands;
    cli::torrent::run(TorrentCommands::Create {
        path: content_dir.clone(),
        output: Some(torrent_path.clone()),
        piece_size: Some(16384), // 16 KiB minimum
        tracker: vec!["http://tracker.example.com/announce".to_string()],
        comment: Some("Test torrent".to_string()),
        private: false,
    })
    .expect("Torrent creation failed");

    // Verify torrent file was created
    assert!(torrent_path.exists(), "Torrent file should be created");

    // Verify against the content directory
    cli::torrent::run(TorrentCommands::Verify {
        torrent: torrent_path,
        path: Some(temp_dir.path().to_path_buf()),
    })
    .expect("Torrent verification should pass");
}

/// Test header detection during scan
#[test]
fn test_header_detection_ines() {
    use cat198x::scanner::{HeaderFormat, detect_header};

    // Create iNES header: "NES\x1A" + 12 bytes of metadata
    let mut ines_data = vec![0x4E, 0x45, 0x53, 0x1A]; // "NES\x1A"
    ines_data.extend([
        0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);

    let header = detect_header(&ines_data, 32784, "nes");
    assert!(header.is_some(), "Should detect iNES header");

    let h = header.unwrap();
    assert_eq!(h.format, HeaderFormat::INes);
    assert_eq!(h.skip_bytes, 16);
}

#[test]
fn test_header_detection_a78() {
    use cat198x::scanner::{HeaderFormat, detect_header};

    // Create A78 header: version byte + "ATARI7800" + padding
    let mut a78_data = vec![0x01]; // version
    a78_data.extend(b"ATARI7800");
    a78_data.resize(128, 0x00); // Pad to 128 bytes

    let header = detect_header(&a78_data, 32896, "a78");
    assert!(header.is_some(), "Should detect A78 header");

    let h = header.unwrap();
    assert_eq!(h.format, HeaderFormat::A78);
    assert_eq!(h.skip_bytes, 128);
}

#[test]
fn test_no_header_for_plain_rom() {
    use cat198x::scanner::detect_header;

    // Plain ROM data without any header magic
    let rom_data = vec![
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
        0x0F,
    ];

    let header = detect_header(&rom_data, 32768, "bin");
    assert!(header.is_none(), "Should not detect header for plain ROM");
}

/// Test stats command runs without error
#[test]
fn test_stats_command() {
    let env = TestEnv::new();
    env.init();

    // Create and import a DAT
    let dat_path = create_test_dat(env.temp_dir.path(), "Stats Test");

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("DAT import failed");

    // Add source and scan
    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("Source add failed");

    cli::scan::run(None, false, None, env.data_dir_opt()).unwrap();

    // Stats should run without error
    cli::stats::run(None, env.data_dir_opt()).expect("Stats command failed");
}
