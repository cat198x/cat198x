use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::dat::parse_dat_file_auto;

use super::collect_dat_files;

/// The nested sub-path for a DAT, from its collection name split on " - " -
/// e.g. "Acorn BBC - Magazines - Laserbug" -> ["Acorn BBC", "Magazines",
/// "Laserbug"]. Each segment is trimmed and any path separator neutralised.
///
/// Note: this is a `" - "`-segmented layout. It does *not* further split the
/// leading manufacturer/system token (so "Acorn BBC" stays one folder, not
/// "Acorn/BBC"). The result is internally consistent - a recursive `dat add`
/// then a reorg lay files out to match it - but it is not TOSEC's traditional
/// hand-split tree.
pub(super) fn sort_segments(collection_name: &str) -> Vec<String> {
    collection_name
        .split(" - ")
        .map(|s| s.trim().replace(['/', '\\'], "_"))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Sort a flat pack of DAT files into a nested tree under `dest`, placing each
/// DAT at `dest/<name segments>/<filename>`. Copies (the pack is left intact)
/// and skips files already present, so it is safe to re-run.
pub(super) fn sort_dats(pack: &Path, dest: &Path) -> Result<()> {
    if !pack.is_dir() {
        anyhow::bail!("Source pack is not a directory: {}", pack.display());
    }

    let dat_files = collect_dat_files(pack);
    if dat_files.is_empty() {
        println!("No .dat or .xml files found under {}", pack.display());
        return Ok(());
    }

    let mut sorted = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<(PathBuf, String)> = Vec::new();

    for file in &dat_files {
        let result = (|| -> Result<bool> {
            let (header, _) = parse_dat_file_auto(file)?;
            let segments = sort_segments(&header.name);
            if segments.is_empty() {
                anyhow::bail!("empty collection name");
            }
            let file_name = file
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("no file name"))?;

            let mut target_dir = dest.to_path_buf();
            for seg in &segments {
                target_dir.push(seg);
            }
            let target = target_dir.join(file_name);

            if target.exists() {
                return Ok(false); // already sorted
            }
            std::fs::create_dir_all(&target_dir)
                .with_context(|| format!("Failed to create directory: {}", target_dir.display()))?;
            std::fs::copy(file, &target)
                .with_context(|| format!("Failed to copy to {}", target.display()))?;
            Ok(true)
        })();

        match result {
            Ok(true) => sorted += 1,
            Ok(false) => skipped += 1,
            Err(e) => failures.push((file.clone(), e.to_string())),
        }
    }

    println!("Sorted {} DAT file(s) into {}.", sorted, dest.display());
    if skipped > 0 {
        println!("{} already present, skipped.", skipped);
    }
    if !failures.is_empty() {
        println!("{} file(s) failed:", failures.len());
        for (file, err) in &failures {
            println!("  {}: {}", file.display(), err);
        }
    }
    println!();
    println!("Next: cat198x dat add -r {}", dest.display());

    Ok(())
}
