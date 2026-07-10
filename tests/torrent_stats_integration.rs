//! Integration tests for torrent and stats command workflows.

use std::fs;

use cat198x::cli;

mod support;
use support::TestEnv;
use support::dats::create_test_dat;

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
