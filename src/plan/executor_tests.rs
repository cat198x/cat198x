use super::*;
use crate::plan::CopyPlacement;
use crate::util::{format_bytes, truncate_path, verify_sha1};
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

#[test]
fn test_truncate_path_short() {
    assert_eq!(truncate_path("/short/path", 50), "/short/path");
}

#[test]
fn test_truncate_path_long() {
    let long = "/very/long/path/that/exceeds/the/maximum/length/allowed";
    let truncated = truncate_path(long, 30);
    assert!(truncated.starts_with("..."));
    assert_eq!(truncated.len(), 30);
}

#[test]
fn test_execute_copy_loose_file() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("source.rom");
    let dest_path = temp.path().join("dest/output.rom");

    // Create source file
    let mut src = fs::File::create(&src_path).unwrap();
    src.write_all(b"test rom content").unwrap();

    // SHA1 of "test rom content" = 331407B2BD72286D458F26C426D78F459D7116D3
    let expected_sha1 = "331407B2BD72286D458F26C426D78F459D7116D3";

    // Execute copy with verification
    execute_copy(
        src_path.to_str().unwrap(),
        None,
        dest_path.to_str().unwrap(),
        expected_sha1,
        &CopyPlacement::LooseFile,
    )
    .unwrap();

    // Verify destination exists with correct content
    let content = fs::read(&dest_path).unwrap();
    assert_eq!(content, b"test rom content");
}

#[test]
fn test_execute_copy_verification_fails_on_bad_hash() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("source.rom");
    let dest_path = temp.path().join("dest/output.rom");

    // Create source file
    let mut src = fs::File::create(&src_path).unwrap();
    src.write_all(b"test rom content").unwrap();

    // Wrong SHA1
    let wrong_sha1 = "0000000000000000000000000000000000000000";

    // Execute copy should fail verification
    let result = execute_copy(
        src_path.to_str().unwrap(),
        None,
        dest_path.to_str().unwrap(),
        wrong_sha1,
        &CopyPlacement::LooseFile,
    );

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Verification failed")
    );

    // Bad file should be removed
    assert!(!dest_path.exists());
}

#[test]
fn test_verify_sha1() {
    let temp = TempDir::new().unwrap();
    let file_path = temp.path().join("test.rom");

    // Create file with known content
    fs::write(&file_path, b"hello").unwrap();

    // SHA1 of "hello" = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
    assert!(verify_sha1(&file_path, "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D").unwrap());
    assert!(!verify_sha1(&file_path, "0000000000000000000000000000000000000000").unwrap());
}

#[test]
fn test_execute_copy_to_zip_output() {
    use std::io::Read;

    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("source.rom");
    let dest_path = temp.path().join("output.zip");

    // Create source file
    let mut src = fs::File::create(&src_path).unwrap();
    src.write_all(b"test rom content").unwrap();

    // SHA1 of "test rom content" = 331407B2BD72286D458F26C426D78F459D7116D3
    let expected_sha1 = "331407B2BD72286D458F26C426D78F459D7116D3";

    // Execute copy to an explicit ZIP entry destination.
    execute_copy(
        src_path.to_str().unwrap(),
        None,
        dest_path.to_str().unwrap(),
        expected_sha1,
        &CopyPlacement::ZipEntry {
            entry_name: "source.rom".to_string(),
        },
    )
    .unwrap();

    // Verify ZIP was created
    assert!(dest_path.exists());

    // Verify ZIP contains the file with correct content
    let file = fs::File::open(&dest_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    assert_eq!(archive.len(), 1);

    let mut entry = archive.by_name("source.rom").unwrap();
    let mut content = Vec::new();
    entry.read_to_end(&mut content).unwrap();
    assert_eq!(content, b"test rom content");
}

#[test]
fn loose_copy_to_zip_named_file_writes_bytes_not_archive() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("source.rom");
    let dest_path = temp.path().join("loose-name.zip");
    fs::write(&src_path, b"test rom content").unwrap();

    execute_copy(
        src_path.to_str().unwrap(),
        None,
        dest_path.to_str().unwrap(),
        "331407B2BD72286D458F26C426D78F459D7116D3",
        &CopyPlacement::LooseFile,
    )
    .unwrap();

    assert_eq!(fs::read(&dest_path).unwrap(), b"test rom content");
    let file = fs::File::open(&dest_path).unwrap();
    assert!(
        zip::ZipArchive::new(file).is_err(),
        "a loose file named .zip must not be wrapped as a ZIP archive"
    );
}

