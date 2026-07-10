use anyhow::Result;
use std::path::PathBuf;

use crate::db::quarantine as db_quarantine;
use crate::util::{format_bytes, truncate_path};

use super::open_database;

/// Show quarantine status and contents.
pub(super) fn run(
    collection: Option<String>,
    detailed: bool,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let entries = if let Some(ref pattern) = collection {
        db_quarantine::list_entries_by_collection(conn, pattern)?
    } else {
        db_quarantine::list_entries(conn)?
    };

    if entries.is_empty() {
        println!("Quarantine is empty.");
        return Ok(());
    }

    let total_size = db_quarantine::total_size(conn)?;
    let count = entries.len();

    println!(
        "Quarantine: {} files, {}",
        count,
        format_bytes(total_size as u64)
    );
    println!();

    let by_collection = db_quarantine::summary_by_collection(conn)?;
    if by_collection.len() > 1 || by_collection.iter().any(|(c, _, _)| c.is_some()) {
        println!("By collection:");
        for (coll, cnt, size) in &by_collection {
            let name = coll.as_deref().unwrap_or("(unknown)");
            println!(
                "  {} ··· {} files, {}",
                name,
                cnt,
                format_bytes(*size as u64)
            );
        }
        println!();
    }

    let by_reason = db_quarantine::summary_by_reason(conn)?;
    println!("By reason:");
    for (reason, cnt, size) in &by_reason {
        println!(
            "  {} ··· {} files, {}",
            reason.description(),
            cnt,
            format_bytes(*size as u64)
        );
    }

    if detailed {
        println!();
        println!("Files:");
        for entry in &entries {
            println!(
                "  {} ({}) - {}",
                truncate_path(&entry.original_path, 50),
                format_bytes(entry.size as u64),
                entry.reason.description()
            );
        }
    }

    println!();
    println!("Use 'cat198x quarantine prune' to permanently delete.");
    println!("Use 'cat198x quarantine restore' to move back to sources.");

    Ok(())
}
