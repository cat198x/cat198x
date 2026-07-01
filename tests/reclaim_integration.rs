//! Integration tests for reclaim workflows.

use std::fs;
use std::path::{Path, PathBuf};

use cat198x::cli;
use cat198x::db::Database;
use tempfile::TempDir;

struct TestEnv {
    temp_dir: TempDir,
    data_dir: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let data_dir = temp_dir.path().join("data");
        TestEnv { temp_dir, data_dir }
    }

    fn init(&self) {
        cli::init::run(Some(self.data_dir.clone()), None).expect("init failed");
    }

    fn db(&self) -> Database {
        let db_path = self.data_dir.join("db.sqlite");
        Database::open(&db_path).expect("failed to open database")
    }

    fn data_dir_opt(&self) -> Option<PathBuf> {
        Some(self.data_dir.clone())
    }

    fn add_source(&self, path: &Path, preserve: bool, consume: bool) {
        use cat198x::SourceCommands;

        cli::source::run(
            SourceCommands::Add {
                preserve,
                consume,
                path: path.to_path_buf(),
            },
            self.data_dir_opt(),
        )
        .expect("source add failed");
    }

    fn source_id(&self, path: &Path, message: &str) -> i64 {
        let db = self.db();
        let conn = db.conn();
        let root = fs::canonicalize(path)
            .expect("source path should canonicalize")
            .to_string_lossy()
            .into_owned();
        cat198x::db::files::list_sources(conn)
            .expect("sources should list")
            .into_iter()
            .find(|source| source.path == root)
            .expect(message)
            .id
    }

    fn single_reclaim_log(&self) -> String {
        let logs_dir = self.data_dir.join("objects/reclaim-logs");
        let logs: Vec<_> = fs::read_dir(&logs_dir)
            .expect("reclaim log directory should exist")
            .filter_map(|entry| entry.ok())
            .collect();
        assert_eq!(logs.len(), 1);
        fs::read_to_string(logs[0].path()).expect("reclaim log should be readable")
    }
}

fn create_test_rom(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
    let rom_path = dir.join(name);
    if let Some(parent) = rom_path.parent() {
        fs::create_dir_all(parent).expect("failed to create rom parent directory");
    }
    fs::write(&rom_path, content).expect("failed to write rom file");
    rom_path
}

fn create_test_zip_entries(dir: &Path, zip_name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
    use std::io::Write;

    let zip_path = dir.join(zip_name);
    if let Some(parent) = zip_path.parent() {
        fs::create_dir_all(parent).expect("failed to create zip parent directory");
    }
    let file = fs::File::create(&zip_path).expect("failed to create zip file");
    let mut zip = zip::ZipWriter::new(file);

    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (entry_name, content) in entries {
        zip.start_file(entry_name, options)
            .expect("start zip entry");
        zip.write_all(content).expect("write zip entry");
    }
    zip.finish().expect("finish zip archive");

    zip_path
}

#[test]
fn reclaim_execute_removes_redundant_consume_source_file() {
    let env = TestEnv::new();
    env.init();

    let staging_dir = env.temp_dir.path().join("staging");
    let library_dir = env.temp_dir.path().join("library");
    let staging_file = create_test_rom(&staging_dir, "redundant.rom", b"same bytes");
    let library_file = create_test_rom(&library_dir, "copy.rom", b"same bytes");
    let staging_file_canonical = fs::canonicalize(&staging_file)
        .expect("staging file should canonicalize")
        .to_string_lossy()
        .into_owned();
    let sha1 = cat198x::scanner::hasher::hash_file(&library_file)
        .expect("library file should hash")
        .sha1;

    env.add_source(&staging_dir, false, true);
    env.add_source(&library_dir, true, false);

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let staging_id = env.source_id(&staging_dir, "staging source exists");

    cli::reclaim::run(Some(staging_id.to_string()), true, env.data_dir_opt())
        .expect("reclaim execute failed");

    assert!(!staging_file.exists(), "redundant staging file is deleted");
    assert!(library_file.exists(), "verified survivor remains on disk");

    let db = env.db();
    let conn = db.conn();
    let locations =
        cat198x::db::files::get_file_locations(conn, &sha1).expect("file locations should load");
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, "copy.rom");

    assert_eq!(env.single_reclaim_log(), staging_file_canonical);
}

#[test]
fn reclaim_execute_refuses_preserve_source() {
    let env = TestEnv::new();
    env.init();

    let preserve_dir = env.temp_dir.path().join("master");
    let library_dir = env.temp_dir.path().join("library");
    let preserve_file = create_test_rom(&preserve_dir, "redundant.rom", b"same bytes");
    let library_file = create_test_rom(&library_dir, "copy.rom", b"same bytes");
    let sha1 = cat198x::scanner::hasher::hash_file(&library_file)
        .expect("library file should hash")
        .sha1;

    env.add_source(&preserve_dir, true, false);
    env.add_source(&library_dir, true, false);

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let preserve_id = env.source_id(&preserve_dir, "preserve source exists");

    cli::reclaim::run(Some(preserve_id.to_string()), true, env.data_dir_opt())
        .expect("reclaim execute failed");

    assert!(
        preserve_file.exists(),
        "preserve source file is left untouched"
    );
    assert!(library_file.exists(), "survivor remains on disk");

    let db = env.db();
    let conn = db.conn();
    let locations =
        cat198x::db::files::get_file_locations(conn, &sha1).expect("file locations should load");
    assert_eq!(locations.len(), 2, "catalogue keeps both preserve copies");

    let logs_dir = env.data_dir.join("objects/reclaim-logs");
    assert!(!logs_dir.exists(), "refused reclaim writes no audit log");
}

#[test]
fn reclaim_execute_removes_redundant_archive_container() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let staging_dir = env.temp_dir.path().join("staging");
    let library_dir = env.temp_dir.path().join("library");
    fs::create_dir_all(&staging_dir).expect("failed to create staging dir");
    fs::create_dir_all(&library_dir).expect("failed to create library dir");

    let entries: &[(&str, &[u8])] = &[("a.rom", b"alpha"), ("b.rom", b"beta")];
    let staging_archive = create_test_zip_entries(&staging_dir, "redundant.zip", entries);
    let library_archive = create_test_zip_entries(&library_dir, "canonical.zip", entries);
    let staging_archive_canonical = fs::canonicalize(&staging_archive)
        .expect("staging archive should canonicalize")
        .to_string_lossy()
        .into_owned();
    let sha1s = entries
        .iter()
        .map(|(_, content)| cat198x::util::hex_upper(sha1::Sha1::digest(content)))
        .collect::<Vec<_>>();

    env.add_source(&staging_dir, false, true);
    env.add_source(&library_dir, true, false);

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let staging_id = env.source_id(&staging_dir, "staging source exists");
    let library_id = env.source_id(&library_dir, "library source exists");

    cli::reclaim::run(Some(staging_id.to_string()), true, env.data_dir_opt())
        .expect("reclaim execute failed");

    assert!(
        !staging_archive.exists(),
        "redundant staging archive is deleted"
    );
    assert!(library_archive.exists(), "library archive remains on disk");

    let db = env.db();
    let conn = db.conn();
    for (sha1, (entry_name, _)) in sha1s.iter().zip(entries.iter()) {
        let locations =
            cat198x::db::files::get_file_locations(conn, sha1).expect("file locations should load");
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].source_id, library_id);
        assert_eq!(locations[0].path, "canonical.zip");
        assert_eq!(locations[0].archive_path.as_deref(), Some(*entry_name));
    }

    assert_eq!(env.single_reclaim_log(), staging_archive_canonical);
}