#[test]
fn test_execute_copy_to_zip_bad_hash() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("source.rom");
    let dest_path = temp.path().join("output.zip");

    // Create source file
    let mut src = fs::File::create(&src_path).unwrap();
    src.write_all(b"test rom content").unwrap();

    // Wrong SHA1
    let wrong_sha1 = "0000000000000000000000000000000000000000";

    // Execute copy should fail verification
    let result = execute_copy(
        src_path.to_str().unwrap(),
        None,
        dest_path.to_str().unwrap(),
        wrong_sha1,
        &CopyPlacement::ZipEntry {
            entry_name: "source.rom".to_string(),
        },
    );

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Verification failed")
    );

    // Bad ZIP should be removed
    assert!(!dest_path.exists());
}

#[test]
fn test_execute_copy_to_zip_creates_parent_dirs() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("source.rom");
    let dest_path = temp.path().join("nested/dirs/output.zip");

    // Create source file
    fs::write(&src_path, b"hello").unwrap();

    // SHA1 of "hello" = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
    let expected_sha1 = "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D";

    // Execute copy to nested ZIP destination.
    execute_copy(
        src_path.to_str().unwrap(),
        None,
        dest_path.to_str().unwrap(),
        expected_sha1,
        &CopyPlacement::ZipEntry {
            entry_name: "source.rom".to_string(),
        },
    )
    .unwrap();

    // Verify ZIP was created in nested directory
    assert!(dest_path.exists());
}

