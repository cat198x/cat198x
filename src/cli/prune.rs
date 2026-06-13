//! `prune-empty` command — remove directories left empty after an in-place tidy.
//!
//! A `--move` apply relocates ROMs out of `ToSort/…` but only ever deletes
//! *files* (the engine has no `remove_dir` anywhere), so the emptied source
//! folders stay behind as a skeleton of empty directories. This prunes them.
//!
//! Safety: it removes a directory only with `fs::remove_dir`, which fails on any
//! non-empty directory — so it can never delete a folder that still holds a
//! file. It is scoped to registered source roots, never removes a source root
//! itself, never follows symlinks, and reports by default (`--remove` executes).
//! Unlike `apply` this is not journaled, which is exactly why it previews first.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::{get_data_dir, open_database};
use crate::db::files::list_sources;

/// File names treated as OS metadata cruft when `--ignore-os-junk` is set: a
/// directory holding only these (no real files, no kept subdirs) counts as
/// empty, and the cruft is deleted just before the directory is removed.
const OS_JUNK_NAMES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

/// Whether a file name is OS metadata cruft: the fixed names above, or an
/// AppleDouble `._*` sidecar.
fn is_os_junk(name: &str) -> bool {
    name.starts_with("._") || OS_JUNK_NAMES.iter().any(|j| j.eq_ignore_ascii_case(name))
}

/// What a prune pass found / did.
#[derive(Default)]
pub struct PruneReport {
    /// Directories that are (or were) removable, deepest first.
    pub dirs: Vec<PathBuf>,
    /// OS-junk files that would be / were deleted to empty those directories.
    pub junk: Vec<PathBuf>,
}

/// Options for a prune pass.
pub struct PruneOptions {
    /// Actually delete (else report only).
    pub remove: bool,
    /// Treat a directory holding only OS cruft as empty (and delete that cruft).
    pub ignore_os_junk: bool,
}

/// Decide whether `dir` is removable, recursing depth-first so a parent emptied
/// by removing its children is caught in the same pass. A directory is removable
/// when every entry is either a removable subdirectory or — under
/// `ignore_os_junk` — OS cruft; i.e. it holds no real files and no kept
/// subdirectories. A symlink (file or dir) is never cruft and is never followed,
/// so a directory containing one is always kept.
///
/// When `opts.remove` is set, a removable directory's cruft is deleted and the
/// directory itself is `remove_dir`'d as the recursion unwinds (children before
/// parents). Found paths are recorded in `report` either way.
fn prune_dir(dir: &Path, opts: &PruneOptions, report: &mut PruneReport) -> Result<bool> {
    let mut removable = true;
    let mut junk_here: Vec<PathBuf> = Vec::new();

    let entries = std::fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read entry in {}", dir.display()))?;
        let path = entry.path();
        // Do not follow symlinks: a symlinked file or dir keeps this directory.
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            removable = false;
            continue;
        }
        if ft.is_dir() {
            if !prune_dir(&path, opts, report)? {
                removable = false;
            }
        } else {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if opts.ignore_os_junk && is_os_junk(&name) {
                junk_here.push(path);
            } else {
                // A real file (or one we can't safely classify) keeps the dir.
                removable = false;
            }
        }
    }

    if removable {
        // Record (and, if removing, delete) the cruft then the directory itself.
        // Children have already been handled by the recursion above.
        for j in &junk_here {
            if opts.remove {
                std::fs::remove_file(j).with_context(|| format!("remove cruft {}", j.display()))?;
            }
            report.junk.push(j.clone());
        }
        if opts.remove {
            std::fs::remove_dir(dir).with_context(|| format!("remove dir {}", dir.display()))?;
        }
        report.dirs.push(dir.to_path_buf());
    }

    Ok(removable)
}

/// Prune empty directories beneath each source root. The root itself is never
/// removed (it stays a registered source) — only directories within it. Shared
/// with `apply --prune-empty`, which calls it once after a move-tidy completes.
pub fn prune_sources(roots: &[PathBuf], opts: &PruneOptions) -> Result<PruneReport> {
    let mut report = PruneReport::default();
    for root in roots {
        if !root.is_dir() {
            continue; // A source whose directory is gone has nothing to prune.
        }
        // Walk the root's children but never offer the root itself for removal.
        let entries =
            std::fs::read_dir(root).with_context(|| format!("read source {}", root.display()))?;
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() && !entry.file_type()?.is_symlink() {
                prune_dir(&entry.path(), opts, &mut report)?;
            }
        }
    }
    Ok(report)
}

