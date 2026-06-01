//! Archive reading utilities (ZIP, 7Z)

use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;

use super::hasher::{hash_reader, FileHashes};

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
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("Failed to open ZIP: {:?}", path))?;

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
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("Failed to open ZIP: {:?}", path))?;

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

fn hash_7z_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
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
}