#[test]
fn test_execute_repack_multiple_files() {
    use crate::plan::SourceRef;
    use std::io::Read;

    let temp = TempDir::new().unwrap();

    // Create source files
    let src1 = temp.path().join("cpu.rom");
    let src2 = temp.path().join("gfx.rom");
    fs::write(&src1, b"cpu data").unwrap();
    fs::write(&src2, b"graphics data").unwrap();

    // SHA1 of "cpu data" = 7D3A7E2E4F5B8C1D9E0F1A2B3C4D5E6F7A8B9C0D (wrong - calculate real)
    // SHA1 of "graphics data" = ... (calculate real)
    let dest_path = temp.path().join("game.zip");

    let sources = vec![
        SourceRef {
            path: src1.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: "76218C22675632AEF6A27578DD0A2C6471D995D5".to_string(), // SHA1 of "cpu data"
            entry_name: None,
        },
        SourceRef {
            path: src2.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: "75BF07C00E138F33E12904F575641F0C06CBB838".to_string(), // SHA1 of "graphics data"
            entry_name: None,
        },
    ];

    execute_repack(&sources, dest_path.to_str().unwrap(), "zip", false).unwrap();

    // Verify ZIP was created with both files
    assert!(dest_path.exists());

    let file = fs::File::open(&dest_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    assert_eq!(archive.len(), 2);

    // Verify content of first file
    let mut entry1 = archive.by_name("cpu.rom").unwrap();
    let mut content1 = Vec::new();
    entry1.read_to_end(&mut content1).unwrap();
    assert_eq!(content1, b"cpu data");

    // Verify content of second file
    drop(entry1);
    let mut entry2 = archive.by_name("gfx.rom").unwrap();
    let mut content2 = Vec::new();
    entry2.read_to_end(&mut content2).unwrap();
    assert_eq!(content2, b"graphics data");
}

#[test]
fn execute_repack_dedupes_duplicate_entry_names() {
    use crate::plan::SourceRef;
    use std::io::Read;

    let temp = TempDir::new().unwrap();
    let src = temp.path().join("data.rom");
    fs::write(&src, b"cpu data").unwrap();
    let dest_path = temp.path().join("game.zip");

    // The same entry name appears twice in the source set (an entry matched
    // via two locations). A ZIP can't hold a duplicate name, so the repack
    // must collapse them rather than abort with "Duplicate filename".
    let one = SourceRef {
        path: src.to_str().unwrap().to_string(),
        archive_path: None,
        sha1: "76218C22675632AEF6A27578DD0A2C6471D995D5".to_string(), // "cpu data"
        entry_name: Some("game.rom".to_string()),
    };
    let sources = vec![one.clone(), one];

    execute_repack(&sources, dest_path.to_str().unwrap(), "zip", false).unwrap();

    let file = fs::File::open(&dest_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    assert_eq!(archive.len(), 1, "duplicate entry name collapsed to one");
    let mut entry = archive.by_name("game.rom").unwrap();
    let mut content = Vec::new();
    entry.read_to_end(&mut content).unwrap();
    assert_eq!(content, b"cpu data");
}

#[test]
fn test_execute_repack_verification_failure() {
    use crate::plan::SourceRef;

    let temp = TempDir::new().unwrap();

    // Create source files
    let src1 = temp.path().join("good.rom");
    let src2 = temp.path().join("bad.rom");
    fs::write(&src1, b"good").unwrap();
    fs::write(&src2, b"bad").unwrap();

    let dest_path = temp.path().join("game.zip");

    let sources = vec![
        SourceRef {
            path: src1.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: "FC19318DD13128CE14344D066510A982269C241B".to_string(), // SHA1 of "good"
            entry_name: None,
        },
        SourceRef {
            path: src2.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: "0000000000000000000000000000000000000000".to_string(), // Wrong hash
            entry_name: None,
        },
    ];

    let result = execute_repack(&sources, dest_path.to_str().unwrap(), "zip", false);

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("verification failed")
    );

    // Bad ZIP should be removed
    assert!(!dest_path.exists());
}

#[test]
fn test_execute_repack_unsupported_format() {
    use crate::plan::SourceRef;

    let temp = TempDir::new().unwrap();
    let src = temp.path().join("file.rom");
    fs::write(&src, b"data").unwrap();

    let dest_path = temp.path().join("game.rar");

    let sources = vec![SourceRef {
        path: src.to_str().unwrap().to_string(),
        archive_path: None,
        sha1: "A17C9AAA61E80A1BF71D0D850AF4E5BAA9800BBD".to_string(), // SHA1 of "data"
        entry_name: None,
    }];

    let result = execute_repack(&sources, dest_path.to_str().unwrap(), "rar", false);

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Unsupported repack format")
    );
}

