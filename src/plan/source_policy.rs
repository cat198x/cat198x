use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

use crate::db::files::{self, Disposition};

/// Every source's disposition, keyed by its root path. This drives move-vs-copy
/// and duplicate-delete decisions throughout planning.
pub(crate) fn load_source_dispositions(conn: &Connection) -> Result<HashMap<String, Disposition>> {
    Ok(files::list_sources(conn)?
        .into_iter()
        .map(|s| (s.path, s.disposition))
        .collect())
}

/// Whether `path` is a file already placed under a destination library root, so
/// it must never be removed as a duplicate. A file in the library is the
/// canonical copy for its own game, not a stray staging copy.
pub(crate) fn is_in_library(path: &str, default_dest: Option<&str>, dest_root: &str) -> bool {
    let under = |root: &str| {
        let root = root.trim_end_matches('/');
        path == root || path.starts_with(&format!("{root}/"))
    };
    under(dest_root) || default_dest.is_some_and(under)
}

/// The disposition of the source at `source_root`; unknown roots are treated as
/// `preserve`, the safe default.
fn disposition_of(dispositions: &HashMap<String, Disposition>, source_root: &str) -> Disposition {
    dispositions
        .get(source_root)
        .copied()
        .unwrap_or(Disposition::Preserve)
}

/// Whether `dest` sits at or under `root`.
fn dest_under(root: &str, dest: &str) -> bool {
    let root = root.trim_end_matches('/');
    dest == root || dest.starts_with(&format!("{root}/"))
}

/// May content read from `source_root` be moved to `dest` rather than copied?
pub(crate) fn may_move(
    dispositions: &HashMap<String, Disposition>,
    source_root: &str,
    dest: &str,
) -> bool {
    match disposition_of(dispositions, source_root) {
        Disposition::Consume => true,
        Disposition::Preserve => dest_under(source_root, dest),
    }
}

/// May a file read from `source_root` be deleted as redundant, given a copy of
/// its content survives at `survivor_dest`?
pub(crate) fn may_delete(
    dispositions: &HashMap<String, Disposition>,
    source_root: &str,
    survivor_dest: &str,
) -> bool {
    match disposition_of(dispositions, source_root) {
        Disposition::Consume => true,
        Disposition::Preserve => dest_under(source_root, survivor_dest),
    }
}

/// The reason recorded on a dedup delete: the canonical copy that survives it.
pub(crate) fn dedup_reason(survivor_dest: &str) -> String {
    format!("exact duplicate — kept {survivor_dest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_in_library_protects_destination_files_not_staging() {
        let dd = Some("/lib/ROMs");
        // Any file under the library root is a placement — protected, even one
        // belonging to a different collection than the one being planned.
        assert!(is_in_library(
            "/lib/ROMs/MAME/g/r.bin",
            dd,
            "/lib/ROMs/FBNeo"
        ));
        assert!(is_in_library(
            "/lib/ROMs/FBNeo/g/r.bin",
            dd,
            "/lib/ROMs/FBNeo"
        ));
        // A staging copy outside every destination root is still removable.
        assert!(!is_in_library(
            "/Volumes/ToSort/x.zip",
            dd,
            "/lib/ROMs/FBNeo"
        ));
        // With no library-wide default, the collection's own dest_root still
        // protects its placements.
        assert!(is_in_library(
            "/lib/ROMs/FBNeo/g/r.bin",
            None,
            "/lib/ROMs/FBNeo"
        ));
        // A sibling path that merely shares a prefix is not "under" the root.
        assert!(!is_in_library("/lib/ROMs2/x", dd, "/lib/ROMs/FBNeo"));
    }
}
