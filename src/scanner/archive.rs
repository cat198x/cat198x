//! Archive reading utilities (ZIP, 7Z)

use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use walkdir::WalkDir;

use super::hasher::{FileHashes, hash_file, hash_reader};

/// An entry within an archive
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub name: String,
    pub size: u64,
    pub hashes: Option<FileHashes>,
}

/// Supported archive types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveType {
    Zip,
    SevenZip,
}

impl ArchiveType {
    /// Detect archive type from file extension
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_lowercase();
        match ext.as_str() {
            "zip" => Some(ArchiveType::Zip),
            "7z" => Some(ArchiveType::SevenZip),
            _ => None,
        }
    }
}

/// List entries in an archive (without hashing)
pub fn list_archive_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    match ArchiveType::from_path(path) {
        Some(ArchiveType::Zip) => list_zip_entries(path),
        Some(ArchiveType::SevenZip) => list_7z_entries(path),
        None => anyhow::bail!("Unknown archive type: {:?}", path),
    }
}

/// Hash all entries in an archive
pub fn hash_archive_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    match ArchiveType::from_path(path) {
        Some(ArchiveType::Zip) => hash_zip_entries(path),
        Some(ArchiveType::SevenZip) => hash_7z_entries(path),
        None => anyhow::bail!("Unknown archive type: {:?}", path),
    }
}

// === ZIP handling ===

fn list_zip_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let file = std::fs::File::open(path)?;
    let mut archive =
        zip::ZipArchive::new(file).with_context(|| format!("Failed to open ZIP: {:?}", path))?;

    let mut entries = Vec::new();

    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;

        // Skip directories
        if entry.is_dir() {
            continue;
        }

        entries.push(ArchiveEntry {
            name: entry.name().to_string(),
            size: entry.size(),
            hashes: None,
        });
    }

    Ok(entries)
}

fn hash_zip_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let file = std::fs::File::open(path)?;
    let mut archive =
        zip::ZipArchive::new(file).with_context(|| format!("Failed to open ZIP: {:?}", path))?;

    let mut entries = Vec::new();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;

        // Skip directories
        if entry.is_dir() {
            continue;
        }

        let size = entry.size();
        let name = entry.name().to_string();

        // Read and hash the entry
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        let mut cursor = std::io::Cursor::new(&data);
        let hashes = hash_reader(&mut cursor, size)?;

        entries.push(ArchiveEntry {
            name,
            size,
            hashes: Some(hashes),
        });
    }

    Ok(entries)
}

// === 7Z handling ===

fn list_7z_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let mut archive = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
        .with_context(|| format!("Failed to open 7Z: {:?}", path))?;

    let mut entries = Vec::new();

    archive.for_each_entries(|entry, _reader| {
        // Skip directories
        if !entry.is_directory() {
            entries.push(ArchiveEntry {
                name: entry.name().to_string(),
                size: entry.size(),
                hashes: None,
            });
        }
        Ok(true)
    })?;

    Ok(entries)
}

/// The system 7z binary (prefer `7zz` > `7z` > `7za`), located once.
///
/// The bundled `sevenz-rust2` decoder is pure-Rust and pathologically slow on
/// large LZMA archives — 70 GB of magazine `.7z` took hours. The system binary
/// is the optimised C implementation and is dramatically faster, so we prefer
/// it when present and fall back to the Rust decoder otherwise.
static SYSTEM_7Z: LazyLock<Option<PathBuf>> = LazyLock::new(find_system_7z);

fn find_system_7z() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for name in ["7zz", "7z", "7za"] {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn hash_7z_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    match SYSTEM_7Z.as_ref() {
        Some(bin) => hash_7z_entries_system(bin, path),
        None => hash_7z_entries_native(path),
    }
}

/// Hash 7z entries by extracting with the system 7z binary into a temp dir,
/// then hashing the loose files. Far faster than the Rust decoder on large
/// archives. Falls back to the Rust decoder if the binary rejects the archive.
fn hash_7z_entries_system(bin: &Path, path: &Path) -> Result<Vec<ArchiveEntry>> {
    let temp = tempfile::Builder::new()
        .prefix("cat198x-7z-")
        .tempdir()
        .context("Failed to create temp dir for 7z extraction")?;

    let status = Command::new(bin)
        .arg("x") // extract with full paths
        .arg(path)
        .arg(format!("-o{}", temp.path().display()))
        .arg("-y") // assume yes on prompts
        .arg("-bso0") // silence stdout
        .arg("-bse0") // silence stderr
        .arg("-bsp0") // silence progress
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("Failed to run {bin:?} on {path:?}"))?;

    if !status.success() {
        // An odd codec or encryption the system binary won't take — let the
        // Rust decoder try, so one stubborn archive isn't fatal to the scan.
        return hash_7z_entries_native(path);
    }

    let mut entries = Vec::new();
    for entry in WalkDir::new(temp.path()).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry
            .path()
            .strip_prefix(temp.path())
            .unwrap_or_else(|_| entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let hashes =
            hash_file(entry.path()).with_context(|| format!("Failed to hash 7z entry {name}"))?;
        entries.push(ArchiveEntry {
            name,
            size,
            hashes: Some(hashes),
        });
    }
    Ok(entries)
}

