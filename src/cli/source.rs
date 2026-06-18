//! Source directory management commands

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

use crate::SourceCommands;
use crate::config::Config;
use crate::db::files::{self, Disposition};

use super::{get_data_dir, open_database};

/// Run a source subcommand
pub fn run(cmd: SourceCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        SourceCommands::Add {
            path,
            preserve,
            consume,
        } => add_source(&path, preserve, consume, data_dir),
        SourceCommands::Remove { path } => remove_source(&path, data_dir),
        SourceCommands::List => list_sources(data_dir),
        SourceCommands::SetDisposition { path, disposition } => {
            set_disposition(&path, &disposition, data_dir)
        }
    }
}

fn add_source(
    path: &PathBuf,
    preserve: bool,
    consume: bool,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let abs_path =
        std::fs::canonicalize(path).with_context(|| format!("Cannot resolve path: {:?}", path))?;

    // Check if directory exists
    if !abs_path.is_dir() {
        anyhow::bail!("Path is not a directory: {}", abs_path.display());
    }

    let db = open_database(data_dir.clone())?;
    let conn = db.conn();

    // Check if already registered
    let path_str = abs_path.to_string_lossy().into_owned();
    if let Some(existing) = files::get_source_by_path(conn, &path_str)? {
        println!("Source already registered:");
        println!("  Path: {}", existing.path);
        println!("  Disposition: {}", existing.disposition.as_str());
        println!("  Case sensitive: {}", existing.case_sensitive);
        println!("  Added: {}", existing.added_at);
        if let Some(scanned) = existing.last_scanned {
            println!("  Last scanned: {}", scanned);
        }
        return Ok(());
    }

    // Disposition follows the directory's role unless overridden. A destination
    // is always preserved — refuse an explicit --consume there.
    let roots = destination_roots(conn, &data_dir)?;
    let is_dest = is_destination(&path_str, &roots);
    let disposition = if preserve {
        Disposition::Preserve
    } else if consume {
        if is_dest {
            anyhow::bail!(
                "{} sits under a destination root — a destination is always preserved \
                 and cannot be consumed.",
                abs_path.display()
            );
        }
        Disposition::Consume
    } else if is_dest {
        Disposition::Preserve
    } else {
        Disposition::Consume
    };

    // Detect case sensitivity
    let case_sensitive = detect_case_sensitivity(&abs_path);

    // Add to database, then set the resolved disposition (add defaults preserve).
    let id = files::add_source(conn, &path_str, case_sensitive)?;
    files::set_source_disposition(conn, &path_str, disposition)?;

    println!("Added source #{}: {}", id, abs_path.display());
    println!("  Disposition: {}", disposition.as_str());
    println!("  Case sensitive: {}", case_sensitive);
    println!();
    println!("Run 'cat198x scan' to index files in this source.");

    Ok(())
}

fn set_disposition(path: &PathBuf, disposition: &str, data_dir: Option<PathBuf>) -> Result<()> {
    let target = match disposition {
        "consume" => Disposition::Consume,
        "preserve" => Disposition::Preserve,
        other => anyhow::bail!("Unknown disposition '{other}' — use 'consume' or 'preserve'."),
    };

    let abs_path =
        std::fs::canonicalize(path).with_context(|| format!("Cannot resolve path: {:?}", path))?;

    let db = open_database(data_dir.clone())?;
    let conn = db.conn();
    let path_str = abs_path.to_string_lossy().into_owned();

    if files::get_source_by_path(conn, &path_str)?.is_none() {
        anyhow::bail!(
            "Not a registered source: {}\nUse 'cat198x source list' to see registered sources.",
            abs_path.display()
        );
    }

    // A destination is always preserved; consuming it would move content out of
    // the library.
    if target == Disposition::Consume {
        let roots = destination_roots(conn, &data_dir)?;
        if is_destination(&path_str, &roots) {
            anyhow::bail!(
                "{} sits under a destination root — a destination is always preserved \
                 and cannot be consumed.",
                abs_path.display()
            );
        }
    }

    files::set_source_disposition(conn, &path_str, target)?;
    println!(
        "Set {} disposition: {}",
        abs_path.display(),
        target.as_str()
    );
    Ok(())
}

/// The configured destination roots: the library-wide `default_dest_path` plus
/// every per-collection `dest_path`. A source at or under any of these is a
/// destination and is always preserved.
fn destination_roots(conn: &Connection, data_dir: &Option<PathBuf>) -> Result<Vec<String>> {
    let mut roots = Vec::new();
    if let Ok(dir) = get_data_dir(data_dir.clone()) {
        let config_path = dir.join("config.toml");
        if config_path.exists()
            && let Ok(config) = Config::load(&config_path)
            && let Some(default_dest) = config.default_dest_path
        {
            roots.push(default_dest);
        }
    }
    let mut stmt =
        conn.prepare("SELECT DISTINCT dest_path FROM dat_config WHERE dest_path IS NOT NULL")?;
    let dest_paths = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    roots.extend(dest_paths);
    Ok(roots)
}

/// Whether `path` is at or under any destination root.
fn is_destination(path: &str, roots: &[String]) -> bool {
    let path = path.trim_end_matches('/');
    roots.iter().any(|root| {
        let root = root.trim_end_matches('/');
        path == root || path.starts_with(&format!("{root}/"))
    })
}

fn remove_source(path: &PathBuf, data_dir: Option<PathBuf>) -> Result<()> {
    let abs_path =
        std::fs::canonicalize(path).with_context(|| format!("Cannot resolve path: {:?}", path))?;

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
        println!("    Disposition: {}", source.disposition.as_str());
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
    fn is_destination_matches_at_or_under_a_root() {
        let roots = vec![
            "/Volumes/Data/Library/ROMs".to_string(),
            "/Volumes/Data/Library/ROMs/MAME/".to_string(), // trailing slash tolerated
        ];
        // At a root, and under one → destination (preserve).
        assert!(is_destination("/Volumes/Data/Library/ROMs", &roots));
        assert!(is_destination("/Volumes/Data/Library/ROMs/TOSEC", &roots));
        assert!(is_destination(
            "/Volumes/Data/Library/ROMs/MAME/Software List/32x",
            &roots
        ));
        // Staging, and a sibling that merely shares a prefix string → not.
        assert!(!is_destination("/Volumes/Data/ToSort/MAME", &roots));
        assert!(!is_destination("/Volumes/Data/Library/ROMs-backup", &roots));
    }

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
