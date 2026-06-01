//! Integration tests for Cat198x CLI workflow
//!
//! These tests exercise the full Phase 1 workflow:
//! init → dat add → source add → scan → status

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

// Import the library crate
use cat198x::cli;
use cat198x::db::Database;

/// Helper to create a test environment with initialized Cat198x
struct TestEnv {
    temp_dir: TempDir,
    data_dir: PathBuf,
    roms_dir: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let data_dir = temp_dir.path().join("data");
        let roms_dir = temp_dir.path().join("roms");

        // Create roms directory
        fs::create_dir_all(&roms_dir).expect("Failed to create roms dir");

        TestEnv {
            temp_dir,
            data_dir,
            roms_dir,
        }
    }

    fn init(&self) {
        cli::init::run(Some(self.data_dir.clone()), None).expect("Init failed");
    }

    fn db(&self) -> Database {
        let db_path = self.data_dir.join("db.sqlite");
        Database::open(&db_path).expect("Failed to open database")
    }

    fn data_dir_opt(&self) -> Option<PathBuf> {
        Some(self.data_dir.clone())
    }
}

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

/// Create a test ROM file with known content
fn create_test_rom(dir: &std::path::Path, name: &str, content: &[u8]) -> PathBuf {
    let rom_path = dir.join(name);
    if let Some(parent) = rom_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&rom_path, content).expect("Failed to write ROM file");
    rom_path
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

    cli::scan::run(None, false, env.data_dir_opt()).expect("Scan failed");

    // Verify file was indexed
    let file_count = cat198x::db::files::count_files_in_source(conn, sources[0].id).unwrap();
    assert_eq!(file_count, 1);

    // Step 5: Check status
    // This should show 50% complete (1 of 2 ROMs found)
    cli::status::run(None, false, None, env.data_dir_opt()).expect("Status failed");
}

#[test]
fn test_dat_import_creates_correct_structure() {
    let env = TestEnv::new();
    env.init();

    let dat_path = create_test_dat(env.temp_dir.path(), "Nintendo - NES");

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    let db = env.db();
    let conn = db.conn();

    // Check collection
    let coll = cat198x::db::collections::get_collection_by_name(conn, "Nintendo - NES")
        .unwrap()
        .expect("Collection not found");

    // Check version
    let version = cat198x::db::collections::get_active_version(conn, coll.id)
        .unwrap()
        .expect("No active version");
    assert_eq!(version.version, "20231215");
    assert!(version.is_active);

    // Check game and ROM counts
    let (games, roms) = cat198x::db::dats::count_games_and_roms(conn, version.id).unwrap();
    assert_eq!(games, 2);
    assert_eq!(roms, 2);
}

#[test]
fn test_source_add_detects_case_sensitivity() {
    let env = TestEnv::new();
    env.init();

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).unwrap();
    assert_eq!(sources.len(), 1);

    // Case sensitivity depends on the filesystem, but it should be detected
    // (macOS default is case-insensitive, Linux is typically case-sensitive)
    let _ = sources[0].case_sensitive; // Just verify the field exists
}

#[test]
fn test_source_add_prevents_duplicates() {
    let env = TestEnv::new();
    env.init();

    use cat198x::SourceCommands;

    // Add source first time
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Add same source again - should succeed but not create duplicate
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).unwrap();
    assert_eq!(sources.len(), 1, "Should not create duplicate source");
}

#[test]
fn test_source_remove() {
    let env = TestEnv::new();
    env.init();

    use cat198x::SourceCommands;

    // Add source
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Remove source
    cli::source::run(
        SourceCommands::Remove {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).unwrap();
    assert_eq!(sources.len(), 0, "Source should be removed");

    // Verify the directory still exists on disk
    assert!(env.roms_dir.exists(), "ROM directory should not be deleted");
}

#[test]
fn test_scan_indexes_loose_files() {
    let env = TestEnv::new();
    env.init();

    // Create some test files
    create_test_rom(&env.roms_dir, "game1.nes", b"NES ROM content");
    create_test_rom(&env.roms_dir, "subdir/game2.nes", b"Another ROM");
    create_test_rom(&env.roms_dir, "game3.sfc", b"SNES ROM");

    // Add source
    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Scan
    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).unwrap();
    let file_count = cat198x::db::files::count_files_in_source(conn, sources[0].id).unwrap();

    assert_eq!(file_count, 3, "Should index all 3 files");
}

