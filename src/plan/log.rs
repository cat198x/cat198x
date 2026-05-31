//! Operation logging for rollback support

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// An operation log entry with forward and reverse operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Unique operation ID (from plan)
    pub operation_id: u64,
    /// Timestamp when operation was executed
    pub executed_at: String,
    /// Forward operation that was executed
    pub forward: LoggedOperation,
    /// Reverse operation for rollback
    pub reverse: Option<LoggedOperation>,
    /// Current status
    pub status: LogStatus,
}

/// Status of a logged operation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStatus {
    /// Operation completed successfully
    Completed,
    /// Operation failed
    Failed,
    /// Operation was rolled back
    RolledBack,
}

/// A logged operation (forward or reverse)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LoggedOperation {
    /// Copy operation
    Copy {
        source: String,
        dest: String,
        sha1: String,
    },
    /// Move operation
    Move {
        source: String,
        dest: String,
        sha1: String,
    },
    /// Delete operation
    Delete { path: String },
    /// Repack operation
    Repack {
        sources: Vec<String>,
        dest: String,
    },
}

/// An operation log containing all entries for an apply session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationLog {
    /// Plan state hash this log is for
    pub plan_hash: String,
    /// When the apply session started
    pub started_at: String,
    /// When the apply session completed (if finished)
    pub completed_at: Option<String>,
    /// All operation entries
    pub entries: Vec<LogEntry>,
}

impl OperationLog {
    /// Create a new operation log for a plan
    pub fn new(plan_hash: String) -> Self {
        Self {
            plan_hash,
            started_at: chrono_now(),
            completed_at: None,
            entries: Vec::new(),
        }
    }

    /// Add a completed copy operation
    pub fn log_copy(
        &mut self,
        operation_id: u64,
        source: &str,
        dest: &str,
        sha1: &str,
        success: bool,
    ) {
        let forward = LoggedOperation::Copy {
            source: source.to_string(),
            dest: dest.to_string(),
            sha1: sha1.to_string(),
        };

        // Reverse of COPY is DELETE
        let reverse = if success {
            Some(LoggedOperation::Delete {
                path: dest.to_string(),
            })
        } else {
            None
        };

        self.entries.push(LogEntry {
            operation_id,
            executed_at: chrono_now(),
            forward,
            reverse,
            status: if success {
                LogStatus::Completed
            } else {
                LogStatus::Failed
            },
        });
    }

    /// Add a completed move operation
    pub fn log_move(
        &mut self,
        operation_id: u64,
        source: &str,
        dest: &str,
        sha1: &str,
        success: bool,
    ) {
        let forward = LoggedOperation::Move {
            source: source.to_string(),
            dest: dest.to_string(),
            sha1: sha1.to_string(),
        };

        // Reverse of MOVE is MOVE back
        let reverse = if success {
            Some(LoggedOperation::Move {
                source: dest.to_string(),
                dest: source.to_string(),
                sha1: sha1.to_string(),
            })
        } else {
            None
        };

        self.entries.push(LogEntry {
            operation_id,
            executed_at: chrono_now(),
            forward,
            reverse,
            status: if success {
                LogStatus::Completed
            } else {
                LogStatus::Failed
            },
        });
    }

    /// Add a completed repack operation
    pub fn log_repack(
        &mut self,
        operation_id: u64,
        sources: &[String],
        dest: &str,
        success: bool,
    ) {
        let forward = LoggedOperation::Repack {
            sources: sources.to_vec(),
            dest: dest.to_string(),
        };

        // Reverse of REPACK is DELETE (the created archive)
        let reverse = if success {
            Some(LoggedOperation::Delete {
                path: dest.to_string(),
            })
        } else {
            None
        };

        self.entries.push(LogEntry {
            operation_id,
            executed_at: chrono_now(),
            forward,
            reverse,
            status: if success {
                LogStatus::Completed
            } else {
                LogStatus::Failed
            },
        });
    }

    /// Mark the log as complete
    pub fn complete(&mut self) {
        self.completed_at = Some(chrono_now());
    }

    /// Save the log to disk
    pub fn save(&self, logs_dir: &Path) -> Result<std::path::PathBuf> {
        fs::create_dir_all(logs_dir).context("Failed to create logs directory")?;

        // Use timestamp + plan hash prefix for uniqueness
        let filename = format!(
            "{}_{}.json",
            self.started_at.replace([':', '-', 'T', 'Z'], ""),
            &self.plan_hash[..8]
        );
        let path = logs_dir.join(filename);

        let json = serde_json::to_string_pretty(self).context("Failed to serialize log")?;
        fs::write(&path, json).context("Failed to write log file")?;

        Ok(path)
    }

    /// Load a log from disk
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path).context("Failed to read log file")?;
        serde_json::from_str(&contents).context("Failed to parse log file")
    }

    /// Count successful operations
    pub fn success_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == LogStatus::Completed)
            .count()
    }

    /// Count failed operations
    pub fn failed_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == LogStatus::Failed)
            .count()
    }
}

/// Get current timestamp in ISO 8601 format (YYYYMMDDTHHMMSSZ)
fn chrono_now() -> String {
    use chrono::{Utc, Timelike, Datelike};
    let now = Utc::now();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
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
            true,
        );

        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].status, LogStatus::Completed);

        // Reverse of repack is delete
        let reverse = log.entries[0].reverse.as_ref().unwrap();
        match reverse {
            LoggedOperation::Delete { path } => {
                assert_eq!(path, "/dest/game.zip");
            }
            _ => panic!("Expected Delete reverse operation"),
        }
    }

    #[test]
    fn test_log_repack_failure() {
        let mut log = OperationLog::new("abc123".to_string());
        log.log_repack(1, &["/src/a.rom".to_string()], "/dest/game.zip", false);

        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].status, LogStatus::Failed);
        assert!(log.entries[0].reverse.is_none());
    }
}