#[test]
fn test_execute_repack_torrentzip_format() {
    use crate::plan::SourceRef;
    use sha1::Digest;
    use std::fs::File;

    let temp = TempDir::new().unwrap();

    // Create source files (added in reverse alphabetical order)
    let src1 = temp.path().join("z_last.rom");
    let src2 = temp.path().join("a_first.rom");
    fs::write(&src1, b"z data").unwrap();
    fs::write(&src2, b"a data").unwrap();

    let dest_path = temp.path().join("game.zip");

    // Compute actual SHA1 values
    let sha1_z = crate::util::hex_upper(sha1::Sha1::digest(b"z data"));
    let sha1_a = crate::util::hex_upper(sha1::Sha1::digest(b"a data"));

    let sources = vec![
        SourceRef {
            path: src1.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: sha1_z,
            entry_name: None,
        },
        SourceRef {
            path: src2.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: sha1_a,
            entry_name: None,
        },
    ];

    execute_repack(&sources, dest_path.to_str().unwrap(), "torrentzip", false).unwrap();

    // Verify the ZIP was created with sorted entries
    let file = File::open(&dest_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    assert_eq!(archive.len(), 2);

    // Entries should be sorted alphabetically (a_first before z_last)
    let first_name = archive.by_index(0).unwrap().name().to_string();
    let second_name = archive.by_index(1).unwrap().name().to_string();
    assert_eq!(first_name, "a_first.rom");
    assert_eq!(second_name, "z_last.rom");

    // Verify timestamp is TorrentZIP standard
    let file = File::open(&dest_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let entry = archive.by_index(0).unwrap();
    let datetime = entry
        .last_modified()
        .expect("entry has a last-modified time");
    assert_eq!(datetime.year(), 1996);
    assert_eq!(datetime.month(), 12);
    assert_eq!(datetime.day(), 24);
}

#[test]
fn test_format_bytes() {
    assert_eq!(format_bytes(0), "0 bytes");
    assert_eq!(format_bytes(512), "512 bytes");
    assert_eq!(format_bytes(1024), "1.00 KB");
    assert_eq!(format_bytes(1536), "1.50 KB");
    assert_eq!(format_bytes(1048576), "1.00 MB");
    assert_eq!(format_bytes(1073741824), "1.00 GB");
}

#[test]
fn test_execute_rollback_move_success() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("current.rom");
    let dest_path = temp.path().join("original/file.rom");

    // Create source file (current location after apply)
    fs::write(&src_path, b"hello").unwrap();

    // SHA1 of "hello" = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
    let expected_sha1 = "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D";

    // Execute rollback move
    execute_rollback_move(
        src_path.to_str().unwrap(),
        dest_path.to_str().unwrap(),
        expected_sha1,
    )
    .unwrap();

    // Verify file moved to destination
    assert!(!src_path.exists());
    assert!(dest_path.exists());
    assert_eq!(fs::read(&dest_path).unwrap(), b"hello");
}

#[test]
fn test_execute_rollback_move_hash_mismatch() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("current.rom");
    let dest_path = temp.path().join("original/file.rom");

    // Create source file
    fs::write(&src_path, b"hello").unwrap();

    // Wrong SHA1
    let wrong_sha1 = "0000000000000000000000000000000000000000";

    // Execute rollback move - should fail
    let result = execute_rollback_move(
        src_path.to_str().unwrap(),
        dest_path.to_str().unwrap(),
        wrong_sha1,
    );

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("hash mismatch"));

    // Source file should still exist (not moved)
    assert!(src_path.exists());
    assert!(!dest_path.exists());
}

#[test]
fn test_execute_rollback_move_source_not_found() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("nonexistent.rom");
    let dest_path = temp.path().join("dest.rom");

    let result = execute_rollback_move(
        src_path.to_str().unwrap(),
        dest_path.to_str().unwrap(),
        "somehash",
    );

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Source file not found")
    );
}

#[test]
fn test_execute_move_loose_file() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("source.rom");
    let dest_path = temp.path().join("dest/moved.rom");

    // Create source file
    fs::write(&src_path, b"test rom content").unwrap();

    // SHA1 of "test rom content" = 331407B2BD72286D458F26C426D78F459D7116D3
    let expected_sha1 = "331407B2BD72286D458F26C426D78F459D7116D3";

    // Execute move
    execute_move(
        src_path.to_str().unwrap(),
        None,
        dest_path.to_str().unwrap(),
        expected_sha1,
        &CopyPlacement::LooseFile,
    )
    .unwrap();

    // Source should be deleted, destination should exist
    assert!(!src_path.exists());
    assert!(dest_path.exists());
    assert_eq!(fs::read(&dest_path).unwrap(), b"test rom content");
}

