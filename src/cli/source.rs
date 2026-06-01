//! Source directory management commands

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::db::files;
use crate::SourceCommands;

use super::open_database;

/// Run a source subcommand
pub fn run(cmd: SourceCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        SourceCommands::Add { path } => add_source(&path, data_dir),
        SourceCommands::Remove { path } => remove_source(&path, data_dir),
        SourceCommands::List => list_sources(data_dir),
    }
}

fn add_source(path: &PathBuf, data_dir: Option<PathBuf>) -> Result<()> {
    let abs_path = std::fs::canonicalize(path)
        .with_context(|| format!("Cannot resolve path: {:?}", path))?;

    // Check if directory exists
    if !abs_path.is_dir() {
        anyhow::bail!("Path is not a directory: {}", abs_path.display());
    }

    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Check if already registered
    let path_str = abs_path.to_string_lossy();
    if let Some(existing) = files::get_source_by_path(conn, &path_str)? {
        println!("Source already registered:");
        println!("  Path: {}", existing.path);
        println!("  Case sensitive: {}", existing.case_sensitive);
        println!("  Added: {}", existing.added_at);
        if let Some(scanned) = existing.last_scanned {
            println!("  Last scanned: {}", scanned);
        }
        return Ok(());
    }

    // Detect case sensitivity
    let case_sensitive = detect_case_sensitivity(&abs_path);

    // Add to database
    let id = files::add_source(conn, &path_str, case_sensitive)?;

    println!("Added source #{}: {}", id, abs_path.display());
    println!("  Case sensitive: {}", case_sensitive);
    println!();
    println!("Run 'cat198x scan' to index files in this source.");

    Ok(())
}

fn remove_source(path: &PathBuf, data_dir: Option<PathBuf>) -> Result<()> {
    let abs_path = std::fs::canonicalize(path)
        .with_context(|| format!("Cannot resolve path: {:?}", path))?;

    let db = open_database(data_dir)?;
    let conn = db.conn();

    let path_str = abs_path.to_string_lossy();
    let removed = files::remove_source(conn, &path_str)?;

    if removed {
        println!("Removed source: {}", abs_path.display());
        println!("  (Files on disk were not modified)");
    } else {
        println!("Source not found: {}", abs_path.display());
        println!();
        println!("Use 'cat198x source list' to see registered sources.");
    }

    Ok(())
}

fn list_sources(data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let sources = files::list_sources(conn)?;

    if sources.is_empty() {
        println!("No sources registered.");
        println!();
        println!("Add a source directory with:");
        println!("  cat198x source add <path>");
        return Ok(());
    }

    println!("Registered sources:");
    println!();

    for source in &sources {
        let file_count = files::count_files_in_source(conn, source.id)?;

        println!("  {} [#{}]", source.path, source.id);
        println!("    Case sensitive: {}", source.case_sensitive);
        println!("    Files indexed: {}", file_count);
        if let Some(ref scanned) = source.last_scanned {
            println!("    Last scanned: {}", scanned);
        } else {
            println!("    Last scanned: never");
        }
        println!();
    }

    println!("Total: {} source(s)", sources.len());

    Ok(())
}

/// Detect if a filesystem is case-sensitive
fn detect_case_sensitivity(path: &Path) -> bool {
    // Try to detect by creating a temp file and checking if the
    // opposite-case version exists
    let test_file = path.join(".cat198x_case_test");
    let test_file_upper = path.join(".CAT198X_CASE_TEST");

    // Clean up any existing test files
    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_file(&test_file_upper);

    // Create lowercase test file
    if std::fs::write(&test_file, "test").is_err() {
        // Can't write - assume case-sensitive (safer default)
        return true;
    }

    // Check if uppercase version exists (would mean case-insensitive)
    let case_sensitive = !test_file_upper.exists();

    // Clean up
    let _ = std::fs::remove_file(&test_file);

    case_sensitive
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_detect_case_sensitivity() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().to_path_buf();

        // This test will pass on any filesystem - it just checks the function runs
        let result = detect_case_sensitivity(&path);
        // Result depends on the filesystem, but function should not panic
        // The expression `result || !result` is always true, verifying it returned a bool
        let _ = result; // Just ensure it ran without panic
    }
}