#[test]
fn test_scan_updates_last_scanned() {
    let env = TestEnv::new();
    env.init();

    create_test_rom(&env.roms_dir, "test.rom", b"test");

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    let db = env.db();
    let conn = db.conn();

    // Check last_scanned is None before scan
    let sources = cat198x::db::files::list_sources(conn).unwrap();
    assert!(sources[0].last_scanned.is_none());

    // Scan
    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    // Check last_scanned is updated
    let sources = cat198x::db::files::list_sources(conn).unwrap();
    assert!(sources[0].last_scanned.is_some());
}

#[test]
fn test_dat_list_shows_collections() {
    let env = TestEnv::new();
    env.init();

    // Add multiple DATs
    let dat1 = create_test_dat(env.temp_dir.path(), "Collection A");
    let dat2 = create_test_dat(env.temp_dir.path(), "Collection B");

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat1,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::dat::run(
        DatCommands::Add {
            path: dat2,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    let db = env.db();
    let conn = db.conn();

    let collections = cat198x::db::collections::list_collections(conn).unwrap();
    assert_eq!(collections.len(), 2);

    // Verify names (should be sorted)
    let names: Vec<_> = collections.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"Collection A"));
    assert!(names.contains(&"Collection B"));
}

#[test]
fn test_dat_activate_version() {
    let env = TestEnv::new();
    env.init();

    // Create first version
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
    .unwrap();

    // Create second version
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
    .unwrap();

    use cat198x::DatCommands;

    // Import v1
    cli::dat::run(
        DatCommands::Add {
            path: dat_v1_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Import v2 (becomes active)
    cli::dat::run(
        DatCommands::Add {
            path: dat_v2_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    let db = env.db();
    let conn = db.conn();

    // Check v2 is active
    let coll = cat198x::db::collections::get_collection_by_name(conn, "Test Collection")
        .unwrap()
        .unwrap();
    let active = cat198x::db::collections::get_active_version(conn, coll.id)
        .unwrap()
        .unwrap();
    assert_eq!(active.version, "v2.0");

    // Activate v1
    cli::dat::run(
        DatCommands::Activate {
            collection: "Test Collection".to_string(),
            version: "v1.0".to_string(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Check v1 is now active
    let active = cat198x::db::collections::get_active_version(conn, coll.id)
        .unwrap()
        .unwrap();
    assert_eq!(active.version, "v1.0");
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
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

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

/// Create a ClrMamePro format DAT file for testing
fn create_clrmamepro_dat(dir: &std::path::Path, name: &str) -> PathBuf {
    let dat_path = dir.join(format!("{}.dat", name));
    let content = format!(
        r#"clrmamepro (
    name "{}"
    description "{} (Test)"
    version "20231215"
    author "Test Author"
)

game (
    name "Test Game 1"
    description "Test Game 1"
    rom ( name "game1.rom" size 1024 crc 12345678 md5 D41D8CD98F00B204E9800998ECF8427E sha1 DA39A3EE5E6B4B0D3255BFEF95601890AFD80709 )
)

game (
    name "Test Game 2"
    description "Test Game 2"
    rom ( name "game2.rom" size 2048 crc ABCDEF01 md5 098F6BCD4621D373CADE4E832627B4F6 sha1 A94A8FE5CCB19BA61C4C0873D391E987982FBBD3 )
)
"#,
        name, name
    );
    fs::write(&dat_path, content).expect("Failed to write DAT file");
    dat_path
}

#[test]
fn test_clrmamepro_dat_import() {
    let env = TestEnv::new();
    env.init();

    // Create ClrMamePro format DAT
    let dat_path = create_clrmamepro_dat(env.temp_dir.path(), "CMP Collection");

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .expect("ClrMamePro DAT import failed");

    let db = env.db();
    let conn = db.conn();

    // Check collection was created
    let coll = cat198x::db::collections::get_collection_by_name(conn, "CMP Collection")
        .unwrap()
        .expect("Collection not found");

    // Check version
    let version = cat198x::db::collections::get_active_version(conn, coll.id)
        .unwrap()
        .expect("No active version");
    assert_eq!(version.version, "20231215");

    // Check game and ROM counts
    let (games, roms) = cat198x::db::dats::count_games_and_roms(conn, version.id).unwrap();
    assert_eq!(games, 2);
    assert_eq!(roms, 2);
}

#[test]
fn test_plan_generation() {
    let env = TestEnv::new();
    env.init();

    // Create DAT
    let dat_path = create_test_dat(env.temp_dir.path(), "Plan Test");

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Create source with matching file
    // The test DAT uses SHA1 DA39A3EE5E6B4B0D3255BFEF95601890AFD80709 (empty file)
    create_test_rom(&env.roms_dir, "game1.rom", b"");

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    // Create destination directory
    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).unwrap();

    // Generate plan - note: this will print output but we just verify it doesn't panic
    // A real plan would require destination configuration
    cli::plan::run(None, env.data_dir_opt()).unwrap();

    // Check that the plans directory exists
    let plans_dir = env.data_dir.join("objects/plans");

    // Note: Without destination config, plan might be empty, which is ok
    // The important thing is the command runs successfully
    assert!(plans_dir.exists(), "Plans directory should exist");
}

#[test]
fn test_incremental_scan_skips_unchanged() {
    let env = TestEnv::new();
    env.init();

    // Create test file
    create_test_rom(&env.roms_dir, "test.rom", b"original");

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // First scan - should process the file
    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).unwrap();
    let first_scanned = sources[0].last_scanned.clone();
    assert!(first_scanned.is_some());

    // Small delay to ensure timestamp difference
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Second scan - should skip unchanged file
    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    // last_scanned should be updated even if no files changed
    let sources = cat198x::db::files::list_sources(conn).unwrap();
    let second_scanned = sources[0].last_scanned.clone();
    assert!(second_scanned.is_some());

    // File count should still be 1
    let file_count = cat198x::db::files::count_files_in_source(conn, sources[0].id).unwrap();
    assert_eq!(file_count, 1);
}

/// Create a DAT file with known SHA1 hashes that match our test content
fn create_matching_dat(dir: &std::path::Path, name: &str, content_sha1: &str) -> PathBuf {
    let dat_path = dir.join(format!("{}.dat", name));
    let content = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
  <header>
    <name>{}</name>
    <description>{} (Test)</description>
    <version>1.0</version>
    <author>Test</author>
  </header>
  <game name="Test Game">
    <description>Test Game</description>
    <rom name="test.rom" size="5" sha1="{}"/>
  </game>
</datafile>"#,
        name, name, content_sha1
    );
    fs::write(&dat_path, content).expect("Failed to write DAT file");
    dat_path
}

#[test]
fn test_plan_apply_rollback_cycle() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    // Create a test file with known content
    // "hello" has SHA1 = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
    let test_content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(test_content));

    // Create DAT that expects this exact SHA1
    let dat_path = create_matching_dat(env.temp_dir.path(), "Apply Test", &sha1_hash);

    // Import DAT
    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .expect("DAT import failed");

    // Create source ROM file with matching content
    create_test_rom(&env.roms_dir, "source.rom", test_content);

    // Add source directory
    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("Source add failed");

    // Scan to index the file
    cli::scan::run(None, false, env.data_dir_opt()).expect("Scan failed");

    // Create destination directory
    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).expect("Failed to create dest dir");

    // Configure destination path for the collection
    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Apply Test", dest_dir.to_str().unwrap())
        .expect("Failed to set dest_path");
    drop(db);

    // Generate plan
    cli::plan::run(None, env.data_dir_opt()).expect("Plan generation failed");

    // Verify plan was created with operations
    let plans_dir = env.data_dir.join("objects/plans");
    let plan_files: Vec<_> = fs::read_dir(&plans_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(!plan_files.is_empty(), "Plan file should be created");

    // Read and verify plan has operations
    let plan_content = fs::read_to_string(plan_files[0].path()).unwrap();
    let plan: serde_json::Value = serde_json::from_str(&plan_content).unwrap();
    let operations = plan["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 1, "Should have 1 copy operation");

    // Apply the plan
    cli::apply::run(false, true, env.data_dir_opt()).expect("Apply failed");

    // Verify file was copied to destination
    let dest_file = dest_dir.join("test.rom");
    assert!(dest_file.exists(), "File should be copied to destination");
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        test_content,
        "Copied file should have correct content"
    );

    // Verify operation log was created
    let logs_dir = env.data_dir.join("objects/logs");
    let log_files: Vec<_> = fs::read_dir(&logs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(!log_files.is_empty(), "Operation log should be created");

    // Rollback the plan
    cli::apply::run_rollback(false, false, env.data_dir_opt()).expect("Rollback failed");

    // Verify file was deleted from destination
    assert!(!dest_file.exists(), "File should be deleted after rollback");

    // Verify source file still exists (rollback only affects destination)
    let source_file = env.roms_dir.join("source.rom");
    assert!(source_file.exists(), "Source file should remain untouched");
}

/// Create a ZIP archive containing a file with known content
fn create_test_zip(
    dir: &std::path::Path,
    zip_name: &str,
    entry_name: &str,
    content: &[u8],
) -> PathBuf {
    use std::io::Write;

    let zip_path = dir.join(zip_name);
    let file = fs::File::create(&zip_path).expect("Failed to create ZIP file");
    let mut zip = zip::ZipWriter::new(file);

    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file(entry_name, options)
        .expect("start ZIP entry");
    zip.write_all(content).expect("write ZIP entry");
    zip.finish().expect("finish ZIP archive");

    zip_path
}

#[test]
fn test_apply_from_zip_archive() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    // Create test content with known hash
    let test_content = b"archived rom data";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(test_content));

    // Create DAT expecting this hash
    let dat_path = create_matching_dat(env.temp_dir.path(), "Archive Test", &sha1_hash);

    // Import DAT
    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .expect("DAT import failed");

    // Create source as a ZIP archive containing the ROM
    create_test_zip(&env.roms_dir, "games.zip", "inner_rom.bin", test_content);

    // Add source and scan
    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("Source add failed");

    cli::scan::run(None, false, env.data_dir_opt()).expect("Scan failed");

    // Verify the file inside the archive was indexed
    let db = env.db();
    let file = cat198x::db::files::get_file_by_sha1(db.conn(), &sha1_hash).expect("Query failed");
    assert!(file.is_some(), "File from archive should be indexed");
    drop(db);

    // Configure destination
    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).expect("Failed to create dest dir");

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Archive Test", dest_dir.to_str().unwrap())
        .expect("Failed to set dest_path");
    drop(db);

    // Generate and apply plan
    cli::plan::run(None, env.data_dir_opt()).expect("Plan generation failed");
    cli::apply::run(false, true, env.data_dir_opt()).expect("Apply failed");

    // Verify file was extracted to destination
    let dest_file = dest_dir.join("test.rom");
    assert!(
        dest_file.exists(),
        "File should be extracted from archive to destination"
    );
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        test_content,
        "Extracted file should have correct content"
    );

    // Rollback
    cli::apply::run_rollback(false, false, env.data_dir_opt()).expect("Rollback failed");
    assert!(!dest_file.exists(), "File should be deleted after rollback");

    // Original archive should be untouched
    assert!(
        env.roms_dir.join("games.zip").exists(),
        "Source archive should remain"
    );
}