#[test]
fn test_execute_move_from_archive_keeps_source() {
    let temp = TempDir::new().unwrap();

    // Create a ZIP archive with a file
    let zip_path = temp.path().join("source.zip");
    {
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("test.rom", options).unwrap();
        std::io::Write::write_all(&mut zip, b"hello").unwrap();
        zip.finish().unwrap();
    }

    let dest_path = temp.path().join("dest/extracted.rom");

    // SHA1 of "hello" = AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D
    let expected_sha1 = "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D";

    // Execute move from archive
    execute_move(
        zip_path.to_str().unwrap(),
        Some("test.rom"),
        dest_path.to_str().unwrap(),
        expected_sha1,
        &CopyPlacement::LooseFile,
    )
    .unwrap();

    // Archive should still exist (we can't delete from inside it)
    assert!(zip_path.exists());
    // Destination should exist with extracted content
    assert!(dest_path.exists());
    assert_eq!(fs::read(&dest_path).unwrap(), b"hello");
}

#[test]
fn test_execute_move_same_fs_renames_without_rehashing() {
    let temp = TempDir::new().unwrap();
    let src_path = temp.path().join("source.rom");
    let dest_path = temp.path().join("dest/moved.rom");
    fs::write(&src_path, b"test rom content").unwrap();

    // A same-filesystem loose move is a pure rename that trusts the
    // catalogue's recorded hash: it does not re-read the file to verify it
    // first. A rename preserves the bytes exactly, so even a hash that does
    // not match the content still moves it. This guards the performance fix
    // that turned re-reading every ROM over a network mount back into a
    // metadata-only rename.
    let wrong_sha1 = "0000000000000000000000000000000000000000";
    execute_move(
        src_path.to_str().unwrap(),
        None,
        dest_path.to_str().unwrap(),
        wrong_sha1,
        &CopyPlacement::LooseFile,
    )
    .unwrap();

    assert!(!src_path.exists(), "source renamed away");
    assert_eq!(fs::read(&dest_path).unwrap(), b"test rom content");
}

#[test]
fn execute_relocate_moves_whole_file_unchanged() {
    let temp = TempDir::new().unwrap();
    let src = temp.path().join("ToSort/SET/Game.zip");
    fs::create_dir_all(src.parent().unwrap()).unwrap();
    fs::write(&src, b"a complete zip's bytes").unwrap();
    // Destination in a not-yet-existing nested dir (same filesystem → rename).
    let dest = temp.path().join("ROMs/SET/Sys/Game.zip");

    execute_relocate(src.to_str().unwrap(), dest.to_str().unwrap()).unwrap();

    assert!(!src.exists(), "source relocated away");
    assert!(dest.exists());
    assert_eq!(fs::read(&dest).unwrap(), b"a complete zip's bytes");
}

#[test]
fn execute_relocate_missing_source_errors() {
    let temp = TempDir::new().unwrap();
    let result = execute_relocate(
        temp.path().join("nope.zip").to_str().unwrap(),
        temp.path().join("dest.zip").to_str().unwrap(),
    );
    assert!(result.is_err());
}

#[test]
fn execute_repack_7z_writes_canonical_entries() {
    use crate::plan::SourceRef;

    let temp = TempDir::new().unwrap();
    // Source file with a non-canonical name; SHA1 of "cpu data".
    let src = temp.path().join("whatever-it-was-called.bin");
    fs::write(&src, b"cpu data").unwrap();
    let expected_sha1 = "76218C22675632AEF6A27578DD0A2C6471D995D5";

    let dest = temp.path().join("game.7z");
    let sources = vec![SourceRef {
        path: src.to_str().unwrap().to_string(),
        archive_path: None,
        sha1: expected_sha1.to_string(),
        entry_name: Some("canonical.rom".to_string()),
    }];

    execute_repack(&sources, dest.to_str().unwrap(), "7z", false).unwrap();
    assert!(dest.exists());

    // Read back: one entry, named canonically, with the right content hash.
    let entries = crate::scanner::archive::hash_archive_entries(&dest).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "canonical.rom");
    assert!(
        entries[0]
            .hashes
            .as_ref()
            .unwrap()
            .sha1
            .eq_ignore_ascii_case(expected_sha1)
    );
}

