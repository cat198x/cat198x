//! Integration tests for planning, apply, archive output, and move workflows.

use std::fs;
use std::path::PathBuf;

use cat198x::cli;

mod support;
use support::{TestEnv, create_test_rom, create_test_zip};

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
    fs::write(&dat_path, content).expect("failed to write DAT file");
    dat_path
}

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
    fs::write(&dat_path, content).expect("failed to write DAT file");
    dat_path
}

#[test]
fn plan_generation() {
    let env = TestEnv::new();
    env.init();

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
    .expect("dat import failed");

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
    .expect("source add failed");

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).expect("failed to create destination");

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");

    let plans_dir = env.data_dir.join("objects/plans");
    assert!(plans_dir.exists(), "plans directory should exist");
}

#[test]
fn plan_apply_rollback_cycle() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let test_content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(test_content));
    let dat_path = create_matching_dat(env.temp_dir.path(), "Apply Test", &sha1_hash);

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
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

    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).expect("failed to create destination");

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Apply Test", dest_dir.to_str().unwrap())
        .expect("failed to set destination path");
    drop(db);

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");

    let plans_dir = env.data_dir.join("objects/plans");
    let plan_files: Vec<_> = fs::read_dir(&plans_dir)
        .expect("plans directory should read")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(!plan_files.is_empty(), "plan file should be created");

    let plan_content = fs::read_to_string(plan_files[0].path()).expect("plan file should read");
    let plan: serde_json::Value =
        serde_json::from_str(&plan_content).expect("plan JSON should parse");
    let operations = plan["operations"]
        .as_array()
        .expect("plan operations should be an array");
    assert_eq!(operations.len(), 1, "should have 1 copy operation");

    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("apply failed");

    let dest_file = dest_dir.join("test.rom");
    assert!(dest_file.exists(), "file should be copied to destination");
    assert_eq!(
        fs::read(&dest_file).expect("destination file should read"),
        test_content,
        "copied file should have correct content"
    );

    let logs_dir = env.data_dir.join("objects/logs");
    let log_files: Vec<_> = fs::read_dir(&logs_dir)
        .expect("logs directory should read")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(!log_files.is_empty(), "operation log should be created");

    cli::apply::run_rollback(false, false, env.data_dir_opt()).expect("rollback failed");

    assert!(!dest_file.exists(), "file should be deleted after rollback");
    assert!(
        env.roms_dir.join("source.rom").exists(),
        "source file should remain untouched"
    );
}

#[test]
fn apply_from_zip_archive() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let test_content = b"archived rom data";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(test_content));
    let dat_path = create_matching_dat(env.temp_dir.path(), "Archive Test", &sha1_hash);

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat import failed");

    create_test_zip(&env.roms_dir, "games.zip", "inner_rom.bin", test_content);

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

    let db = env.db();
    let file = cat198x::db::files::get_file_by_sha1(db.conn(), &sha1_hash).expect("query failed");
    assert!(file.is_some(), "file from archive should be indexed");
    drop(db);

    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).expect("failed to create destination");

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Archive Test", dest_dir.to_str().unwrap())
        .expect("failed to set destination path");
    drop(db);

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");
    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("apply failed");

    let dest_file = dest_dir.join("test.rom");
    assert!(
        dest_file.exists(),
        "file should be extracted from archive to destination"
    );
    assert_eq!(
        fs::read(&dest_file).expect("destination file should read"),
        test_content,
        "extracted file should have correct content"
    );

    cli::apply::run_rollback(false, false, env.data_dir_opt()).expect("rollback failed");
    assert!(!dest_file.exists(), "file should be deleted after rollback");
    assert!(
        env.roms_dir.join("games.zip").exists(),
        "source archive should remain"
    );
}

