//! `clean-superseded` command — remove loose files stranded under the library
//! beside their canonical per-machine archive.
//!
//! A re-layout (e.g. the MAME loose → per-machine-zip split) leaves the previous
//! layout's files under the library, next to the new canonical archives that now
//! hold the same content. The planner cannot collect them: it is additive and
//! its `is_in_library` guard refuses to delete anything under a destination root.
//! This command does — but only where the removal is provably safe.
//!
//! Safety model (hard delete has no undo) — a loose file under the library is
//! removed only when all four conditions hold:
//!
//! 1. its content sits in the canonical archive the active DAT assigns it to, and
//!    that archive is catalogued holding the content (a match against that
//!    specific archive, not a bare same-SHA1-exists-somewhere check);
//! 2. that canonical archive is itself a current desired-state member of the
//!    collection (it is the archive an active DAT game places);
//! 3. the file being removed is not itself a current desired-state member of any
//!    collection (it is not a canonical destination the library wants kept); and
//! 4. the surviving copy is re-verified on disk at delete time, via the shared
//!    verify-before-delete net `apply` uses.
//!
//! Version-gap residue — content the active DAT no longer lists — sits in no
//! canonical archive, so it fails condition 1 and is left untouched. Like
//! `reclaim` and `prune-empty`, this reports by default and only deletes under
//! `--execute`, journaling what it removed.

mod analysis;
mod execution;

use anyhow::Result;
use std::path::PathBuf;

use crate::config::Config;
use crate::db::files;
use crate::util::format_bytes;

use super::{get_data_dir, open_database};
use analysis::analyze;
use execution::execute_cleanup;

#[cfg(test)]
use std::collections::HashSet;

#[cfg(test)]
use crate::db::files::Source;

