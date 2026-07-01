#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use cat198x::cli;
use cat198x::db::Database;
use tempfile::TempDir;

pub mod dats;

pub struct TestEnv {
    pub temp_dir: TempDir,
    pub data_dir: PathBuf,
    pub roms_dir: PathBuf,
}

impl TestEnv {
    pub fn new() -> Self {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let data_dir = temp_dir.path().join("data");
        let roms_dir = temp_dir.path().join("roms");

        fs::create_dir_all(&roms_dir).expect("failed to create roms dir");

        TestEnv {
            temp_dir,
            data_dir,
            roms_dir,
        }
    }

    pub fn init(&self) {
        cli::init::run(Some(self.data_dir.clone()), None).expect("init failed");
    }

    pub fn db(&self) -> Database {
        let db_path = self.data_dir.join("db.sqlite");
        Database::open(&db_path).expect("failed to open database")
    }

    pub fn data_dir_opt(&self) -> Option<PathBuf> {
        Some(self.data_dir.clone())
    }

    pub fn add_source(&self, path: &Path, preserve: bool, consume: bool) {
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

    pub fn source_id(&self, path: &Path, message: &str) -> i64 {
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

    pub fn single_reclaim_log(&self) -> String {
        let logs_dir = self.data_dir.join("objects/reclaim-logs");
        let logs: Vec<_> = fs::read_dir(&logs_dir)
            .expect("reclaim log directory should exist")
            .filter_map(|entry| entry.ok())
            .collect();
        assert_eq!(logs.len(), 1);
        fs::read_to_string(logs[0].path()).expect("reclaim log should be readable")
    }
}

pub fn create_test_rom(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
    let rom_path = dir.join(name);
    if let Some(parent) = rom_path.parent() {
        fs::create_dir_all(parent).expect("failed to create rom parent directory");
    }
    fs::write(&rom_path, content).expect("failed to write rom file");
    rom_path
}

pub fn create_test_zip(dir: &Path, zip_name: &str, entry_name: &str, content: &[u8]) -> PathBuf {
    create_test_zip_entries(dir, zip_name, &[(entry_name, content)])
}

pub fn create_test_zip_entries(dir: &Path, zip_name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
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
