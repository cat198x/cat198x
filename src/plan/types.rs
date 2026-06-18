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

/// Status of an operation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
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
    /// Delete a file
    Delete { path: String },
    /// Move a file to quarantine (instead of deleting)
    Quarantine {
        path: String,
        sha1: String,
        size: u64,
        reason: String,
        collection: Option<String>,
    },
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
    /// kept elsewhere, so nothing unique is lost.
    pub fn add_delete(&mut self, path: String) {
        let id = self.operations.len() as u64;
        self.operations.push(Operation {
            id,
            status: OperationStatus::Pending,
            kind: OperationKind::Delete { path },
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