#[test]
fn test_stale_plan_detection() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    // Create initial setup
    let test_content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(test_content));
    let dat_path = create_matching_dat(env.temp_dir.path(), "Stale Test", &sha1_hash);

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    create_test_rom(&env.roms_dir, "test.rom", test_content);

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).unwrap();

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Stale Test", dest_dir.to_str().unwrap())
        .unwrap();
    drop(db);

    // Generate plan
    cli::plan::run(None, env.data_dir_opt()).unwrap();

    // Now modify the state by adding a new file and rescanning
    create_test_rom(&env.roms_dir, "new_file.rom", b"new content");
    cli::scan::run(None, true, env.data_dir_opt()).unwrap(); // Full rescan

    // Apply should detect stale plan and not execute
    // (The apply command prints a message but doesn't error)
    cli::apply::run(false, true, env.data_dir_opt()).unwrap();

    // File should NOT be copied because plan is stale
    let dest_file = dest_dir.join("test.rom");
    assert!(
        !dest_file.exists(),
        "Stale plan should not be applied - file should not exist"
    );
}

/// Create a DAT file with multiple games for testing multi-file scenarios
fn create_multi_rom_dat(dir: &std::path::Path, name: &str, roms: &[(&str, &str)]) -> PathBuf {
    let dat_path = dir.join(format!("{}.dat", name));

    let mut games_xml = String::new();
    for (i, (rom_name, sha1)) in roms.iter().enumerate() {
        games_xml.push_str(&format!(
            r#"  <game name="Game {}">
    <description>Game {}</description>
    <rom name="{}" size="5" sha1="{}"/>
  </game>
"#,
            i + 1,
            i + 1,
            rom_name,
            sha1
        ));
    }

    let content = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
  <header>
    <name>{}</name>
    <description>{}</description>
    <version>1.0</version>
    <author>Test</author>
  </header>
{}
</datafile>"#,
        name, name, games_xml
    );
    fs::write(&dat_path, content).expect("Failed to write DAT file");
    dat_path
}

