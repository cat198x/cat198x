//! Catalogue and extraction must agree on names across ZIP dependency upgrades.
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use cat198x::archive::{ZipWriter, ZipWriterOptions};
use cat198x::scanner::{hash_archive_entries, list_archive_entries};
use tempfile::TempDir;
use zip::write::FullFileOptions;

fn fixture(
    path: &Path,
    raw_name: &[u8; 5],
    utf8: bool,
    unicode: Option<(&str, bool)>,
) -> Result<()> {
    let mut options = FullFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    if let Some((name, valid_crc)) = unicode {
        let mut extra = vec![1];
        let crc = crc32fast::hash(raw_name) ^ u32::from(!valid_crc);
        extra.extend_from_slice(&crc.to_le_bytes());
        extra.extend_from_slice(name.as_bytes());
        options.add_extra_field(0x7075, extra, false)?;
    }
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("a.bin", options)?;
    writer.write_all(b"payload")?;
    let mut bytes = writer.finish()?.into_inner();
    let central = bytes
        .windows(4)
        .position(|b| b == b"PK\x01\x02")
        .context("fixture central directory")?;
    bytes[30..35].copy_from_slice(raw_name);
    bytes[central + 46..central + 51].copy_from_slice(raw_name);
    for offset in [7, central + 9] {
        bytes[offset] = (bytes[offset] & !0x08) | if utf8 { 0x08 } else { 0 };
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn assert_scan_and_repack(path: &Path, expected: &str, output: &Path) -> Result<()> {
    let listed = list_archive_entries(path)?;
    let hashed = hash_archive_entries(path)?;
    assert_eq!(listed.len(), 1);
    assert_eq!(hashed.len(), 1);
    assert_eq!(listed[0].name, expected);
    assert_eq!(hashed[0].name, expected);
    let mut writer = ZipWriter::new(output, ZipWriterOptions::default())?;
    writer.add_from_archive(expected, path, &hashed[0].name)?;
    writer.finish()?;
    let mut archive = zip::ZipArchive::new(fs::File::open(output)?)?;
    let mut entry = archive.by_index(0)?;
    assert_eq!(entry.name()?, expected);
    let mut data = Vec::new();
    entry.read_to_end(&mut data)?;
    assert_eq!(data, b"payload");
    Ok(())
}

#[test]
fn ambiguous_cp437_and_utf8_names_survive_scan_and_repack() {
    let temp = TempDir::new().unwrap();
    for (index, utf8, expected) in [(0, false, "├⌐bin"), (1, true, "ébin")] {
        let input = temp.path().join(format!("input-{index}.zip"));
        fixture(&input, b"\xc3\xa9bin", utf8, None).unwrap();
        assert_scan_and_repack(
            &input,
            expected,
            &temp.path().join(format!("output-{index}.zip")),
        )
        .unwrap();
    }
}

#[test]
fn unicode_path_override_requires_matching_original_name_crc() {
    let temp = TempDir::new().unwrap();
    for (index, valid, expected) in [(0, true, "日本.bin"), (1, false, "├⌐bin")] {
        let input = temp.path().join(format!("input-{index}.zip"));
        fixture(&input, b"\xc3\xa9bin", false, Some(("日本.bin", valid))).unwrap();
        assert_scan_and_repack(
            &input,
            expected,
            &temp.path().join(format!("output-{index}.zip")),
        )
        .unwrap();
    }
}

#[test]
fn malformed_flagged_utf8_keeps_catalogued_replacement_character() {
    let temp = TempDir::new().unwrap();
    let input = temp.path().join("input.zip");
    fixture(&input, b"\x86.bin", true, None).unwrap();
    assert_scan_and_repack(&input, "�.bin", &temp.path().join("output.zip")).unwrap();
}

#[test]
fn unreadable_zip_entry_is_an_error_before_repacking() {
    let temp = TempDir::new().unwrap();
    let input = temp.path().join("input.zip");
    fixture(&input, b"a.bin", false, None).unwrap();
    let mut bytes = fs::read(&input).unwrap();
    bytes[0] = 0; // Central directory remains valid; the local header is corrupt.
    fs::write(&input, &bytes).unwrap();
    assert!(list_archive_entries(&input).is_err());
    assert!(hash_archive_entries(&input).is_err());
    let mut writer =
        ZipWriter::new(&temp.path().join("output.zip"), ZipWriterOptions::default()).unwrap();
    let error = writer
        .add_from_archive("a.bin", &input, "a.bin")
        .unwrap_err();
    assert!(!error.to_string().contains("Entry not found"), "{error:#}");
    assert_eq!(fs::read(&input).unwrap(), bytes);
}
