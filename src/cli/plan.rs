//! Plan command implementation

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

use crate::config::Config;
use crate::plan::{Plan, PlanOptions, generate_plan_filtered};

use super::{get_data_dir, open_database};

/// Run the plan command
pub fn run(dat_filter: Option<String>, move_files: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();

    // The library-wide default destination (if configured) lets collections
    // without their own dest_path be planned under one root.
    let config_path = get_data_dir(data_dir.clone())?.join("config.toml");
    let file_config = if config_path.exists() {
        Config::load(&config_path).unwrap_or_default()
    } else {
        Config::default()
    };

    if let Some(ref filter) = dat_filter {
        println!("Generating plan for collections matching: {}", filter);
    } else {
        println!("Generating plan for all configured collections...");
    }
    println!();

    let plan = generate_plan_filtered(
        conn,
        &PlanOptions {
            dat_filter,
            default_dest: file_config.default_dest_path,
            default_format: file_config.default_output_format,
            move_files,
        },
    )?;

    let data_dir = get_data_dir(data_dir)?;

    // Write the skipped-collection list (no destination resolved) for review,
    // even when there are no operations to perform.
    if !plan.skipped_no_dest.is_empty() {
        let skipped_path = data_dir.join("skipped-no-destination.txt");
        let mut names = plan.skipped_no_dest.clone();
        names.sort();
        names.push(String::new()); // trailing newline
        fs::write(&skipped_path, names.join("\n"))
            .context("Failed to write skipped-collection list")?;
        println!("  Full list written to: {}", skipped_path.display());
    }

    if plan.is_empty() {
        println!();
        println!("No operations needed.");
        return Ok(());
    }

    // Save the plan
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

    print_breakdown_by_set(&plan);

    println!();
    println!("Plan saved to: {}", plan_path.display());
    println!();
    println!("Review the plan, then apply with:");
    println!("  cat198x apply");

    Ok(())
}

/// Print a breakdown of pending operations rolled up by set (the top segment of
/// each collection's library path), so a large plan is reviewable at a glance.
/// Only sets with operations to perform are shown.
fn print_breakdown_by_set(plan: &Plan) {
    use std::collections::BTreeMap;

    let mut by_set: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for c in &plan.per_collection {
        if c.to_write == 0 {
            continue;
        }
        let set = c.node_path.split('/').next().unwrap_or(&c.node_path);
        let entry = by_set.entry(set).or_default();
        entry.0 += c.to_write;
        entry.1 += c.bytes;
    }

    if by_set.is_empty() {
        return;
    }

    println!();
    println!("By set (operations pending):");
    for (set, (count, bytes)) in &by_set {
        println!("  {:30}  {} to write, {}", set, count, format_bytes(*bytes));
    }
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
            && let Ok(modified) = metadata.modified()
        {
            match &latest {
                None => latest = Some((path, modified)),
                Some((_, prev_time)) if modified > *prev_time => latest = Some((path, modified)),
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