#[test]
fn test_multi_file_plan_apply() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    // Create multiple ROMs with different content
    let contents: Vec<(&[u8], &str)> = vec![
        (b"rom one", "rom1.rom"),
        (b"rom two", "rom2.rom"),
        (b"rom three", "rom3.rom"),
    ];

    let roms: Vec<(&str, String)> = contents
        .iter()
        .map(|(content, name)| {
            let hash = cat198x::util::hex_upper(sha1::Sha1::digest(*content));
            (*name, hash)
        })
        .collect();

    let roms_for_dat: Vec<(&str, &str)> = roms.iter().map(|(n, h)| (*n, h.as_str())).collect();
    let dat_path = create_multi_rom_dat(env.temp_dir.path(), "Multi Test", &roms_for_dat);

    // Import DAT
    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .expect("DAT import failed");

    // Create source files
    for (content, name) in &contents {
        create_test_rom(&env.roms_dir, name, content);
    }

    // Add source and scan
    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    // Configure destination
    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).unwrap();

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Multi Test", dest_dir.to_str().unwrap())
        .unwrap();
    drop(db);

    // Generate plan
    cli::plan::run(None, env.data_dir_opt()).unwrap();

    // Verify plan has 3 operations
    let plans_dir = env.data_dir.join("objects/plans");
    let plan_files: Vec<_> = fs::read_dir(&plans_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();

    let plan_content = fs::read_to_string(plan_files[0].path()).unwrap();
    let plan: serde_json::Value = serde_json::from_str(&plan_content).unwrap();
    let operations = plan["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 3, "Should have 3 copy operations");

    // Apply plan
    cli::apply::run(false, true, env.data_dir_opt()).unwrap();

    // Verify all 3 files were copied
    for (content, name) in &contents {
        let dest_file = dest_dir.join(name);
        assert!(
            dest_file.exists(),
            "File {} should exist at destination",
            name
        );
        assert_eq!(
            fs::read(&dest_file).unwrap(),
            *content,
            "File {} should have correct content",
            name
        );
    }

    // Rollback all
    cli::apply::run_rollback(false, false, env.data_dir_opt()).unwrap();

    // Verify all files removed
    for (_, name) in &contents {
        let dest_file = dest_dir.join(name);
        assert!(
            !dest_file.exists(),
            "File {} should be deleted after rollback",
            name
        );
    }
}

#[test]
fn test_apply_skips_already_correct_files() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let test_content = b"existing content";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(test_content));
    let dat_path = create_matching_dat(env.temp_dir.path(), "Skip Test", &sha1_hash);

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    create_test_rom(&env.roms_dir, "source.rom", test_content);

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    // Create destination with file already in place
    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).unwrap();

    // Pre-create the destination file with correct content
    let dest_file = dest_dir.join("test.rom");
    fs::write(&dest_file, test_content).unwrap();

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Skip Test", dest_dir.to_str().unwrap()).unwrap();
    drop(db);

    // Generate plan - should detect file is already correct
    cli::plan::run(None, env.data_dir_opt()).unwrap();

    // When no operations are needed, plan file might not be saved.
    // The key verification is that the destination file still has correct content
    // and wasn't overwritten.
    assert!(dest_file.exists(), "Destination file should still exist");
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        test_content,
        "Destination file should still have correct content"
    );

    // Verify plan directory exists (may or may not have files depending on implementation)
    let plans_dir = env.data_dir.join("objects/plans");
    assert!(plans_dir.exists(), "Plans directory should exist");

    // If a plan file was created, verify it shows 0 operations
    let plan_files: Vec<_> = fs::read_dir(&plans_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();

    if !plan_files.is_empty() {
        let plan_content = fs::read_to_string(plan_files[0].path()).unwrap();
        let plan: serde_json::Value = serde_json::from_str(&plan_content).unwrap();
        let operations = plan["operations"].as_array().unwrap();
        assert_eq!(
            operations.len(),
            0,
            "Should have 0 operations - file already correct"
        );
    }
}

