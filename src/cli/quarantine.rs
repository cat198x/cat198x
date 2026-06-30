//! Quarantine command implementations
//!
//! The quarantine is a holding area for files that are no longer needed
//! at their current location but shouldn't be immediately deleted.

mod prune;
mod restore;
mod status;

use anyhow::Result;
use std::path::PathBuf;

use crate::cli::args::QuarantineCommands;
use crate::db::quarantine as db_quarantine;

use super::open_database;

/// Run the quarantine command
pub fn run(cmd: QuarantineCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        QuarantineCommands::Status {
            collection,
            detailed,
        } => status::run(collection, detailed, data_dir),
        QuarantineCommands::Prune { collection, yes } => prune::run(collection, yes, data_dir),
        QuarantineCommands::Restore {
            collection,
            target,
            yes,
        } => restore::run(collection, target, yes, data_dir),
    }
}

/// Move a file to quarantine
///
/// This is called from the apply workflow when a file needs to be quarantined.
pub fn move_to_quarantine(
    file_path: &str,
    sha1: &str,
    size: i64,
    reason: db_quarantine::QuarantineReason,
    collection_name: Option<&str>,
    data_dir: Option<PathBuf>,
) -> Result<String> {
    // Resolve the store location here (config vs default) and open the
    // connection; the file move + catalogue entry are the library primitive.
    let quarantine_dir = super::config::resolve_quarantine_dir(data_dir.clone())?;
    let db = open_database(data_dir)?;
    crate::plan::executor::execute_quarantine(
        db.conn(),
        file_path,
        sha1,
        size,
        reason,
        collection_name,
        &quarantine_dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_to_quarantine_refuses_to_overwrite() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path().join("data");
        let sha1 = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";

        // Cat198x must be initialised so the quarantine DB exists.
        crate::cli::init::run(None, Some(data_dir.clone())).unwrap();

        // First file is quarantined under a full-SHA1 filename.
        let f1 = temp.path().join("game.rom");
        std::fs::write(&f1, b"first").unwrap();
        move_to_quarantine(
            f1.to_str().unwrap(),
            sha1,
            5,
            db_quarantine::QuarantineReason::SetRemoved,
            None,
            Some(data_dir.clone()),
        )
        .unwrap();
        let qfile = data_dir
            .join("quarantine")
            .join(format!("{}_game.rom", sha1));
        assert!(qfile.exists(), "quarantined under the full-SHA1 name");
        let original = std::fs::read(&qfile).unwrap();

        // A different file mapping to the same quarantine path must be refused,
        // not silently clobbered, and its source left in place.
        let f2 = temp.path().join("game.rom");
        std::fs::write(&f2, b"second-and-different").unwrap();
        let result = move_to_quarantine(
            f2.to_str().unwrap(),
            sha1,
            20,
            db_quarantine::QuarantineReason::SetRemoved,
            None,
            Some(data_dir.clone()),
        );
        assert!(
            result.is_err(),
            "must refuse to overwrite an existing quarantine file"
        );
        assert_eq!(
            std::fs::read(&qfile).unwrap(),
            original,
            "existing quarantine file untouched"
        );
        assert!(
            f2.exists(),
            "source left in place when quarantine is refused"
        );
    }
}
