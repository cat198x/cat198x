//! Integration tests for Cat198x CLI workflow
//!
//! These tests exercise the full Phase 1 workflow:
//! init → dat add → source add → scan → status

use std::fs;
use std::path::PathBuf;

// Import the library crate
use cat198x::cli;

mod support;
use support::{TestEnv, create_test_rom, create_test_zip};

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
            recursive: false,
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
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, None, env.data_dir_opt()).unwrap();

    // Create destination directory
    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).unwrap();

    // Generate plan - note: this will print output but we just verify it doesn't panic
    // A real plan would require destination configuration
    cli::plan::run(None, None, env.data_dir_opt()).unwrap();

    // Check that the plans directory exists
    let plans_dir = env.data_dir.join("objects/plans");

    // Note: Without destination config, plan might be empty, which is ok
    // The important thing is the command runs successfully
    assert!(plans_dir.exists(), "Plans directory should exist");
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
            recursive: false,
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
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("Source add failed");

    // Scan to index the file
    cli::scan::run(None, false, None, env.data_dir_opt()).expect("Scan failed");

    // Create destination directory
    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).expect("Failed to create dest dir");

    // Configure destination path for the collection
    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Apply Test", dest_dir.to_str().unwrap())
        .expect("Failed to set dest_path");
    drop(db);

    // Generate plan
    cli::plan::run(None, None, env.data_dir_opt()).expect("Plan generation failed");

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
    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("Apply failed");

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
            recursive: false,
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
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("Source add failed");

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("Scan failed");

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
    cli::plan::run(None, None, env.data_dir_opt()).expect("Plan generation failed");
    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("Apply failed");

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
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    create_test_rom(&env.roms_dir, "test.rom", test_content);

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

    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).unwrap();

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Stale Test", dest_dir.to_str().unwrap())
        .unwrap();
    drop(db);

    // Generate plan
    cli::plan::run(None, None, env.data_dir_opt()).unwrap();

    // Now modify the state by adding a new file and rescanning
    create_test_rom(&env.roms_dir, "new_file.rom", b"new content");
    cli::scan::run(None, true, None, env.data_dir_opt()).unwrap(); // Full rescan

    // Apply should detect stale plan and not execute
    // (The apply command prints a message but doesn't error)
    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).unwrap();

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
            recursive: false,
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
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, None, env.data_dir_opt()).unwrap();

    // Configure destination
    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).unwrap();

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Multi Test", dest_dir.to_str().unwrap())
        .unwrap();
    drop(db);

    // Generate plan
    cli::plan::run(None, None, env.data_dir_opt()).unwrap();

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
    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).unwrap();

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
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Place the matching file at its canonical destination and scan THAT as the
    // source, so the catalogue records the file already where it belongs. Under
    // the catalogue-trust model, "already correct" means the catalogue shows the
    // file at its destination — which a re-plan then leaves alone.
    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).unwrap();
    // Canonicalize so the configured destination matches the canonicalized path
    // `source add` records (on macOS /var is a symlink to /private/var).
    let dest_dir = std::fs::canonicalize(&dest_dir).unwrap();
    let dest_file = dest_dir.join("test.rom");
    fs::write(&dest_file, test_content).unwrap();

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: dest_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();
    cli::scan::run(None, false, None, env.data_dir_opt()).unwrap();

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Skip Test", dest_dir.to_str().unwrap()).unwrap();
    drop(db);

    // Generate plan - should detect file is already correct
    cli::plan::run(None, None, env.data_dir_opt()).unwrap();

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
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Add a source
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
            recursive: false,
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
            recursive: false,
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
            recursive: false,
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
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::scan::run(None, false, None, env.data_dir_opt()).unwrap();

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

