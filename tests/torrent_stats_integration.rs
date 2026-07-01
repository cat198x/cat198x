//! Integration tests for torrent and stats command workflows.

use std::fs;
use std::path::PathBuf;

use cat198x::cli;

mod support;
use support::TestEnv;

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
fn torrent_create_and_verify() {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let content_dir = temp_dir.path().join("content");
    fs::create_dir_all(&content_dir).expect("failed to create content dir");

    fs::write(content_dir.join("file1.bin"), b"test content one").expect("failed to write file1");
    fs::write(content_dir.join("file2.bin"), b"test content two").expect("failed to write file2");

    let torrent_path = temp_dir.path().join("test.torrent");

    use cat198x::TorrentCommands;
    cli::torrent::run(TorrentCommands::Create {
        path: content_dir.clone(),
        output: Some(torrent_path.clone()),
        piece_size: Some(16384),
        tracker: vec!["http://tracker.example.com/announce".to_string()],
        comment: Some("Test torrent".to_string()),
        private: false,
    })
    .expect("torrent creation failed");

    assert!(torrent_path.exists(), "torrent file should be created");

    cli::torrent::run(TorrentCommands::Verify {
        torrent: torrent_path,
        path: Some(temp_dir.path().to_path_buf()),
    })
    .expect("torrent verification should pass");
}

#[test]
fn stats_command() {
    let env = TestEnv::new();
    env.init();

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

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");
    cli::stats::run(None, env.data_dir_opt()).expect("stats command failed");
}