#[test]
fn stale_plan_detection() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

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
    .expect("dat import failed");

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
    .expect("source add failed");

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).expect("failed to create destination");

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Stale Test", dest_dir.to_str().unwrap())
        .expect("failed to set destination path");
    drop(db);

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");

    create_test_rom(&env.roms_dir, "new_file.rom", b"new content");
    cli::scan::run(None, true, None, env.data_dir_opt()).expect("full rescan failed");

    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("apply failed");

    let dest_file = dest_dir.join("test.rom");
    assert!(
        !dest_file.exists(),
        "stale plan should not be applied - file should not exist"
    );
}

#[test]
fn multi_file_plan_apply() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

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

    use cat198x::DatCommands;
    cli::dat::run(
        DatCommands::Add {
            path: dat_path,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat import failed");

    for (content, name) in &contents {
        create_test_rom(&env.roms_dir, name, content);
    }

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

    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).expect("failed to create destination");

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Multi Test", dest_dir.to_str().unwrap())
        .expect("failed to set destination path");
    drop(db);

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");

    let plans_dir = env.data_dir.join("objects/plans");
    let plan_files: Vec<_> = fs::read_dir(&plans_dir)
        .expect("plans directory should read")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();

    let plan_content = fs::read_to_string(plan_files[0].path()).expect("plan file should read");
    let plan: serde_json::Value =
        serde_json::from_str(&plan_content).expect("plan JSON should parse");
    let operations = plan["operations"]
        .as_array()
        .expect("plan operations should be an array");
    assert_eq!(operations.len(), 3, "should have 3 copy operations");

    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("apply failed");

    for (content, name) in &contents {
        let dest_file = dest_dir.join(name);
        assert!(
            dest_file.exists(),
            "file {} should exist at destination",
            name
        );
        assert_eq!(
            fs::read(&dest_file).expect("destination file should read"),
            *content,
            "file {} should have correct content",
            name
        );
    }

    cli::apply::run_rollback(false, false, env.data_dir_opt()).expect("rollback failed");

    for (_, name) in &contents {
        let dest_file = dest_dir.join(name);
        assert!(
            !dest_file.exists(),
            "file {} should be deleted after rollback",
            name
        );
    }
}

#[test]
fn apply_skips_already_correct_files() {
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
    .expect("dat import failed");

    let dest_dir = env.temp_dir.path().join("output");
    fs::create_dir_all(&dest_dir).expect("failed to create destination");
    let dest_dir = std::fs::canonicalize(&dest_dir).expect("destination should canonicalize");
    let dest_file = dest_dir.join("test.rom");
    fs::write(&dest_file, test_content).expect("failed to write destination file");

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: dest_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("source add failed");
    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let db = env.db();
    cat198x::db::config::set_dest_path(db.conn(), "Skip Test", dest_dir.to_str().unwrap())
        .expect("failed to set destination path");
    drop(db);

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");

    assert!(dest_file.exists(), "destination file should still exist");
    assert_eq!(
        fs::read(&dest_file).expect("destination file should read"),
        test_content,
        "destination file should still have correct content"
    );

    let plans_dir = env.data_dir.join("objects/plans");
    assert!(plans_dir.exists(), "plans directory should exist");

    let plan_files: Vec<_> = fs::read_dir(&plans_dir)
        .expect("plans directory should read")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();

    if !plan_files.is_empty() {
        let plan_content = fs::read_to_string(plan_files[0].path()).expect("plan file should read");
        let plan: serde_json::Value =
            serde_json::from_str(&plan_content).expect("plan JSON should parse");
        let operations = plan["operations"]
            .as_array()
            .expect("plan operations should be an array");
        assert_eq!(
            operations.len(),
            0,
            "should have 0 operations - file already correct"
        );
    }
}