/// End-to-end: a recursive add over a nested DAT tree, a library-wide default
/// destination, and a plan that lays the matched ROM out under its hierarchy —
/// the whole layout-engine chain (M1 + M2 + M3 + set-default) composed.
#[test]
fn test_recursive_add_plans_into_hierarchical_destination() {
    use cat198x::{ConfigCommands, DatCommands, SourceCommands};
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    // A file with known content, and a DAT — nested under a set tree like the
    // canonical DatRoot — that expects exactly its SHA1.
    let content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(content));

    let dats_root = env.temp_dir.path().join("dats");
    let coll_dir = dats_root.join("Acorn/BBC/Magazines/Laserbug");
    fs::create_dir_all(&coll_dir).unwrap();
    create_matching_dat(&coll_dir, "Acorn BBC - Magazines - Laserbug", &sha1_hash);

    // Recursive add records the nested path on the collection's node.
    cli::dat::run(
        DatCommands::Add {
            path: dats_root.clone(),
            collection: None,
            recursive: true,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // A source holding the matching file, scanned.
    fs::write(env.roms_dir.join("test.rom"), content).unwrap();
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

    // A library-wide default destination — no per-collection config at all.
    let dest_root = env.temp_dir.path().join("library");
    cli::config::run(
        ConfigCommands::SetDefault {
            key: "dest_path".to_string(),
            value: dest_root.to_string_lossy().into_owned(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Plan, then read the saved plan and assert the destination is hierarchical.
    cli::plan::run(None, None, env.data_dir_opt()).unwrap();

    let plans_dir = env.data_dir.join("objects/plans");
    let plan_file = fs::read_dir(&plans_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .expect("a plan file should have been written");
    let plan_json = fs::read_to_string(&plan_file).unwrap();

    let expected = format!(
        "{}/Acorn/BBC/Magazines/Laserbug/test.rom",
        dest_root.to_string_lossy()
    );
    assert!(
        plan_json.contains(&expected),
        "plan should place the ROM at its hierarchical destination '{}'.\nPlan was:\n{}",
        expected,
        plan_json
    );
}

/// End-to-end archive output: a zip-format collection plans one archive per game,
/// `apply` builds it with canonical entry names, and a re-plan converges to a
/// no-op because the archive is already correct.
#[test]
fn test_zip_output_format_plans_applies_and_converges() {
    use cat198x::config::OutputFormat;
    use cat198x::plan::generate_plan_filtered;
    use cat198x::{ConfigCommands, DatCommands, SourceCommands};
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(content));
    let dat = create_matching_dat(env.temp_dir.path(), "Zip Test", &sha1_hash);
    cli::dat::run(
        DatCommands::Add {
            path: dat,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // The matching source file (entry should be named "test.rom" from the DAT).
    fs::write(env.roms_dir.join("test.rom"), content).unwrap();
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

    // Library-wide defaults: a destination and zip output.
    let dest_root = env.temp_dir.path().join("library");
    fs::create_dir_all(&dest_root).unwrap();
    // Canonicalize so the built archive's path resolves under the source root
    // (on macOS /var is a symlink to /private/var, which `source add` resolves).
    let dest_root = std::fs::canonicalize(&dest_root).unwrap();
    // The library is itself a source, so apply can record what it places there
    // and a re-plan converges without a re-scan.
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: dest_root.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();
    for (k, v) in [
        ("dest_path", dest_root.to_string_lossy().into_owned()),
        ("output_format", "zip".to_string()),
    ] {
        cli::config::run(
            ConfigCommands::SetDefault {
                key: k.to_string(),
                value: v,
            },
            env.data_dir_opt(),
        )
        .unwrap();
    }

    // Plan: one repack to an archive named after the game.
    cli::plan::run(None, None, env.data_dir_opt()).unwrap();
    let plans_dir = env.data_dir.join("objects/plans");
    let plan_file = fs::read_dir(&plans_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .expect("a plan file should have been written");
    let plan_json = fs::read_to_string(&plan_file).unwrap();
    assert!(
        plan_json.contains("\"type\": \"repack\""),
        "zip output should produce a repack op, plan was:\n{plan_json}"
    );
    // Non-recursive add → flat node path "Zip Test", so the collection's root is
    // <library>/Zip Test and the archive is named after the game within it.
    let expected_archive = dest_root.join("Zip Test").join("Test Game.zip");
    assert!(
        plan_json.contains(&expected_archive.to_string_lossy().into_owned()),
        "archive should be named after the game; plan was:\n{plan_json}"
    );

    // Apply builds the archive.
    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).unwrap();
    assert!(
        expected_archive.is_file(),
        "apply should have created {}",
        expected_archive.display()
    );

    // Re-plan converges: the archive is already correct, so no repack.
    let db = env.db();
    let plan = generate_plan_filtered(
        db.conn(),
        &cat198x::plan::PlanOptions {
            default_dest: Some(dest_root.to_string_lossy().into_owned()),
            default_format: OutputFormat::Zip,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        plan.summary.repack_count, 0,
        "an already-correct archive should not be repacked again"
    );
}

/// `apply --skip-repack` applies the cheap operations (here a duplicate
/// quarantine) and defers the expensive repack, leaving it pending. A second
/// `apply` then completes the repack — and must resume even though the cheap
/// pass changed the catalogue (and so the state hash) underneath the plan.
#[test]
fn test_apply_skip_repack_defers_then_resumes() {
    use cat198x::{ConfigCommands, DatCommands, SourceCommands};
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(content));
    let dat = create_matching_dat(env.temp_dir.path(), "Skip Repack Test", &sha1_hash);
    cli::dat::run(
        DatCommands::Add {
            path: dat,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // The same ROM held in two sources: one becomes the repack source, the
    // duplicate is quarantined. That gives a cheap op (quarantine) plus an
    // expensive one (repack) in a single plan.
    let roms2 = env.temp_dir.path().join("roms2");
    fs::create_dir_all(&roms2).unwrap();
    fs::write(env.roms_dir.join("test.rom"), content).unwrap();
    fs::write(roms2.join("test.rom"), content).unwrap();
    for dir in [env.roms_dir.clone(), roms2.clone()] {
        cli::source::run(
            SourceCommands::Add {
                path: dir,
                preserve: false,
                consume: false,
            },
            env.data_dir_opt(),
        )
        .unwrap();
    }
    cli::scan::run(None, false, None, env.data_dir_opt()).unwrap();

    let dest_root = env.temp_dir.path().join("library");
    fs::create_dir_all(&dest_root).unwrap();
    let dest_root = std::fs::canonicalize(&dest_root).unwrap();
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: dest_root.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();
    for (k, v) in [
        ("dest_path", dest_root.to_string_lossy().into_owned()),
        ("output_format", "zip".to_string()),
    ] {
        cli::config::run(
            ConfigCommands::SetDefault {
                key: k.to_string(),
                value: v,
            },
            env.data_dir_opt(),
        )
        .unwrap();
    }

    // Move mode: the duplicate is deleted (cheap), the kept copy is repacked.
    cli::plan::run(None, None, env.data_dir_opt()).unwrap();
    let expected_archive = dest_root.join("Skip Repack Test").join("Test Game.zip");

    // Pass 1: defer the repack. The duplicate is deleted (one of the two copies
    // removed), leaving the kept copy for the repack, but the archive is not
    // built yet.
    cli::apply::run(false, true, true, 4, false, env.data_dir_opt()).unwrap();
    let remaining = [env.roms_dir.join("test.rom"), roms2.join("test.rom")]
        .iter()
        .filter(|p| p.exists())
        .count();
    assert_eq!(
        remaining, 1,
        "the duplicate copy should have been deleted in the cheap pass"
    );
    assert!(
        !expected_archive.exists(),
        "the repack should be deferred, not built, in pass 1"
    );

    // Pass 2: no flag. The cheap pass changed the catalogue, so the plan's
    // stored state hash no longer matches — the resume path must still apply the
    // pending repack rather than rejecting the plan as stale.
    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).unwrap();
    assert!(
        expected_archive.is_file(),
        "the deferred repack should be completed on the second apply: {}",
        expected_archive.display()
    );
}

/// A move-mode repack deletes the loose source once the archive holds a
/// verified copy, and a rollback restores that source by extracting it back out
/// of the archive before deleting the archive — a lossless round trip.
#[test]
fn test_move_repack_deletes_source_and_rollback_restores_it() {
    use cat198x::{ConfigCommands, DatCommands, SourceCommands};
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(content));
    let dat = create_matching_dat(env.temp_dir.path(), "Move Repack Test", &sha1_hash);
    cli::dat::run(
        DatCommands::Add {
            path: dat,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    let source_file = env.roms_dir.join("test.rom");
    fs::write(&source_file, content).unwrap();
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

    let dest_root = env.temp_dir.path().join("library");
    fs::create_dir_all(&dest_root).unwrap();
    let dest_root = std::fs::canonicalize(&dest_root).unwrap();
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: dest_root.clone(),
        },
        env.data_dir_opt(),
    )
    .unwrap();
    for (k, v) in [
        ("dest_path", dest_root.to_string_lossy().into_owned()),
        ("output_format", "zip".to_string()),
    ] {
        cli::config::run(
            ConfigCommands::SetDefault {
                key: k.to_string(),
                value: v,
            },
            env.data_dir_opt(),
        )
        .unwrap();
    }

    // Plan in move mode (the third argument), then apply.
    cli::plan::run(None, None, env.data_dir_opt()).unwrap();
    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).unwrap();

    let archive = dest_root.join("Move Repack Test").join("Test Game.zip");
    assert!(archive.is_file(), "the archive should have been built");
    assert!(
        !source_file.exists(),
        "the loose source should have been deleted by the move-mode repack"
    );

    // Rolling back restores the loose source and removes the archive.
    cli::apply::run_rollback(false, false, env.data_dir_opt()).unwrap();
    assert!(
        source_file.is_file(),
        "rollback should restore the loose source out of the archive"
    );
    assert_eq!(
        fs::read(&source_file).unwrap(),
        content,
        "the restored source should be byte-identical"
    );
    assert!(
        !archive.exists(),
        "rollback should delete the archive it built"
    );
}

/// The plan records a per-collection tally (feeding the by-set breakdown).
#[test]
fn test_plan_records_per_collection_breakdown() {
    use cat198x::config::OutputFormat;
    use cat198x::plan::generate_plan_filtered;
    use cat198x::{DatCommands, SourceCommands};
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(content));
    let dat = create_matching_dat(env.temp_dir.path(), "Breakdown Test", &sha1_hash);
    cli::dat::run(
        DatCommands::Add {
            path: dat,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    fs::write(env.roms_dir.join("test.rom"), content).unwrap();
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

    let dest = env.temp_dir.path().join("out");
    let db = env.db();
    let plan = generate_plan_filtered(
        db.conn(),
        &cat198x::plan::PlanOptions {
            default_dest: Some(dest.to_string_lossy().into_owned()),
            default_format: OutputFormat::Loose,
            ..Default::default()
        },
    )
    .unwrap();

    let stat = plan
        .per_collection
        .iter()
        .find(|c| c.name == "Breakdown Test")
        .expect("collection should appear in the per-collection breakdown");
    assert_eq!(stat.to_write, 1, "one ROM to copy");
    assert_eq!(stat.already_correct, 0);
    assert!(stat.bytes > 0);
}

/// `plan --move` relocates a misnamed file into its canonical place and removes
/// the original, rather than copying and leaving a duplicate.
#[test]
fn test_move_mode_relocates_and_removes_source() {
    use cat198x::{ConfigCommands, DatCommands, SourceCommands};
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(content));
    // The DAT's canonical ROM name is "test.rom"; the source file is misnamed,
    // so a real relocation happens.
    let dat = create_matching_dat(env.temp_dir.path(), "Move Test", &sha1_hash);
    cli::dat::run(
        DatCommands::Add {
            path: dat,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    let misnamed = env.roms_dir.join("wrongname.rom");
    fs::write(&misnamed, content).unwrap();
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

    let dest_root = env.temp_dir.path().join("library");
    cli::config::run(
        ConfigCommands::SetDefault {
            key: "dest_path".to_string(),
            value: dest_root.to_string_lossy().into_owned(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // Plan with --move, then apply.
    cli::plan::run(None, None, env.data_dir_opt()).unwrap();
    let plans_dir = env.data_dir.join("objects/plans");
    let plan_json = fs::read_to_string(
        fs::read_dir(&plans_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .expect("a plan file"),
    )
    .unwrap();
    assert!(
        plan_json.contains("\"type\": \"move\""),
        "--move should produce move ops, plan was:\n{plan_json}"
    );

    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).unwrap();

    // Canonical file placed; misnamed original gone (moved, not copied).
    let placed = dest_root.join("Move Test").join("test.rom");
    assert!(
        placed.is_file(),
        "canonical file should exist at {}",
        placed.display()
    );
    assert!(
        !misnamed.exists(),
        "the misnamed source should have been removed by the move"
    );
}

/// `apply --prune-empty` removes the source subdirectory a `--move` tidy emptied,
/// while leaving the registered source root in place.
#[test]
fn test_apply_prune_empty_removes_emptied_source_subdir() {
    use cat198x::{ConfigCommands, DatCommands, SourceCommands};
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let content = b"prunable";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(content));
    let dat = create_matching_dat(env.temp_dir.path(), "Prune Test", &sha1_hash);
    cli::dat::run(
        DatCommands::Add {
            path: dat,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .unwrap();

    // The only file lives in a subdirectory of the source root, misnamed so the
    // move relocates it out — leaving the subdirectory empty.
    let subdir = env.roms_dir.join("nested");
    fs::create_dir_all(&subdir).unwrap();
    let misnamed = subdir.join("wrongname.rom");
    fs::write(&misnamed, content).unwrap();
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

    let dest_root = env.temp_dir.path().join("library");
    cli::config::run(
        ConfigCommands::SetDefault {
            key: "dest_path".to_string(),
            value: dest_root.to_string_lossy().into_owned(),
        },
        env.data_dir_opt(),
    )
    .unwrap();

    cli::plan::run(None, None, env.data_dir_opt()).unwrap();
    // Apply with prune_empty = true.
    cli::apply::run(false, true, false, 4, true, env.data_dir_opt()).unwrap();

    // The file was placed, the emptied subdirectory pruned, the source root kept.
    assert!(
        dest_root.join("Prune Test").join("test.rom").is_file(),
        "canonical file should be placed"
    );
    assert!(
        !subdir.exists(),
        "the emptied source subdirectory should have been pruned"
    );
    assert!(
        env.roms_dir.is_dir(),
        "the registered source root is never pruned"
    );
}