#[test]
fn execute_repacks_concurrent_runs_all_jobs() {
    use crate::plan::SourceRef;

    let temp = TempDir::new().unwrap();
    // SHA1 of "cpu data"
    let sha1 = "76218C22675632AEF6A27578DD0A2C6471D995D5";

    // More jobs than workers, so the queue actually round-robins.
    let jobs: Vec<RepackJob> = (0..10)
        .map(|i| {
            let src = temp.path().join(format!("src-{i}.rom"));
            fs::write(&src, b"cpu data").unwrap();
            RepackJob {
                plan_index: i,
                operation_id: i as u64 + 100,
                sources: vec![SourceRef {
                    path: src.to_str().unwrap().to_string(),
                    archive_path: None,
                    sha1: sha1.to_string(),
                    entry_name: Some("game.rom".to_string()),
                }],
                dest: temp
                    .path()
                    .join(format!("game-{i}.zip"))
                    .to_str()
                    .unwrap()
                    .to_string(),
                format: "zip".to_string(),
                move_sources: false,
                size: 8,
            }
        })
        .collect();
    let dests: Vec<String> = jobs.iter().map(|j| j.dest.clone()).collect();

    // The callback mutates plain locals with no synchronisation — proof it
    // runs on the calling thread, the property apply relies on to keep the
    // journal and catalogue updates serial.
    let mut seen = Vec::new();
    let mut started = 0;
    execute_repacks_concurrent(jobs, 4, |event| match event {
        RepackEvent::Started { slot, .. } => {
            assert!(slot < 4);
            started += 1;
        }
        RepackEvent::Finished { outcome, .. } => {
            assert!(outcome.result.is_ok(), "{:?}", outcome.result.err());
            seen.push(outcome.job.plan_index);
        }
    });

    seen.sort_unstable();
    assert_eq!(started, 10, "every job started once");
    assert_eq!(seen, (0..10).collect::<Vec<_>>(), "every job reported once");
    for dest in dests {
        assert!(Path::new(&dest).exists(), "archive built: {dest}");
    }
}

#[test]
fn execute_repacks_concurrent_reports_failures_individually() {
    use crate::plan::SourceRef;

    let temp = TempDir::new().unwrap();
    let good_src = temp.path().join("good.rom");
    let bad_src = temp.path().join("bad.rom");
    fs::write(&good_src, b"cpu data").unwrap();
    fs::write(&bad_src, b"cpu data").unwrap();

    let make_job = |idx: usize, src: &Path, sha1: &str| RepackJob {
        plan_index: idx,
        operation_id: idx as u64,
        sources: vec![SourceRef {
            path: src.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: sha1.to_string(),
            entry_name: None,
        }],
        dest: temp
            .path()
            .join(format!("out-{idx}.zip"))
            .to_str()
            .unwrap()
            .to_string(),
        format: "zip".to_string(),
        move_sources: false,
        size: 8,
    };

    let jobs = vec![
        make_job(0, &good_src, "76218C22675632AEF6A27578DD0A2C6471D995D5"),
        make_job(1, &bad_src, "0000000000000000000000000000000000000000"),
    ];
    let good_dest = jobs[0].dest.clone();
    let bad_dest = jobs[1].dest.clone();

    let mut outcomes: Vec<(usize, bool)> = Vec::new();
    execute_repacks_concurrent(jobs, 2, |event| {
        if let RepackEvent::Finished { outcome, .. } = event {
            outcomes.push((outcome.job.plan_index, outcome.result.is_ok()));
        }
    });
    outcomes.sort_unstable();

    // One job failing verification doesn't take the batch down: the good
    // job still builds, the bad one reports Err and removed its partial.
    assert_eq!(outcomes, vec![(0, true), (1, false)]);
    assert!(Path::new(&good_dest).exists());
    assert!(!Path::new(&bad_dest).exists());
}

