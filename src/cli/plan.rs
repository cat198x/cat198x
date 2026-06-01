//! Plan command implementation

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

use crate::plan::{generate_plan_filtered, Plan};

use super::{get_data_dir, open_database};

/// Run the plan command
pub fn run(dat_filter: Option<String>, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();

    if let Some(ref filter) = dat_filter {
        println!("Generating plan for collections matching: {}", filter);
    } else {
        println!("Generating plan for all configured collections...");
    }
    println!();

    let plan = generate_plan_filtered(conn, dat_filter.as_deref())?;

    if plan.is_empty() {
        println!();
        println!("No operations needed.");
        return Ok(());
    }

    // Save the plan
    let data_dir = get_data_dir(data_dir)?;
    let plans_dir = data_dir.join("objects/plans");
    fs::create_dir_all(&plans_dir).context("Failed to create plans directory")?;

    let plan_path = plans_dir.join(format!("{}.json", &plan.state_hash[..16]));
    let plan_json = serde_json::to_string_pretty(&plan).context("Failed to serialize plan")?;
    fs::write(&plan_path, &plan_json).context("Failed to write plan file")?;

    // Print summary
    println!();
    println!("Plan summary:");
    println!("  {} copy operations", plan.summary.copy_count);
    if plan.summary.move_count > 0 {
        println!("  {} move operations", plan.summary.move_count);
    }
    if plan.summary.repack_count > 0 {
        println!("  {} repack operations", plan.summary.repack_count);
    }
    println!("  {} already correct", plan.summary.already_correct);
    println!(
        "  {} bytes to transfer",
        format_bytes(plan.summary.total_bytes)
    );
    println!();
    println!("Plan saved to: {}", plan_path.display());
    println!();
    println!("Review the plan, then apply with:");
    println!("  cat198x apply");

    Ok(())
}

/// Load the most recent plan from disk
pub fn load_latest_plan(data_dir: Option<PathBuf>) -> Result<Option<(Plan, PathBuf)>> {
    let data_dir = get_data_dir(data_dir)?;
    let plans_dir = data_dir.join("objects/plans");

    if !plans_dir.exists() {
        return Ok(None);
    }

    // Find the most recently modified plan file
    let mut latest: Option<(PathBuf, std::time::SystemTime)> = None;

    for entry in fs::read_dir(&plans_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "json").unwrap_or(false)
            && let Ok(metadata) = entry.metadata()
                && let Ok(modified) = metadata.modified() {
                    match &latest {
                        None => latest = Some((path, modified)),
                        Some((_, prev_time)) if modified > *prev_time => {
                            latest = Some((path, modified))
                        }
                        _ => {}
                    }
                }
    }

    match latest {
        Some((path, _)) => {
            let contents = fs::read_to_string(&path)?;
            let plan: Plan = serde_json::from_str(&contents)?;
            Ok(Some((plan, path)))
        }
        None => Ok(None),
    }
}

/// Format bytes as human-readable string
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1048576), "1.00 MB");
        assert_eq!(format_bytes(1073741824), "1.00 GB");
    }
}