/// Helper to create a DAT file with specific version
fn create_versioned_dat(dir: &std::path::Path, name: &str, version: &str) -> PathBuf {
    let dat_path = dir.join(format!("{}_{}.dat", name.replace(' ', "_"), version));
    let content = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
  <header>
    <name>{}</name>
    <description>{} (Test)</description>
    <version>{}</version>
    <author>Test Author</author>
  </header>
  <game name="Test Game 1">
    <description>Test Game 1</description>
    <rom name="game1.rom" size="1024" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"/>
  </game>
</datafile>"#,
        name, name, version
    );
    fs::write(&dat_path, content).expect("Failed to write DAT file");
    dat_path
}

/// Test dat remove removes the active version and activates the next one
#[test]
fn test_dat_remove_active_version() {
    let env = TestEnv::new();
    env.init();

    // Import two versions of the same DAT
    let dat_v1_path = create_versioned_dat(env.temp_dir.path(), "Remove Test", "20240101");
    let dat_v2_path = create_versioned_dat(env.temp_dir.path(), "Remove Test", "20240201");

    // Import both versions
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Remove Test".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Remove Test".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Verify we have 2 versions and v2 is active
    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Remove Test")
        .unwrap()
        .unwrap();
    let versions = cat198x::db::collections::list_versions(db.conn(), coll.id).unwrap();
    assert_eq!(versions.len(), 2);
    let active = cat198x::db::collections::get_active_version(db.conn(), coll.id)
        .unwrap()
        .unwrap();
    assert_eq!(active.version, "20240201");
    drop(db);

    // Remove the active version (v2)
    cli::dat::run(
        cat198x::DatCommands::Remove {
            target: "Remove Test".to_string(),
            all_versions: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Verify v1 is now active and v2 is gone
    let db = env.db();
    let versions = cat198x::db::collections::list_versions(db.conn(), coll.id).unwrap();
    assert_eq!(versions.len(), 1, "Should have 1 version remaining");
    assert_eq!(versions[0].version, "20240101");

    let active = cat198x::db::collections::get_active_version(db.conn(), coll.id)
        .unwrap()
        .unwrap();
    assert_eq!(active.version, "20240101", "v1 should now be active");
}

/// Test dat remove --all-versions removes entire collection
#[test]
fn test_dat_remove_all_versions() {
    let env = TestEnv::new();
    env.init();

    // Import two versions
    let dat_v1_path = create_versioned_dat(env.temp_dir.path(), "Remove All Test", "20240101");
    let dat_v2_path = create_versioned_dat(env.temp_dir.path(), "Remove All Test", "20240201");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Remove All Test".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Remove All Test".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Verify collection exists with 2 versions
    let db = env.db();
    let coll =
        cat198x::db::collections::get_collection_by_name(db.conn(), "Remove All Test").unwrap();
    assert!(coll.is_some());
    drop(db);

    // Remove all versions
    cli::dat::run(
        cat198x::DatCommands::Remove {
            target: "Remove All Test".to_string(),
            all_versions: true,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Verify collection is gone
    let db = env.db();
    let coll =
        cat198x::db::collections::get_collection_by_name(db.conn(), "Remove All Test").unwrap();
    assert!(coll.is_none(), "Collection should be removed");
}

/// Test dat remove with specific version syntax "Collection:version"
#[test]
fn test_dat_remove_specific_version() {
    let env = TestEnv::new();
    env.init();

    // Import two versions
    let dat_v1_path = create_versioned_dat(env.temp_dir.path(), "Specific Remove", "20240101");
    let dat_v2_path = create_versioned_dat(env.temp_dir.path(), "Specific Remove", "20240201");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Specific Remove".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Specific Remove".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Remove v1 specifically (the inactive one)
    cli::dat::run(
        cat198x::DatCommands::Remove {
            target: "Specific Remove:20240101".to_string(),
            all_versions: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Verify v2 is still there and active
    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Specific Remove")
        .unwrap()
        .unwrap();
    let versions = cat198x::db::collections::list_versions(db.conn(), coll.id).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version, "20240201");
    assert!(versions[0].is_active);
}

/// Test dat diff compares two versions
#[test]
fn test_dat_diff_versions() {
    let env = TestEnv::new();
    env.init();

    // Create v1 with 2 games
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

    // Create v2 with 3 games (added Game C, removed Game B)
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
    fs::write(&dat_v1_path, dat_v1).unwrap();
    fs::write(&dat_v2_path, dat_v2).unwrap();

    // Import both versions
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Diff Test".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Diff Test".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Run diff - this shouldn't error
    let result = cli::dat::run(
        cat198x::DatCommands::Diff {
            collection: "Diff Test".to_string(),
            from: Some("20240101".to_string()),
            to: Some("20240201".to_string()),
        },
        env.data_dir_opt(),
    );

    assert!(result.is_ok(), "dat diff should succeed");

    // Verify the data through the DB to confirm diff would show correct changes
    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Diff Test")
        .unwrap()
        .unwrap();
    let versions = cat198x::db::collections::list_versions(db.conn(), coll.id).unwrap();

    let v1 = versions.iter().find(|v| v.version == "20240101").unwrap();
    let v2 = versions.iter().find(|v| v.version == "20240201").unwrap();

    let v1_games = cat198x::db::dats::get_games_for_version(db.conn(), v1.id).unwrap();
    let v2_games = cat198x::db::dats::get_games_for_version(db.conn(), v2.id).unwrap();

    assert_eq!(v1_games.len(), 2, "v1 should have 2 games");
    assert_eq!(v2_games.len(), 3, "v2 should have 3 games");

    // Check specific game names
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

/// Test dat diff with only one version fails gracefully
#[test]
fn test_dat_diff_requires_two_versions() {
    let env = TestEnv::new();
    env.init();

    // Import only one version
    let dat_path = create_versioned_dat(env.temp_dir.path(), "Single Version", "20240101");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: Some("Single Version".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Diff should fail when there's only one version
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

/// Test doctor command runs successfully on healthy database
#[test]
fn test_doctor_healthy_database() {
    let env = TestEnv::new();
    env.init();

    // Import a DAT
    let dat_path = create_test_dat(env.temp_dir.path(), "Doctor Test");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Add a source
    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Run doctor - should succeed on healthy database
    let result = cli::doctor::run(false, env.data_dir_opt());
    assert!(result.is_ok(), "Doctor should succeed on healthy database");
}

/// Test doctor --fix repairs orphaned collections
#[test]
fn test_doctor_fix_orphaned_collection() {
    let env = TestEnv::new();
    env.init();

    // Import a DAT
    let dat_path = create_test_dat(env.temp_dir.path(), "Orphan Test");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Manually deactivate all versions to create an orphaned collection
    let db = env.db();
    db.conn()
        .execute("UPDATE collection_versions SET is_active = 0", [])
        .unwrap();
    drop(db);

    // Verify no active version
    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Orphan Test")
        .unwrap()
        .unwrap();
    let active = cat198x::db::collections::get_active_version(db.conn(), coll.id).unwrap();
    assert!(active.is_none(), "Should have no active version before fix");
    drop(db);

    // Run doctor with --fix
    cli::doctor::run(true, env.data_dir_opt()).unwrap();

    // Verify a version is now active
    let db = env.db();
    let active = cat198x::db::collections::get_active_version(db.conn(), coll.id).unwrap();
    assert!(active.is_some(), "Should have an active version after fix");
}

/// Test dat versions lists all versions of a collection
#[test]
fn test_dat_versions_lists_all() {
    let env = TestEnv::new();
    env.init();

    // Import two versions
    let dat_v1_path = create_versioned_dat(env.temp_dir.path(), "Versions Test", "20240101");
    let dat_v2_path = create_versioned_dat(env.temp_dir.path(), "Versions Test", "20240201");

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v1_path,
            collection: Some("Versions Test".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_v2_path,
            collection: Some("Versions Test".to_string()),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // dat versions should succeed
    let result = cli::dat::run(
        cat198x::DatCommands::Versions {
            collection: "Versions Test".to_string(),
        },
        env.data_dir_opt(),
    );
    assert!(result.is_ok(), "dat versions should succeed");

    // Verify through DB that we have 2 versions
    let db = env.db();
    let coll = cat198x::db::collections::get_collection_by_name(db.conn(), "Versions Test")
        .unwrap()
        .unwrap();
    let versions = cat198x::db::collections::list_versions(db.conn(), coll.id).unwrap();
    assert_eq!(versions.len(), 2, "Should have 2 versions");
}

/// Test export command outputs to different formats
#[test]
fn test_export_formats() {
    let env = TestEnv::new();
    env.init();

    // Import a DAT
    let dat_path = create_test_dat(env.temp_dir.path(), "Export Test");
    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Test text export
    let txt_path = env.temp_dir.path().join("export.txt");
    let result = cli::export::run(
        "Export Test",
        Some(txt_path.clone()),
        Some("txt"),
        false,
        false,
        env.data_dir_opt(),
    );
    assert!(result.is_ok(), "Text export should succeed");
    assert!(txt_path.exists(), "Text file should be created");

    let txt_content = fs::read_to_string(&txt_path).unwrap();
    assert!(
        txt_content.contains("Export Test"),
        "Should contain collection name"
    );
    assert!(txt_content.contains("ROMs:"), "Should contain ROM stats");

    // Test CSV export
    let csv_path = env.temp_dir.path().join("export.csv");
    let result = cli::export::run(
        "Export Test",
        Some(csv_path.clone()),
        Some("csv"),
        false,
        false,
        env.data_dir_opt(),
    );
    assert!(result.is_ok(), "CSV export should succeed");
    assert!(csv_path.exists(), "CSV file should be created");

    let csv_content = fs::read_to_string(&csv_path).unwrap();
    assert!(
        csv_content.contains("game,rom,sha1"),
        "Should contain CSV header"
    );

    // Test JSON export
    let json_path = env.temp_dir.path().join("export.json");
    let result = cli::export::run(
        "Export Test",
        Some(json_path.clone()),
        Some("json"),
        false,
        false,
        env.data_dir_opt(),
    );
    assert!(result.is_ok(), "JSON export should succeed");
    assert!(json_path.exists(), "JSON file should be created");

    let json_content = fs::read_to_string(&json_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&json_content).unwrap();
    assert_eq!(json["collection"], "Export Test");
    assert!(json["roms"].is_array());
}

/// Test export with --have and --missing filters
#[test]
fn test_export_filters() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    // Create test content with known hash
    let test_content = b"have this rom";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(test_content));

    // Create DAT with the matching SHA1
    let dat_path = create_matching_dat(env.temp_dir.path(), "Filter Test", &sha1_hash);

    cli::dat::run(
        cat198x::DatCommands::Add {
            path: dat_path,
            collection: None,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Create source ROM
    create_test_rom(&env.roms_dir, "source.rom", test_content);

    // Add source and scan
    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    // Export with --have filter
    let have_path = env.temp_dir.path().join("have.json");
    cli::export::run(
        "Filter Test",
        Some(have_path.clone()),
        Some("json"),
        true, // have only
        false,
        env.data_dir_opt(),
    )
    .unwrap();

    let have_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&have_path).unwrap()).unwrap();
    let have_roms = have_json["roms"].as_array().unwrap();
    assert_eq!(have_roms.len(), 1, "Should have 1 ROM with --have filter");
    assert!(
        have_roms[0]["have"].as_bool().unwrap(),
        "ROM should be marked as 'have'"
    );

    // Export with --missing filter
    let missing_path = env.temp_dir.path().join("missing.json");
    cli::export::run(
        "Filter Test",
        Some(missing_path.clone()),
        Some("json"),
        false,
        true, // missing only
        env.data_dir_opt(),
    )
    .unwrap();

    let missing_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&missing_path).unwrap()).unwrap();
    let missing_roms = missing_json["roms"].as_array().unwrap();
    // All ROMs in our test DAT should be "have" since we scanned the matching file
    assert_eq!(
        missing_roms.len(),
        0,
        "Should have 0 ROMs with --missing filter (all are found)"
    );
}

/// Test dat fetch --list shows available sources
#[test]
fn test_dat_fetch_list() {
    // The fetch module has built-in sources
    assert!(!cat198x::cli::fetch::KNOWN_SOURCES.is_empty());

    // Check that MAME source exists
    let mame = cat198x::cli::fetch::KNOWN_SOURCES
        .iter()
        .find(|s| s.name == "mame");
    assert!(mame.is_some(), "MAME source should be available");
}

/// Test torrent create and verify commands
#[test]
fn test_torrent_create_and_verify() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
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
        },
        env.data_dir_opt(),
    )
    .expect("DAT import failed");

    // Add source and scan
    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("Source add failed");

    cli::scan::run(None, false, env.data_dir_opt()).unwrap();

    // Stats should run without error
    cli::stats::run(env.data_dir_opt()).expect("Stats command failed");
}