#[test]
fn execute_placements_concurrent_runs_all_and_reports_each() {
    use crate::plan::SourceRef;

    let temp = TempDir::new().unwrap();
    // sha1("cpu data") — every good copy verifies against this.
    let sha = "76218C22675632AEF6A27578DD0A2C6471D995D5";

    // Eight loose copies to distinct destinations, plus one whose expected
    // hash is wrong so it fails verification — concurrency must not let one
    // failure take the batch down, and every outcome must be reported once.
    let mut jobs = Vec::new();
    let mut good_dests = Vec::new();
    for i in 0..8 {
        let src = temp.path().join(format!("src-{i}.rom"));
        fs::write(&src, b"cpu data").unwrap();
        let dest = temp.path().join(format!("dst-{i}.rom"));
        good_dests.push(dest.clone());
        jobs.push(PlacementJob {
            plan_index: i,
            operation_id: i as u64,
            kind: PlacementKind::Copy {
                source: SourceRef {
                    path: src.to_str().unwrap().to_string(),
                    archive_path: None,
                    sha1: sha.to_string(),
                    entry_name: None,
                },
                dest: dest.to_str().unwrap().to_string(),
                placement: CopyPlacement::LooseFile,
            },
        });
    }
    let bad_src = temp.path().join("bad.rom");
    fs::write(&bad_src, b"cpu data").unwrap();
    let bad_dest = temp.path().join("bad-dst.rom");
    jobs.push(PlacementJob {
        plan_index: 8,
        operation_id: 8,
        kind: PlacementKind::Copy {
            source: SourceRef {
                path: bad_src.to_str().unwrap().to_string(),
                archive_path: None,
                sha1: "0000000000000000000000000000000000000000".to_string(),
                entry_name: None,
            },
            dest: bad_dest.to_str().unwrap().to_string(),
            placement: CopyPlacement::LooseFile,
        },
    });

    let mut outcomes: Vec<(usize, bool)> = Vec::new();
    let mut started = 0;
    execute_placements_concurrent(jobs, 4, |e| match e {
        PlacementEvent::Started { slot, .. } => {
            assert!(slot < 4, "slot is within the worker pool");
            started += 1;
        }
        PlacementEvent::Finished { outcome, .. } => {
            outcomes.push((outcome.job.plan_index, outcome.result.is_ok()));
        }
    });
    outcomes.sort_unstable();

    // Each job started and finished exactly once; the eight good copies
    // landed, the bad one failed and wrote nothing.
    assert_eq!(started, 9, "one start event per job");
    assert_eq!(outcomes.len(), 9);
    for (i, dest) in good_dests.iter().enumerate() {
        assert_eq!(outcomes[i], (i, true));
        assert_eq!(fs::read(dest).unwrap(), b"cpu data");
    }
    assert_eq!(outcomes[8], (8, false));
    assert!(!bad_dest.exists());
}

#[test]
fn execute_repack_7z_verifies_content_hash() {
    use crate::plan::SourceRef;

    let temp = TempDir::new().unwrap();
    let src = temp.path().join("a.bin");
    fs::write(&src, b"cpu data").unwrap();
    let dest = temp.path().join("bad.7z");

    // Wrong expected hash → repack fails and removes the partial archive.
    let sources = vec![SourceRef {
        path: src.to_str().unwrap().to_string(),
        archive_path: None,
        sha1: "0000000000000000000000000000000000000000".to_string(),
        entry_name: Some("x.rom".to_string()),
    }];

    assert!(execute_repack(&sources, dest.to_str().unwrap(), "7z", false).is_err());
    assert!(!dest.exists());
}
