//! Plan types for ROM operations

use serde::{Deserialize, Serialize};

/// A complete plan for reorganising ROMs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// State hash - invalidates plan if state changes
    pub state_hash: String,
    /// When the plan was generated
    pub created_at: String,
    /// Operations to execute
    pub operations: Vec<Operation>,
    /// Summary statistics
    pub summary: PlanSummary,
    /// Collections skipped because no destination could be resolved. Transient
    /// (not part of the persisted plan) — surfaced to the user after planning.
    #[serde(skip)]
    pub skipped_no_dest: Vec<String>,
    /// Collections skipped because their match expansion exceeds the memory-safe
    /// cap — a MAME-style meta-aggregate that lists content held across hundreds
    /// of files. Each entry is `"<name> (<rows> match-rows)"`. Transient.
    #[serde(skip)]
    pub skipped_oversized: Vec<String>,
    /// Per-collection operation tallies, for a reviewable breakdown. Persisted
    /// so a reader (the UI's pending-work overlay) can roll the to-write counts
    /// up the library tree without re-running the planner; `default` keeps plans
    /// written before it was saved loadable.
    #[serde(default)]
    pub per_collection: Vec<CollectionPlanStat>,
}

/// A single collection's contribution to a plan, for the by-group breakdown.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionPlanStat {
    pub name: String,
    /// The collection's library path (carries the set as its top segment).
    pub node_path: String,
    pub to_write: usize,
    pub already_correct: usize,
    pub bytes: u64,
}

/// Summary of planned operations
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanSummary {
    pub copy_count: usize,
    pub move_count: usize,
    pub repack_count: usize,
    pub delete_count: usize,
    #[serde(default)]
    pub quarantine_count: usize,
    pub already_correct: usize,
    pub missing: usize,
    /// Total bytes to copy/move
    pub total_bytes: u64,
}

/// A single operation in the plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    /// Unique ID for this operation
    pub id: u64,
    /// Operation status
    pub status: OperationStatus,
    /// The operation details
    pub kind: OperationKind,
}

/// Status of an operation.
///
/// `Failed` is **retryable** — a later `apply` re-attempts it, so a transient I/O
/// error (a dropped network mount mid-run) recovers by running `apply` again.
/// `Refused` is **sticky** — a safety check (verify-before-delete) declined the
/// operation, and re-running must not blindly retry it; it stays put until the
/// plan is regenerated. `Completed` and `Refused` are the terminal states a
/// re-apply skips; `Pending` and `Failed` are the work it still does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    /// A safety check declined the operation (e.g. no surviving copy to make a
    /// delete safe). Sticky: a re-apply does not retry it.
    Refused,
    Skipped,
}

impl OperationStatus {
    /// Whether a re-apply should (re-)attempt this operation. `Pending` work and
    /// retryable `Failed` ops are run; `Completed`/`Refused`/`Skipped` are not.
    pub fn is_remaining_work(self) -> bool {
        matches!(self, OperationStatus::Pending | OperationStatus::Failed)
    }
}

/// The type of operation to perform
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OperationKind {
    /// Copy a file from source to destination
    Copy {
        source: SourceRef,
        dest: String,
        size: u64,
    },
    /// Move a file (copy + delete source)
    Move {
        source: SourceRef,
        dest: String,
        size: u64,
    },
    /// Relocate a whole file unchanged — e.g. a complete archive that is already
    /// in its final form and only needs to sit at its canonical path. Unlike
    /// `Move`, the content is not re-verified against a ROM hash (the catalogue
    /// hashes an archive's inner entries, not the archive file itself); a
    /// same-filesystem rename preserves the bytes, and the cross-device fallback
    /// verifies the copy is byte-faithful to the source instead.
    Relocate {
        source: String,
        dest: String,
        size: u64,
    },
    /// Repack files into an archive
    Repack {
        sources: Vec<SourceRef>,
        dest: String,
        format: String,
        /// Bytes the rebuilt archive will hold — the planner already knows this,
        /// so the space check uses it instead of stat-ing every source over the
        /// (network) mount. Defaults to 0 for plans written before it was stored.
        #[serde(default)]
        size: u64,
        /// Move mode: delete the loose source files once the archive is built
        /// and verified (a true in-place tidy). Archive-member sources are never
        /// deleted — that would destroy a container shared with other games.
        #[serde(default)]
        move_sources: bool,
    },
    /// Delete a file. `reason` records *why* it is safe to remove — for a dedup
    /// delete, the canonical copy that survives it ("exact duplicate — kept …") —
    /// so the plan can be reviewed and the live log can show it. Defaults to empty
    /// for plans written before the field existed.
    ///
    /// `rebuild` is present only when this delete *drains a staging container* a
    /// repack rebuilt from: removing it is safe (the verify-before-delete net
    /// confirms every entry survives at its destination archive), but a rollback
    /// must put the container back before those destinations are removed. The
    /// spec records how — extract each entry out of the destination it was
    /// repacked into (SHA1-verified) and rebuild the container archive. An
    /// ordinary dedup delete leaves it `None` and carries no reverse, exactly as
    /// before. See [`Plan::add_container_drain`].
    Delete {
        path: String,
        #[serde(default)]
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rebuild: Option<ContainerRebuild>,
    },
    /// Move a file to quarantine (instead of deleting)
    Quarantine {
        path: String,
        sha1: String,
        size: u64,
        reason: String,
        collection: Option<String>,
    },
}