/// Run the prune-empty command.
pub fn run(
    sources: Vec<String>,
    remove: bool,
    ignore_os_junk: bool,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let all = list_sources(db.conn())?;

    // Resolve which source roots to walk: all registered, or only those whose id
    // or path matches a `--source` selector.
    let roots: Vec<PathBuf> = if sources.is_empty() {
        all.iter().map(|s| PathBuf::from(&s.path)).collect()
    } else {
        all.iter()
            .filter(|s| {
                sources
                    .iter()
                    .any(|sel| sel == &s.id.to_string() || s.path.contains(sel.as_str()))
            })
            .map(|s| PathBuf::from(&s.path))
            .collect()
    };

    if roots.is_empty() {
        println!("No matching source roots to prune.");
        return Ok(());
    }

    let opts = PruneOptions {
        remove,
        ignore_os_junk,
    };
    let report = prune_sources(&roots, &opts)?;

    if report.dirs.is_empty() {
        println!(
            "No empty directories found under {} source(s).",
            roots.len()
        );
        return Ok(());
    }

    let verb = if remove { "Removed" } else { "Would remove" };
    println!(
        "{} {} empty director{}{}.",
        verb,
        report.dirs.len(),
        if report.dirs.len() == 1 { "y" } else { "ies" },
        if report.junk.is_empty() {
            String::new()
        } else {
            format!(" and {} OS-cruft file(s)", report.junk.len())
        }
    );

    // Write the full list (deepest-first, the removal order) for review.
    let out = get_data_dir(data_dir)?.join("pruned-empty-dirs.txt");
    let mut lines: Vec<String> = report
        .dirs
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    lines.push(String::new()); // trailing newline
    std::fs::write(&out, lines.join("\n")).context("Failed to write pruned-dirs list")?;
    println!("Full list written to: {}", out.display());

    if !remove {
        println!();
        println!("This was a preview. Re-run with --remove to delete these directories.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn opts(remove: bool, ignore_os_junk: bool) -> PruneOptions {
        PruneOptions {
            remove,
            ignore_os_junk,
        }
    }

    #[test]
    fn removes_nested_empty_dirs_but_keeps_dirs_with_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // root/empty/deeper  -> both empty, both prunable
        // root/keep/rom.bin  -> kept (holds a real file)
        fs::create_dir_all(root.join("empty/deeper")).unwrap();
        fs::create_dir_all(root.join("keep")).unwrap();
        fs::write(root.join("keep/rom.bin"), b"data").unwrap();

        let report = prune_sources(&[root.to_path_buf()], &opts(true, false)).unwrap();

        // Both empty dirs gone, deepest first; the root itself untouched.
        assert!(!root.join("empty").exists());
        assert!(!root.join("empty/deeper").exists());
        assert!(root.join("keep/rom.bin").exists());
        assert!(root.exists(), "the source root is never removed");
        assert_eq!(report.dirs.len(), 2);
        assert!(
            report.dirs[0].ends_with("deeper"),
            "deepest dir reported first (removal order)"
        );
    }

    #[test]
    fn preview_does_not_touch_the_filesystem() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("a/b")).unwrap();

        let report = prune_sources(&[root.to_path_buf()], &opts(false, false)).unwrap();
        assert_eq!(report.dirs.len(), 2);
        // Nothing was actually removed.
        assert!(root.join("a/b").exists());
    }

    #[test]
    fn os_junk_only_dir_pruned_only_with_the_flag() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("g")).unwrap();
        fs::write(root.join("g/.DS_Store"), b"x").unwrap();

        // Without the flag a .DS_Store keeps the directory.
        let plain = prune_sources(&[root.to_path_buf()], &opts(false, false)).unwrap();
        assert!(plain.dirs.is_empty());

        // With the flag the dir is prunable and the cruft is deleted with it.
        let report = prune_sources(&[root.to_path_buf()], &opts(true, true)).unwrap();
        assert_eq!(report.dirs.len(), 1);
        assert_eq!(report.junk.len(), 1);
        assert!(!root.join("g").exists());
    }

    #[test]
    fn parent_cascades_when_all_children_are_junk_only() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // root/p/c/.DS_Store — with the flag, c is junk-only → removable, so p
        // becomes empty → also removable.
        fs::create_dir_all(root.join("p/c")).unwrap();
        fs::write(root.join("p/c/.DS_Store"), b"x").unwrap();

        let report = prune_sources(&[root.to_path_buf()], &opts(true, true)).unwrap();
        assert!(!root.join("p").exists(), "parent cascades to removal");
        assert_eq!(report.dirs.len(), 2);
    }
}
