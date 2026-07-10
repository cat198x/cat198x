use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use crate::plan::{OperationKind, OperationStatus, Plan};
use crate::util::format_bytes;

/// Check there's enough free space for everything the plan will write.
///
/// Counts only genuinely new bytes, grouped by destination volume. A same-volume
/// `Move` or `Relocate` is a rename — it frees as much as it consumes, so it
/// needs no space; only copies, cross-volume moves, and the transient archive a
/// repack builds count. This matters for a `--move` in-place tidy, where the
/// moves dominate the byte total yet need no space at all.
///
/// Bucketing by volume (not per destination directory) also keeps this to one
/// free-space query per volume instead of thousands of `stat`s over a network
/// mount, and the repack size comes from the plan rather than from stat-ing
/// every source file.
pub fn check_disk_space(plan: &Plan) -> Result<()> {
    let mut bytes_by_volume: HashMap<String, u64> = HashMap::new();

    for op in &plan.operations {
        if op.status != OperationStatus::Pending {
            continue;
        }

        let (dest, needed): (&str, u64) = match &op.kind {
            OperationKind::Copy { dest, size, .. } => (dest, *size),
            OperationKind::Move {
                source, dest, size, ..
            } => (
                dest,
                if same_volume(&source.path, dest) {
                    0
                } else {
                    *size
                },
            ),
            OperationKind::Relocate { source, dest, size } => {
                (dest, if same_volume(source, dest) { 0 } else { *size })
            }
            // A repack builds a new archive at dest; while it is written its
            // sources still exist (move-mode deletion happens only after the
            // archive verifies), so the transient peak is the archive size.
            OperationKind::Repack { dest, size, .. } => (dest, *size),
            // Deletes free space.
            OperationKind::Delete { .. } => continue,
            // Quarantine writes into the data dir, a separate space concern from
            // the library volume — not checked here.
            OperationKind::Quarantine { .. } => continue,
        };

        if needed == 0 {
            continue;
        }
        *bytes_by_volume.entry(volume_root(dest)).or_insert(0) += needed;
    }

    for (volume, bytes_needed) in &bytes_by_volume {
        let available = get_available_space(volume)?;

        // Add 10% safety margin
        let bytes_with_margin = (*bytes_needed as f64 * 1.1) as u64;

        if available < bytes_with_margin {
            anyhow::bail!(
                "Insufficient space on '{}': need {} (with 10% margin), have {}",
                volume,
                format_bytes(bytes_with_margin),
                format_bytes(available)
            );
        }
    }

    Ok(())
}

/// The volume root of an absolute path — `/Volumes/<name>` for a mounted volume,
/// otherwise `/`. A string test, so it costs no `stat` over a network mount.
/// A nested mount under `/Volumes/<name>` is treated as the same volume; the
/// library is one tree per volume, so this is exact in practice and only ever
/// conservative (it never under-reserves space).
fn volume_root(path: &str) -> String {
    let mut comps = path.trim_start_matches('/').split('/');
    match (comps.next(), comps.next()) {
        (Some("Volumes"), Some(name)) if !name.is_empty() => format!("/Volumes/{name}"),
        _ => "/".to_string(),
    }
}

/// Whether two paths live on the same volume, so a move between them is a rename
/// that needs no new space.
fn same_volume(a: &str, b: &str) -> bool {
    volume_root(a) == volume_root(b)
}

/// Get available disk space for a path (in bytes)
fn get_available_space(path: &str) -> Result<u64> {
    // Find an existing parent directory to stat — the destination itself may
    // not exist yet (we're about to create it).
    let mut check_path = Path::new(path).to_path_buf();
    while !check_path.exists() {
        check_path = match check_path.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
    }

    // fs4 wraps statvfs / GetDiskFreeSpaceExW, returning the space available to
    // non-privileged users (matching the old f_bavail-based result) with no
    // unsafe FFI on our side, so the crate keeps unsafe_code = "forbid".
    fs4::available_space(&check_path)
        .with_context(|| format!("Failed to get disk space for '{}'", path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{CopyPlacement, Operation, SourceRef};

    #[test]
    fn get_available_space_exists() {
        // Test with a path that definitely exists
        let result = get_available_space("/tmp");
        assert!(result.is_ok());
        // Should return something > 0
        assert!(result.unwrap() > 0);
    }

    #[test]
    fn get_available_space_nonexistent_finds_parent() {
        // Test with a path that doesn't exist but has existing parent
        let result = get_available_space("/tmp/nonexistent_dir_12345/nested");
        assert!(result.is_ok());
        // Should return something > 0 (falls back to /tmp)
        assert!(result.unwrap() > 0);
    }

    #[test]
    fn check_disk_space_empty_plan() {
        let plan = Plan::new("test".to_string());
        // Empty plan should always pass
        assert!(check_disk_space(&plan).is_ok());
    }

    #[test]
    fn check_disk_space_small_operations() {
        let mut plan = Plan::new("test".to_string());

        // Add a small copy operation to /tmp (which should have space)
        plan.operations.push(Operation {
            id: 0,
            status: OperationStatus::Pending,
            kind: OperationKind::Copy {
                source: SourceRef {
                    path: "/source/file.rom".to_string(),
                    archive_path: None,
                    sha1: "ABC123".to_string(),
                    entry_name: None,
                },
                dest: "/tmp/test_dest/file.rom".to_string(),
                size: 1024, // 1 KB
                placement: CopyPlacement::LooseFile,
            },
        });

        // Small operation should pass
        assert!(check_disk_space(&plan).is_ok());
    }

    #[test]
    fn volume_root_and_same_volume() {
        assert_eq!(
            volume_root("/Volumes/Data/Library/ROMs/x.zip"),
            "/Volumes/Data"
        );
        assert_eq!(volume_root("/Volumes/Data"), "/Volumes/Data");
        assert_eq!(volume_root("/Users/me/roms/x.zip"), "/");
        assert_eq!(volume_root("/"), "/");
        // ToSort and Library on the same volume compare equal.
        assert!(same_volume(
            "/Volumes/Data/ToSort/MAME/g.zip",
            "/Volumes/Data/Library/ROMs/MAME/g.zip"
        ));
        // Different volumes do not.
        assert!(!same_volume("/Volumes/Data/x.zip", "/Volumes/Backup/x.zip"));
    }

    #[test]
    fn check_disk_space_ignores_same_volume_moves() {
        // A same-volume move is a rename — it needs no space, however large. Were
        // it counted, this u64::MAX move would fail the check; it must pass.
        let mut plan = Plan::new("h".to_string());
        let src = SourceRef {
            path: "/Volumes/Data/ToSort/big.bin".to_string(),
            archive_path: None,
            sha1: "a".to_string(),
            entry_name: None,
        };
        plan.add_move(src, "/Volumes/Data/Library/big.bin".to_string(), u64::MAX);
        assert!(check_disk_space(&plan).is_ok());
    }

    #[test]
    fn check_disk_space_counts_cross_volume_moves() {
        // A cross-volume move genuinely needs space at the destination, so an
        // impossible u64::MAX move must be refused.
        let mut plan = Plan::new("h".to_string());
        let src = SourceRef {
            path: "/Volumes/Data/big.bin".to_string(),
            archive_path: None,
            sha1: "a".to_string(),
            entry_name: None,
        };
        plan.add_move(src, "/Volumes/Backup/big.bin".to_string(), u64::MAX);
        assert!(check_disk_space(&plan).is_err());
    }
}