/// How to rebuild a drained staging container on rollback — the reverse of a
/// container-drain delete.
///
/// The container's content survives, after the drain, inside the destination
/// archive(s) its entries were repacked into. To restore the container we
/// extract each entry back out of the destination it lives in (verifying SHA1)
/// and write it into a fresh archive at the container's path. Crucially this
/// runs *before* the repacks' own reverses delete those destinations: a drain is
/// emitted after every repack, so rollback (reverse plan order) rebuilds the
/// container while every destination still exists.
///
/// A container that fed several games spreads its entries across several
/// destinations — each [`RebuildEntry`] therefore names its own `dest`, not one
/// shared archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerRebuild {
    /// Archive format to rebuild the container in (`zip` or `7z`).
    pub format: String,
    /// Every entry the container held, with where to pull it back from.
    pub entries: Vec<RebuildEntry>,
}

/// One entry pulled back into a drained container on rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebuildEntry {
    /// The destination archive the entry was repacked into; extracted from here.
    pub dest: String,
    /// The entry's name within `dest`.
    pub dest_entry: String,
    /// The name the entry had inside the container; restored under this name.
    pub container_entry: String,
    /// Content SHA1, re-verified on extract.
    pub sha1: String,
}

/// Reference to a source file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    /// Full path to the file or archive
    pub path: String,
    /// Path within archive (None for loose files)
    pub archive_path: Option<String>,
    /// SHA1 hash of the content
    pub sha1: String,
    /// Desired entry name when this source is written into an archive (the
    /// DAT-canonical ROM name). `None` falls back to the source file's own name.
    /// Irrelevant to Copy/Move, which carry the full destination path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_name: Option<String>,
}

impl Plan {
    /// Create a new empty plan with the given state hash
    pub fn new(state_hash: String) -> Self {
        let now = chrono_lite_now();
        Self {
            state_hash,
            created_at: now,
            operations: Vec::new(),
            summary: PlanSummary::default(),
            skipped_no_dest: Vec::new(),
            skipped_oversized: Vec::new(),
            per_collection: Vec::new(),
        }
    }

    /// Add a copy operation
    pub fn add_copy(&mut self, source: SourceRef, dest: String, size: u64) {
        let id = self.operations.len() as u64;
        self.operations.push(Operation {
            id,
            status: OperationStatus::Pending,
            kind: OperationKind::Copy { source, dest, size },
        });
        self.summary.copy_count += 1;
        self.summary.total_bytes += size;
    }

    /// Add a move operation (copy into place, then delete the source).
    pub fn add_move(&mut self, source: SourceRef, dest: String, size: u64) {
        let id = self.operations.len() as u64;
        self.operations.push(Operation {
            id,
            status: OperationStatus::Pending,
            kind: OperationKind::Move { source, dest, size },
        });
        self.summary.move_count += 1;
        self.summary.total_bytes += size;
    }

    /// Add a relocate operation: move a whole file (e.g. a complete archive)
    /// unchanged to its canonical path. Counts as a move for the summary.
    pub fn add_relocate(&mut self, source: String, dest: String, size: u64) {
        let id = self.operations.len() as u64;
        self.operations.push(Operation {
            id,
            status: OperationStatus::Pending,
            kind: OperationKind::Relocate { source, dest, size },
        });
        self.summary.move_count += 1;
        self.summary.total_bytes += size;
    }

    /// Add a repack operation: one archive containing a game's ROMs. `size` is
    /// the total uncompressed bytes, used for the plan summary and space check.
    pub fn add_repack(
        &mut self,
        sources: Vec<SourceRef>,
        dest: String,
        format: String,
        size: u64,
        move_sources: bool,
    ) {
        let id = self.operations.len() as u64;
        self.operations.push(Operation {
            id,
            status: OperationStatus::Pending,
            kind: OperationKind::Repack {
                sources,
                dest,
                format,
                size,
                move_sources,
            },
        });
        self.summary.repack_count += 1;
        self.summary.total_bytes += size;
    }

    /// Add a quarantine operation
    pub fn add_quarantine(
        &mut self,
        path: String,
        sha1: String,
        size: u64,
        reason: String,
        collection: Option<String>,
    ) {
        let id = self.operations.len() as u64;
        self.operations.push(Operation {
            id,
            status: OperationStatus::Pending,
            kind: OperationKind::Quarantine {
                path,
                sha1,
                size,
                reason,
                collection,
            },
        });
        self.summary.quarantine_count += 1;
    }