/// Run the clean-superseded command.
pub fn run(
    set_filter: Option<Vec<String>>,
    execute: bool,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();

    // The library root the loose layer sits under is the library-wide default
    // destination; without it there is no "under the library" to clean.
    let config_path = get_data_dir(data_dir.clone())?.join("config.toml");
    let file_config = if config_path.exists() {
        Config::load(&config_path).unwrap_or_default()
    } else {
        Config::default()
    };
    let Some(library) = file_config.default_dest_path.clone() else {
        anyhow::bail!(
            "clean-superseded needs a library root. Set it with:\n  \
             cat198x config set-default dest_path <path>"
        );
    };
    let library = library.trim_end_matches('/').to_string();

    let sources = files::list_sources(conn)?;

    println!("Examining the loose layer under {} …", library);
    let report = analyze(
        conn,
        &sources,
        &library,
        file_config.default_output_format,
        file_config.default_merge_mode,
        set_filter.as_deref(),
    )?;

    if report.total_files == 0 {
        println!("No loose files under the library to examine.");
        return Ok(());
    }

    let kept = report.total_files - report.targets.len();
    let kept_bytes = report.total_bytes - report.removable_bytes;
    println!();
    println!(
        "Loose files under the library: {} ({})",
        report.total_files,
        format_bytes(report.total_bytes.max(0) as u64)
    );
    println!(
        "  Removable — content preserved in its canonical archive: {} ({})",
        report.targets.len(),
        format_bytes(report.removable_bytes.max(0) as u64)
    );
    println!(
        "  Left untouched — content in no canonical archive, or itself canonical: {} ({})",
        kept,
        format_bytes(kept_bytes.max(0) as u64)
    );

    if report.targets.is_empty() {
        println!();
        println!("Nothing to clean.");
        return Ok(());
    }

    if !execute {
        println!();
        for c in report.targets.iter().take(20) {
            println!("  would remove  {}", c.abs_path);
        }
        if report.targets.len() > 20 {
            println!("  … and {} more", report.targets.len() - 20);
        }
        println!();
        println!("Dry run — nothing deleted. Re-run with --execute to free the space.");
        return Ok(());
    }

    let execution = execute_cleanup(conn, &sources, &report.targets, data_dir)?;

    println!();
    println!(
        "Removed {} file(s), freed {}{}.",
        execution.removed_count,
        format_bytes(execution.freed_bytes.max(0) as u64),
        if execution.skipped > 0 {
            format!(" ({} skipped — survivor not verified)", execution.skipped)
        } else {
            String::new()
        }
    );
    println!("Audit log: {}", execution.log_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, collections, dats};

    fn src(id: i64, path: &str) -> Source {
        Source {
            id,
            path: path.to_string(),
            case_sensitive: false,
            added_at: String::new(),
            last_scanned: None,
            disposition: crate::db::files::Disposition::Preserve,
        }
    }

    /// A MAME-style split fixture mirroring the real case: a parent (puckman) and
    /// a clone (pacmanm) whose inherited shared ROM lives in the parent's archive
    /// under split. The library holds the canonical per-machine zips, the stranded
    /// loose layer beside them, and one version-gap orphan in no archive.
    fn setup_split_library(conn: &rusqlite::Connection) {
        let coll = collections::create_collection(conn, "MAME", "mame").unwrap();
        let vid = collections::add_version(conn, coll, "v1", "/dats/mame.dat", true).unwrap();
        let node = dats::create_node(conn, vid, None, "MAME", "dat", "MAME").unwrap();

        let parent =
            dats::create_game(conn, node, "puckman", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            parent,
            "shared.rom",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        let clone = dats::create_game(
            conn,
            node,
            "pacmanm",
            None,
            Some("puckman"),
            false,
            false,
            false,
        )
        .unwrap();
        // shared.rom is inherited (merge-tagged → parent under split); clone.rom is unique.
        dats::create_rom(
            conn,
            clone,
            "shared.rom",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            Some("shared.rom"),
        )
        .unwrap();
        dats::create_rom(
            conn,
            clone,
            "clone.rom",
            10,
            Some("BBB"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO files (sha1, size) VALUES ('AAA',10),('BBB',10),('ZZZ',999)",
            [],
        )
        .unwrap();
        // Library source at the default-dest root.
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (33, '/lib/ROMs', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                -- canonical per-machine archives (the survivors)
                ('AAA', 33, 'MAME/puckman.zip', 'shared.rom'),
                ('BBB', 33, 'MAME/pacmanm.zip', 'clone.rom'),
                -- stranded loose layer beside them
                ('AAA', 33, 'MAME/puckman/shared.rom', NULL),
                ('AAA', 33, 'MAME/pacmanm/shared.rom', NULL),
                ('BBB', 33, 'MAME/pacmanm/clone.rom', NULL),
                -- a version-gap orphan: its content is in no canonical archive
                ('ZZZ', 33, 'MAME/oldgame/x.rom', NULL)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn removes_loose_layer_keeps_version_gap_orphan() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        setup_split_library(conn);

        let report = analyze(
            conn,
            &[src(33, "/lib/ROMs")],
            "/lib/ROMs",
            crate::config::OutputFormat::Zip,
            crate::config::MergeMode::Split,
            None,
        )
        .unwrap();

        let paths: HashSet<&str> = report.targets.iter().map(|c| c.abs_path.as_str()).collect();

        assert_eq!(report.total_files, 4, "four loose files examined");
        assert_eq!(
            report.targets.len(),
            3,
            "three are redundant with a canonical zip"
        );
        // The clone's inherited loose copy is preserved by the PARENT's zip
        // (split), so it is removable even though pacmanm.zip does not hold it.
        assert!(paths.contains("/lib/ROMs/MAME/pacmanm/shared.rom"));
        assert!(paths.contains("/lib/ROMs/MAME/puckman/shared.rom"));
        assert!(paths.contains("/lib/ROMs/MAME/pacmanm/clone.rom"));
        // The version-gap orphan's content is in no canonical archive → kept.
        assert!(
            !paths.contains("/lib/ROMs/MAME/oldgame/x.rom"),
            "content in no canonical archive is left untouched"
        );
    }

    #[test]
    fn never_removes_a_file_that_is_itself_a_canonical_destination() {
        // A loose-format collection whose canonical destination IS a loose file:
        // that file must never be removed, even if its content also sits in an
        // archive elsewhere (condition 3 — not a desired member of any collection).
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();

        // Archive collection MAME places AAA canonically in an archive.
        let mame = collections::create_collection(conn, "MAME", "mame").unwrap();
        let mvid = collections::add_version(conn, mame, "v1", "/d/mame.dat", true).unwrap();
        let mnode = dats::create_node(conn, mvid, None, "MAME", "dat", "MAME").unwrap();
        let g = dats::create_game(conn, mnode, "puckman", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            g,
            "shared.rom",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Loose collection Tapes places AAA canonically as a loose file.
        let tapes = collections::create_collection(conn, "Tapes", "tosec").unwrap();
        let tvid = collections::add_version(conn, tapes, "v1", "/d/tapes.dat", true).unwrap();
        let tnode = dats::create_node(conn, tvid, None, "Tapes", "dat", "Tapes").unwrap();
        let tg = dats::create_game(conn, tnode, "solo", None, None, false, false, false).unwrap();
        dats::create_rom(
            conn,
            tg,
            "solo.tap",
            10,
            Some("AAA"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA',10)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (33, '/lib/ROMs', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('AAA', 33, 'MAME/puckman.zip', 'shared.rom'),
                -- this loose file IS the canonical Tapes destination for AAA
                ('AAA', 33, 'Tapes/solo.tap', NULL)",
            [],
        )
        .unwrap();

        let report = analyze(
            conn,
            &[src(33, "/lib/ROMs")],
            "/lib/ROMs",
            crate::config::OutputFormat::Loose,
            crate::config::MergeMode::NonMerged,
            None,
        )
        .unwrap();

        assert!(
            report.targets.is_empty(),
            "a file that is itself a canonical destination is never removed, \
             even though its content is preserved in a MAME archive"
        );
    }

    #[test]
    fn set_filter_scopes_the_candidate_scan() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        setup_split_library(conn);

        // Restricting to a set with no loose layer finds nothing to clean.
        let report = analyze(
            conn,
            &[src(33, "/lib/ROMs")],
            "/lib/ROMs",
            crate::config::OutputFormat::Zip,
            crate::config::MergeMode::Split,
            Some(&["TOSEC".to_string()]),
        )
        .unwrap();
        assert_eq!(report.total_files, 0, "no loose files under the TOSEC set");
    }
}