fn hash_7z_entries_native(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let mut archive = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
        .with_context(|| format!("Failed to open 7Z: {:?}", path))?;

    let mut entries = Vec::new();
    let mut hash_error: Option<anyhow::Error> = None;

    archive.for_each_entries(|entry, reader| {
        // Skip directories
        if entry.is_directory() {
            return Ok(true);
        }

        let size = entry.size();
        let name = entry.name().to_string();

        // Read into buffer and hash
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;

        let mut cursor = std::io::Cursor::new(&data);
        match hash_reader(&mut cursor, size) {
            Ok(hashes) => {
                entries.push(ArchiveEntry {
                    name,
                    size,
                    hashes: Some(hashes),
                });
            }
            Err(e) => {
                hash_error = Some(e);
                return Ok(false); // Stop iteration
            }
        }

        Ok(true)
    })?;

    if let Some(e) = hash_error {
        return Err(e);
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_archive_type_detection() {
        assert_eq!(
            ArchiveType::from_path(Path::new("test.zip")),
            Some(ArchiveType::Zip)
        );
        assert_eq!(
            ArchiveType::from_path(Path::new("test.7z")),
            Some(ArchiveType::SevenZip)
        );
        assert_eq!(ArchiveType::from_path(Path::new("test.rar")), None);
        assert_eq!(ArchiveType::from_path(Path::new("test.txt")), None);
    }

    /// Round-trip a real 7z through the reader path. This exercises the
    /// sevenz-rust2 API our adapter depends on (ArchiveReader::open,
    /// for_each_entries yielding a readable stream, entry name/size), which
    /// type-checking alone can't confirm.
    #[test]
    fn test_hash_7z_entries_roundtrip() {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
        use std::io::Cursor;

        let temp = tempfile::TempDir::new().unwrap();
        let archive_path = temp.path().join("roms.7z");

        let content = b"7z rom content";
        {
            let mut writer = ArchiveWriter::create(&archive_path).unwrap();
            let entry = ArchiveEntry {
                name: "game.rom".to_string(),
                has_stream: true,
                ..Default::default()
            };
            writer
                .push_archive_entry(entry, Some(Cursor::new(content.to_vec())))
                .unwrap();
            writer.finish().unwrap();
        }

        let entries = hash_7z_entries(&archive_path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "game.rom");
        assert_eq!(entries[0].size, content.len() as u64);

        let hashes = entries[0].hashes.as_ref().expect("entry was hashed");
        // SHA1 of "7z rom content"
        assert_eq!(hashes.sha1, "76BF6FA80E58B8D8263A0663FBC189441AD2C30D");
    }

    /// The native (sevenz-rust2) fallback must stay correct even on machines
    /// where the system 7z binary is present and the dispatcher prefers it.
    /// Asserting the same SHA1 as the dispatcher test also proves the two
    /// decode paths agree byte-for-byte.
    #[test]
    fn test_hash_7z_entries_native_fallback() {
        use sevenz_rust2::{ArchiveEntry as SzEntry, ArchiveWriter};
        use std::io::Cursor;

        let temp = tempfile::TempDir::new().unwrap();
        let archive_path = temp.path().join("roms.7z");
        let content = b"7z rom content";
        {
            let mut writer = ArchiveWriter::create(&archive_path).unwrap();
            let entry = SzEntry {
                name: "game.rom".to_string(),
                has_stream: true,
                ..Default::default()
            };
            writer
                .push_archive_entry(entry, Some(Cursor::new(content.to_vec())))
                .unwrap();
            writer.finish().unwrap();
        }

        let entries = hash_7z_entries_native(&archive_path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "game.rom");
        let hashes = entries[0].hashes.as_ref().expect("hashed");
        assert_eq!(hashes.sha1, "76BF6FA80E58B8D8263A0663FBC189441AD2C30D");
    }
}