#[test]
fn recursive_add_plans_into_hierarchical_destination() {
    use cat198x::{ConfigCommands, DatCommands, SourceCommands};
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(content));

    let dats_root = env.temp_dir.path().join("dats");
    let coll_dir = dats_root.join("Acorn/BBC/Magazines/Laserbug");
    fs::create_dir_all(&coll_dir).expect("failed to create DAT collection directory");
    create_matching_dat(&coll_dir, "Acorn BBC - Magazines - Laserbug", &sha1_hash);

    cli::dat::run(
        DatCommands::Add {
            path: dats_root.clone(),
            collection: None,
            recursive: true,
        },
        env.data_dir_opt(),
    )
    .expect("recursive dat add failed");

    fs::write(env.roms_dir.join("test.rom"), content).expect("failed to write source ROM");
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

    let dest_root = env.temp_dir.path().join("library");
    cli::config::run(
        ConfigCommands::SetDefault {
            key: "dest_path".to_string(),
            value: dest_root.to_string_lossy().into_owned(),
        },
        env.data_dir_opt(),
    )
    .expect("config set-default failed");

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");

    let plans_dir = env.data_dir.join("objects/plans");
    let plan_file = fs::read_dir(&plans_dir)
        .expect("plans directory should read")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .expect("a plan file should have been written");
    let plan_json = fs::read_to_string(&plan_file).expect("plan file should read");

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

#[test]
fn zip_output_format_plans_applies_and_converges() {
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
    .expect("dat import failed");

    fs::write(env.roms_dir.join("test.rom"), content).expect("failed to write source ROM");
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

    let dest_root = env.temp_dir.path().join("library");
    fs::create_dir_all(&dest_root).expect("failed to create library");
    let dest_root = std::fs::canonicalize(&dest_root).expect("library should canonicalize");
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: dest_root.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("library source add failed");
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
        .expect("config set-default failed");
    }

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");
    let plans_dir = env.data_dir.join("objects/plans");
    let plan_file = fs::read_dir(&plans_dir)
        .expect("plans directory should read")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .expect("a plan file should have been written");
    let plan_json = fs::read_to_string(&plan_file).expect("plan file should read");
    assert!(
        plan_json.contains("\"type\": \"repack\""),
        "zip output should produce a repack op, plan was:\n{plan_json}"
    );

    let expected_archive = dest_root.join("Zip Test").join("Test Game.zip");
    assert!(
        plan_json.contains(&expected_archive.to_string_lossy().into_owned()),
        "archive should be named after the game; plan was:\n{plan_json}"
    );

    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("apply failed");
    assert!(
        expected_archive.is_file(),
        "apply should have created {}",
        expected_archive.display()
    );

    let db = env.db();
    let plan = generate_plan_filtered(
        db.conn(),
        &cat198x::plan::PlanOptions {
            default_dest: Some(dest_root.to_string_lossy().into_owned()),
            default_format: OutputFormat::Zip,
            ..Default::default()
        },
    )
    .expect("plan should generate");
    assert_eq!(
        plan.summary.repack_count, 0,
        "an already-correct archive should not be repacked again"
    );
}

#[test]
fn apply_skip_repack_defers_then_resumes() {
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
    .expect("dat import failed");

    let roms2 = env.temp_dir.path().join("roms2");
    fs::create_dir_all(&roms2).expect("failed to create second ROM dir");
    fs::write(env.roms_dir.join("test.rom"), content).expect("failed to write first ROM");
    fs::write(roms2.join("test.rom"), content).expect("failed to write second ROM");
    for dir in [env.roms_dir.clone(), roms2.clone()] {
        cli::source::run(
            SourceCommands::Add {
                path: dir,
                preserve: false,
                consume: false,
            },
            env.data_dir_opt(),
        )
        .expect("source add failed");
    }
    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let dest_root = env.temp_dir.path().join("library");
    fs::create_dir_all(&dest_root).expect("failed to create library");
    let dest_root = std::fs::canonicalize(&dest_root).expect("library should canonicalize");
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: dest_root.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("library source add failed");
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
        .expect("config set-default failed");
    }

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");
    let expected_archive = dest_root.join("Skip Repack Test").join("Test Game.zip");

    cli::apply::run(false, true, true, 4, false, env.data_dir_opt()).expect("apply failed");
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

    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("apply failed");
    assert!(
        expected_archive.is_file(),
        "the deferred repack should be completed on the second apply: {}",
        expected_archive.display()
    );
}

