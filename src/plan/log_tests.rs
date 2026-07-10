use super::*;
use tempfile::TempDir;

#[test]
fn test_operation_log_new() {
    let log = OperationLog::new("abc123".to_string());
    assert_eq!(log.plan_hash, "abc123");
    assert!(log.entries.is_empty());
    assert!(log.completed_at.is_none());
}

#[test]
fn test_log_copy_success() {
    let mut log = OperationLog::new("abc123".to_string());
    log.log_copy(1, "/src/file.rom", "/dest/file.rom", "sha1hash", true);

    assert_eq!(log.entries.len(), 1);
    assert_eq!(log.entries[0].status, LogStatus::Completed);
    assert!(log.entries[0].reverse.is_some());
}

#[test]
fn test_log_copy_failure() {
    let mut log = OperationLog::new("abc123".to_string());
    log.log_copy(1, "/src/file.rom", "/dest/file.rom", "sha1hash", false);

    assert_eq!(log.entries.len(), 1);
    assert_eq!(log.entries[0].status, LogStatus::Failed);
    assert!(log.entries[0].reverse.is_none());
}

#[test]
fn test_log_move_has_reverse_move() {
    let mut log = OperationLog::new("abc123".to_string());
    log.log_move(1, "/src/file.rom", "/dest/file.rom", "sha1hash", true);

    let reverse = log.entries[0].reverse.as_ref().unwrap();
    match reverse {
        LoggedOperation::Move { source, dest, .. } => {
            assert_eq!(source, "/dest/file.rom");
            assert_eq!(dest, "/src/file.rom");
        }
        _ => panic!("Expected Move reverse operation"),
    }
}

#[test]
fn test_log_quarantine_reverses_with_move_back() {
    let mut log = OperationLog::new("abc123".to_string());
    log.log_quarantine(
        1,
        "/roms/game.rom",
        "/data/quarantine/h_game.rom",
        "HASH",
        true,
    );

    // The reverse of a quarantine restores the original from the store.
    match log.entries[0].reverse.as_ref().unwrap() {
        LoggedOperation::Move { source, dest, sha1 } => {
            assert_eq!(source, "/data/quarantine/h_game.rom");
            assert_eq!(dest, "/roms/game.rom");
            assert_eq!(sha1, "HASH");
        }
        other => panic!("expected Move reverse, got {:?}", other),
    }
    assert_eq!(log.entries[0].status, LogStatus::Completed);
}

#[test]
fn test_log_save_and_load() {
    let temp = TempDir::new().unwrap();
    let logs_dir = temp.path().join("logs");

    let mut log = OperationLog::new("abc12345".to_string());
    log.log_copy(1, "/src/a.rom", "/dest/a.rom", "hash1", true);
    log.log_copy(2, "/src/b.rom", "/dest/b.rom", "hash2", false);
    log.complete();

    let path = log.save(&logs_dir).unwrap();
    assert!(path.exists());

    let loaded = OperationLog::load(&path).unwrap();
    assert_eq!(loaded.plan_hash, "abc12345");
    assert_eq!(loaded.entries.len(), 2);
    assert!(loaded.completed_at.is_some());
}

#[test]
fn test_success_and_failed_counts() {
    let mut log = OperationLog::new("abc123".to_string());
    log.log_copy(1, "/src/a.rom", "/dest/a.rom", "hash1", true);
    log.log_copy(2, "/src/b.rom", "/dest/b.rom", "hash2", true);
    log.log_copy(3, "/src/c.rom", "/dest/c.rom", "hash3", false);

    assert_eq!(log.success_count(), 2);
    assert_eq!(log.failed_count(), 1);
}

#[test]
fn test_log_repack_success() {
    let mut log = OperationLog::new("abc123".to_string());
    log.log_repack(
        1,
        &["/src/a.rom".to_string(), "/src/b.rom".to_string()],
        "/dest/game.zip",
        &[],
        true,
    );

    assert_eq!(log.entries.len(), 1);
    assert_eq!(log.entries[0].status, LogStatus::Completed);

    // Reverse of a copy-mode repack is delete
    let reverse = log.entries[0].reverse.as_ref().unwrap();
    match reverse {
        LoggedOperation::Delete { path } => {
            assert_eq!(path, "/dest/game.zip");
        }
        _ => panic!("Expected Delete reverse operation"),
    }
}

#[test]
fn test_log_repack_move_reverse_restores_sources() {
    let mut log = OperationLog::new("abc123".to_string());
    log.log_repack(
        1,
        &["/src/a.rom".to_string()],
        "/dest/game.zip",
        &[("a.rom".to_string(), "/src/a.rom".to_string())],
        true,
    );

    // A move-mode repack consumed its loose source, so the reverse restores
    // it out of the archive (rather than only deleting the archive).
    let reverse = log.entries[0].reverse.as_ref().unwrap();
    match reverse {
        LoggedOperation::UnpackRepack { dest, restore } => {
            assert_eq!(dest, "/dest/game.zip");
            assert_eq!(restore, &[("a.rom".to_string(), "/src/a.rom".to_string())]);
        }
        _ => panic!("Expected UnpackRepack reverse operation"),
    }
}

#[test]
fn test_log_repack_failure() {
    let mut log = OperationLog::new("abc123".to_string());
    log.log_repack(1, &["/src/a.rom".to_string()], "/dest/game.zip", &[], false);

    assert_eq!(log.entries.len(), 1);
    assert_eq!(log.entries[0].status, LogStatus::Failed);
    assert!(log.entries[0].reverse.is_none());
}