    /// Add a delete operation: remove a redundant file outright. Used to drop an
    /// exact-content duplicate whose bytes are preserved by the canonical copy
    /// kept elsewhere, so nothing unique is lost. `reason` records that "why"
    /// (e.g. the surviving copy) for review and the live log.
    pub fn add_delete(&mut self, path: String, reason: String) {
        let id = self.operations.len() as u64;
        self.operations.push(Operation {
            id,
            status: OperationStatus::Pending,
            kind: OperationKind::Delete {
                path,
                reason,
                rebuild: None,
            },
        });
        self.summary.delete_count += 1;
    }

    /// Add a container-drain delete: remove a staging container a repack rebuilt
    /// from, once its content is safely consolidated into destination archive(s).
    /// Like [`Plan::add_delete`] it runs through the verify-before-delete net, but
    /// it also carries a [`ContainerRebuild`] so a rollback restores the container
    /// (extracting its entries back out of the destinations) before those
    /// destinations are themselves removed by the repacks' reverses.
    pub fn add_container_drain(&mut self, path: String, reason: String, rebuild: ContainerRebuild) {
        let id = self.operations.len() as u64;
        self.operations.push(Operation {
            id,
            status: OperationStatus::Pending,
            kind: OperationKind::Delete {
                path,
                reason,
                rebuild: Some(rebuild),
            },
        });
        self.summary.delete_count += 1;
    }

    /// Check if the plan has any operations
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Get the number of operations
    pub fn operation_count(&self) -> usize {
        self.operations.len()
    }
}

/// Generate a timestamp string in SQLite datetime format (YYYY-MM-DD HH:MM:SS)
fn chrono_lite_now() -> String {
    use chrono::{Datelike, Local, Timelike};
    let now = Local::now();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_new() {
        let plan = Plan::new("abc123".to_string());
        assert_eq!(plan.state_hash, "abc123");
        assert!(plan.is_empty());
        assert_eq!(plan.operation_count(), 0);
    }

    #[test]
    fn test_plan_add_copy() {
        let mut plan = Plan::new("test".to_string());

        plan.add_copy(
            SourceRef {
                path: "/source/game.rom".to_string(),
                archive_path: None,
                sha1: "ABC123".to_string(),
                entry_name: None,
            },
            "/dest/game.rom".to_string(),
            1024,
        );

        assert!(!plan.is_empty());
        assert_eq!(plan.operation_count(), 1);
        assert_eq!(plan.summary.copy_count, 1);
        assert_eq!(plan.summary.total_bytes, 1024);
    }

    #[test]
    fn test_plan_serialize() {
        let mut plan = Plan::new("hash123".to_string());
        plan.add_copy(
            SourceRef {
                path: "/src/rom.nes".to_string(),
                archive_path: None,
                sha1: "SHA1HASH".to_string(),
                entry_name: None,
            },
            "/dest/rom.nes".to_string(),
            2048,
        );

        let json = serde_json::to_string_pretty(&plan).unwrap();
        assert!(json.contains("\"state_hash\": \"hash123\""));
        assert!(json.contains("\"type\": \"copy\""));
        assert!(json.contains("\"/src/rom.nes\""));
    }

    #[test]
    fn test_plan_deserialize() {
        let json = r#"{
            "state_hash": "test123",
            "created_at": "2024-01-01 00:00:00",
            "operations": [
                {
                    "id": 0,
                    "status": "pending",
                    "kind": {
                        "type": "copy",
                        "source": {
                            "path": "/src/file.rom",
                            "archive_path": null,
                            "sha1": "DEADBEEF"
                        },
                        "dest": "/dest/file.rom",
                        "size": 1000
                    }
                }
            ],
            "summary": {
                "copy_count": 1,
                "move_count": 0,
                "repack_count": 0,
                "delete_count": 0,
                "already_correct": 0,
                "missing": 0,
                "total_bytes": 1000
            }
        }"#;

        let plan: Plan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.state_hash, "test123");
        assert_eq!(plan.operations.len(), 1);
        assert_eq!(plan.summary.copy_count, 1);
    }

    #[test]
    fn test_operation_kind_copy() {
        let kind = OperationKind::Copy {
            source: SourceRef {
                path: "/src".to_string(),
                archive_path: None,
                sha1: "hash".to_string(),
                entry_name: None,
            },
            dest: "/dest".to_string(),
            size: 100,
        };

        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"copy\""));
    }

    #[test]
    fn test_operation_kind_repack() {
        let kind = OperationKind::Repack {
            sources: vec![
                SourceRef {
                    path: "/src/a.rom".to_string(),
                    archive_path: None,
                    sha1: "hash1".to_string(),
                    entry_name: None,
                },
                SourceRef {
                    path: "/src/b.rom".to_string(),
                    archive_path: None,
                    sha1: "hash2".to_string(),
                    entry_name: None,
                },
            ],
            dest: "/dest/game.zip".to_string(),
            format: "zip".to_string(),
            size: 2048,
            move_sources: false,
        };

        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"repack\""));
        assert!(json.contains("\"sources\""));
    }
}
