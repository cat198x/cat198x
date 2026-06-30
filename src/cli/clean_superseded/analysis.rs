use anyhow::Result;
use std::collections::{HashMap, HashSet};

use crate::db::files::{self, Source};
use crate::plan::PlanOptions;
use crate::plan::compute_desired_state;

/// A loose file under the library, a candidate for removal.
#[derive(Debug, Clone)]
pub(super) struct Candidate {
    /// Absolute path on disk.
    pub(super) abs_path: String,
    /// Content SHA1 (the catalogue's native upper-case form).
    pub(super) sha1: String,
    /// Bytes freed by removing it.
    pub(super) size: i64,
}

/// The outcome of analysing the library's loose layer against the desired state.
pub(super) struct CleanupReport {
    /// Loose files safe to remove (all four conditions hold).
    pub(super) targets: Vec<Candidate>,
    /// Every loose file examined.
    pub(super) total_files: usize,
    /// Bytes held by every loose file examined.
    pub(super) total_bytes: i64,
    /// Bytes freed by removing the targets.
    pub(super) removable_bytes: i64,
}

/// Every loose file physically under the library root, optionally restricted to
/// the given sets (the first path segment beneath the library, e.g. `MAME`).
fn collect_loose_under_library(
    conn: &rusqlite::Connection,
    sources: &[Source],
    library: &str,
    set_filter: Option<&[String]>,
) -> Result<Vec<Candidate>> {
    let lib_prefix = format!("{}/", library);
    let mut out = Vec::new();
    for s in sources {
        let root = s.path.trim_end_matches('/');
        // Only sources at or beneath the library hold the loose layer.
        if root != library && !root.starts_with(&lib_prefix) {
            continue;
        }
        let mut stmt = conn.prepare(
            "SELECT fl.path, fl.sha1, f.size
               FROM file_locations fl
               JOIN files f ON f.sha1 = fl.sha1
              WHERE fl.source_id = ?1 AND fl.archive_path IS NULL",
        )?;
        let rows = stmt.query_map([s.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (rel, sha1, size) = row?;
            let abs_path = format!("{}/{}", root, rel);
            if let Some(sets) = set_filter {
                let under = abs_path.strip_prefix(&lib_prefix).unwrap_or(&abs_path);
                let seg = under.split('/').next().unwrap_or(under);
                if !sets.iter().any(|s| s == seg) {
                    continue;
                }
            }
            out.push(Candidate {
                abs_path,
                sha1,
                size,
            });
        }
    }
    Ok(out)
}

/// The candidate contents whose canonical archive both is designated by the
/// active DAT (`archive_homes`) and is catalogued under the library holding that
/// content - conditions 1 and 2 together. A content with no DAT-assigned archive
/// home (version-gap residue) is never returned.
fn removable_contents(
    conn: &rusqlite::Connection,
    sources: &[Source],
    library: &str,
    archive_homes: &HashMap<String, HashSet<String>>,
    interesting: &HashSet<String>,
) -> Result<HashSet<String>> {
    let lib_prefix = format!("{}/", library);
    let mut removable = HashSet::new();
    for sha1 in interesting {
        // Condition 2: the active DAT assigns this content to a canonical archive.
        let Some(homes) = archive_homes.get(sha1) else {
            continue;
        };
        // Condition 1: that specific archive is catalogued under the library
        // holding this content.
        for loc in files::get_file_locations(conn, sha1)? {
            if loc.archive_path.is_none() {
                continue; // a loose copy is not the canonical archive
            }
            let Some(root) = sources
                .iter()
                .find(|s| s.id == loc.source_id)
                .map(|s| s.path.trim_end_matches('/').to_string())
            else {
                continue;
            };
            let container_abs = format!("{}/{}", root, loc.path);
            if container_abs != *library && !container_abs.starts_with(&lib_prefix) {
                continue; // an archive outside the library is not the canonical home
            }
            if homes.contains(&container_abs) {
                removable.insert(sha1.clone());
                break;
            }
        }
    }
    Ok(removable)
}

/// Analyse the library's loose layer: which loose files are safe to remove
/// because their content is preserved in the canonical archive the active DAT
/// assigns it to, and which are left untouched.
pub(super) fn analyze(
    conn: &rusqlite::Connection,
    sources: &[Source],
    library: &str,
    default_format: crate::config::OutputFormat,
    default_merge_mode: crate::config::MergeMode,
    set_filter: Option<&[String]>,
) -> Result<CleanupReport> {
    let candidates = collect_loose_under_library(conn, sources, library, set_filter)?;
    let total_files = candidates.len();
    let total_bytes: i64 = candidates.iter().map(|c| c.size).sum();

    let interesting: HashSet<String> = candidates.iter().map(|c| c.sha1.clone()).collect();

    // Desired state across ALL active collections (unfiltered): a file canonical
    // under any collection must not be removed, so the safety sets are global
    // regardless of which sets the candidate scan was narrowed to.
    let opts = PlanOptions {
        dat_filter: None,
        set_filter: None,
        default_dest: Some(library.to_string()),
        default_format,
        default_merge_mode,
    };
    let desired = compute_desired_state(conn, &opts, &interesting)?;
    let removable_sha1 =
        removable_contents(conn, sources, library, &desired.archive_homes, &interesting)?;

    let mut targets: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| {
            // Conditions 1+2: content preserved in its canonical archive; and
            // condition 3: the file is not itself a desired-state destination.
            removable_sha1.contains(&c.sha1) && !desired.dest_paths.contains(&c.abs_path)
        })
        .collect();
    targets.sort_by(|a, b| a.abs_path.cmp(&b.abs_path));
    let removable_bytes: i64 = targets.iter().map(|c| c.size).sum();

    Ok(CleanupReport {
        targets,
        total_files,
        total_bytes,
        removable_bytes,
    })
}
