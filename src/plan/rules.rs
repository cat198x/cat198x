use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

use super::generator::PlanOptions;
use super::matching::MatchedRom;
use crate::config::{MergeMode, OutputFormat};
use crate::db::config as db_config;
use crate::filter::{RomCandidate, parse_game_name, select_preferred};

/// Above this many match-rows, a collection is skipped rather than planned.
/// `find_matched_roms` materialises every (ROM x held-location) pair, at
/// roughly half a kilobyte each, so tens of millions of rows would need many
/// gigabytes and risk OOM. The only collections that reach this are MAME-style
/// meta-aggregates (e.g. `all_non-zipped_content`) whose "games" list content
/// held across hundreds of files -- not real romsets to place. The largest
/// legitimate set seen, FinalBurn Neo - Arcade Games, expands to ~7.9M rows,
/// comfortably under the cap.
pub(crate) const MAX_MATCH_ROWS: i64 = 20_000_000;

/// The effective output format: an explicit per-collection setting wins,
/// otherwise the library-wide default. An unrecognised string falls back to the
/// default rather than failing the whole plan.
pub(crate) fn resolve_output_format(explicit: Option<&str>, default: OutputFormat) -> OutputFormat {
    match explicit.map(str::to_ascii_lowercase).as_deref() {
        Some("loose") => OutputFormat::Loose,
        Some("zip") => OutputFormat::Zip,
        Some("torrentzip") => OutputFormat::TorrentZip,
        Some("7z") => OutputFormat::SevenZip,
        _ => default,
    }
}

/// The effective merge mode for a collection: an explicit setting
/// (per-collection or per-set) wins, otherwise the library-wide default. An
/// unrecognised string falls back to the default rather than failing the whole
/// plan. The kebab-case strings match the `MergeMode` serde representation in
/// `config::types`.
pub(crate) fn resolve_merge_mode(explicit: Option<&str>, default: MergeMode) -> MergeMode {
    match explicit.map(str::to_ascii_lowercase).as_deref() {
        Some("non-merged") => MergeMode::NonMerged,
        Some("merged") => MergeMode::Merged,
        Some("split") => MergeMode::Split,
        _ => default,
    }
}

/// The repack format tag for an archive format, or `None` for loose (which is
/// copied, not repacked).
pub(crate) fn archive_format_tag(format: OutputFormat) -> Option<&'static str> {
    match format {
        OutputFormat::Loose => None,
        OutputFormat::Zip => Some("zip"),
        OutputFormat::TorrentZip => Some("torrentzip"),
        OutputFormat::SevenZip => Some("7z"),
    }
}

/// The archive file extension for a repack format tag.
pub(crate) fn archive_extension(tag: &str) -> &'static str {
    if tag == "7z" { "7z" } else { "zip" }
}

/// The effective merge mode for a collection, in precedence order: an explicit
/// per-collection setting, then a per-set rule (a config row keyed on the set --
/// the top segment of the library path), then the library-wide default. Shared
/// by the planner and desired-state construction so the two never disagree on
/// which ROMs a game places.
pub(crate) fn effective_merge_mode(
    conn: &Connection,
    opts: &PlanOptions,
    cfg: Option<&db_config::CollectionConfig>,
    hierarchy: &str,
) -> Result<MergeMode> {
    let explicit_merge = cfg.and_then(|c| c.merge_mode.clone());
    let set_merge = match explicit_merge {
        Some(_) => None,
        None => {
            let set = hierarchy.split('/').next().unwrap_or(hierarchy);
            if set != hierarchy {
                db_config::get_collection_config(conn, set)?.and_then(|c| c.merge_mode)
            } else {
                None
            }
        }
    };
    Ok(resolve_merge_mode(
        explicit_merge.as_deref().or(set_merge.as_deref()),
        opts.default_merge_mode,
    ))
}

/// The effective output format for a collection, in the same precedence order
/// as [`effective_merge_mode`]: explicit per-collection, then per-set rule, then
/// the library-wide default.
pub(crate) fn effective_format(
    conn: &Connection,
    opts: &PlanOptions,
    cfg: Option<&db_config::CollectionConfig>,
    hierarchy: &str,
) -> Result<OutputFormat> {
    let explicit_format = cfg.and_then(|c| c.output_format.clone());
    let set_format = match explicit_format {
        Some(_) => None,
        None => {
            let set = hierarchy.split('/').next().unwrap_or(hierarchy);
            if set != hierarchy {
                db_config::get_collection_config(conn, set)?.and_then(|c| c.output_format)
            } else {
                None
            }
        }
    };
    Ok(resolve_output_format(
        explicit_format.as_deref().or(set_format.as_deref()),
        opts.default_format,
    ))
}

/// Simple glob pattern matching (case-insensitive).
///
/// Supports:
/// - `*` matches any sequence of characters (including empty)
/// - `?` matches exactly one character
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_impl(
        pattern.to_lowercase().as_bytes(),
        text.to_lowercase().as_bytes(),
    )
}

fn glob_match_impl(pattern: &[u8], text: &[u8]) -> bool {
    let mut p = 0;
    let mut t = 0;
    let mut star_p = None;
    let mut star_t = 0;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star_p = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

/// Apply 1G1R filtering to a list of matched ROMs.
///
/// Groups ROMs by their base title (extracted from game_name) and selects the
/// preferred variant based on region priority and dump quality.
pub(crate) fn apply_one_g_one_r_filter(
    matches: &[MatchedRom],
    prefs: &crate::filter::FilterPreferences,
) -> Vec<MatchedRom> {
    let mut groups: HashMap<String, Vec<&MatchedRom>> = HashMap::new();

    for m in matches {
        let parsed = parse_game_name(&m.game_name);
        groups.entry(parsed.title).or_default().push(m);
    }

    let mut result = Vec::new();

    for (_title, group) in groups {
        if group.len() == 1 {
            let m = group[0];
            let parsed = parse_game_name(&m.game_name);
            if !prefs.should_exclude(&parsed) {
                result.push(m.clone());
            }
        } else {
            let candidates: Vec<_> = group
                .iter()
                .map(|m| RomCandidate::new(&m.game_name))
                .collect();

            if let Some(preferred_name) = select_preferred(&candidates, prefs)
                && let Some(m) = group.iter().find(|m| m.game_name == preferred_name)
            {
                result.push((*m).clone());
            }
        }
    }

    result
}