#[test]
fn move_repack_deletes_source_and_rollback_restores_it() {
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
    .expect("dat import failed");

    let source_file = env.roms_dir.join("test.rom");
    fs::write(&source_file, content).expect("failed to write source ROM");
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

    let dest_root = env.temp_dir.path().join("library");
    fs::create_dir_all(&dest_root).expect("failed to create library");
    let dest_root = std::fs::canonicalize(&dest_root).expect("library should canonicalize");
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: dest_root.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("library source add failed");
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
        .expect("config set-default failed");
    }

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");
    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("apply failed");

    let archive = dest_root.join("Move Repack Test").join("Test Game.zip");
    assert!(archive.is_file(), "the archive should have been built");
    assert!(
        !source_file.exists(),
        "the loose source should have been deleted by the move-mode repack"
    );

    cli::apply::run_rollback(false, false, env.data_dir_opt()).expect("rollback failed");
    assert!(
        source_file.is_file(),
        "rollback should restore the loose source out of the archive"
    );
    assert_eq!(
        fs::read(&source_file).expect("restored source should read"),
        content,
        "the restored source should be byte-identical"
    );
    assert!(
        !archive.exists(),
        "rollback should delete the archive it built"
    );
}

#[test]
fn plan_records_per_collection_breakdown() {
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
    .expect("dat import failed");

    fs::write(env.roms_dir.join("test.rom"), content).expect("failed to write source ROM");
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
    .expect("plan should generate");

    let stat = plan
        .per_collection
        .iter()
        .find(|c| c.name == "Breakdown Test")
        .expect("collection should appear in the per-collection breakdown");
    assert_eq!(stat.to_write, 1, "one ROM to copy");
    assert_eq!(stat.already_correct, 0);
    assert!(stat.bytes > 0);
}

#[test]
fn move_mode_relocates_and_removes_source() {
    use cat198x::{ConfigCommands, DatCommands, SourceCommands};
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let content = b"hello";
    let sha1_hash = cat198x::util::hex_upper(sha1::Sha1::digest(content));
    let dat = create_matching_dat(env.temp_dir.path(), "Move Test", &sha1_hash);
    cli::dat::run(
        DatCommands::Add {
            path: dat,
            collection: None,
            recursive: false,
        },
        env.data_dir_opt(),
    )
    .expect("dat import failed");

    let misnamed = env.roms_dir.join("wrongname.rom");
    fs::write(&misnamed, content).expect("failed to write misnamed source ROM");
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

    let dest_root = env.temp_dir.path().join("library");
    cli::config::run(
        ConfigCommands::SetDefault {
            key: "dest_path".to_string(),
            value: dest_root.to_string_lossy().into_owned(),
        },
        env.data_dir_opt(),
    )
    .expect("config set-default failed");

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");
    let plans_dir = env.data_dir.join("objects/plans");
    let plan_json = fs::read_to_string(
        fs::read_dir(&plans_dir)
            .expect("plans directory should read")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .expect("a plan file"),
    )
    .expect("plan file should read");
    assert!(
        plan_json.contains("\"type\": \"move\""),
        "--move should produce move ops, plan was:\n{plan_json}"
    );

    cli::apply::run(false, true, false, 4, false, env.data_dir_opt()).expect("apply failed");

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

#[test]
fn apply_prune_empty_removes_emptied_source_subdir() {
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
    .expect("dat import failed");

    let subdir = env.roms_dir.join("nested");
    fs::create_dir_all(&subdir).expect("failed to create source subdirectory");
    let misnamed = subdir.join("wrongname.rom");
    fs::write(&misnamed, content).expect("failed to write misnamed source ROM");
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

    let dest_root = env.temp_dir.path().join("library");
    cli::config::run(
        ConfigCommands::SetDefault {
            key: "dest_path".to_string(),
            value: dest_root.to_string_lossy().into_owned(),
        },
        env.data_dir_opt(),
    )
    .expect("config set-default failed");

    cli::plan::run(None, None, env.data_dir_opt()).expect("plan generation failed");
    cli::apply::run(false, true, false, 4, true, env.data_dir_opt()).expect("apply failed");

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
