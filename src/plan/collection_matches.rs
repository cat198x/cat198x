use anyhow::Result;
use rusqlite::Connection;

use super::matching::{MatchedRom, find_matched_roms};
use super::rules::apply_one_g_one_r_filter;
use crate::config::MergeMode;
use crate::db::config as db_config;

pub(crate) struct CollectionMatchInputs<'a> {
    pub(crate) conn: &'a Connection,
    pub(crate) version_id: i64,
    pub(crate) collection_name: &'a str,
    pub(crate) merge_mode: MergeMode,
    pub(crate) cfg: Option<&'a db_config::CollectionConfig>,
    /// Per-content holder cap for a pathological oversized collection: `None`
    /// leaves the match query uncapped (the normal path), `Some(n)` bounds each
    /// content to its first `n` holder-locations. Set by the size guard.
    pub(crate) location_cap: Option<i64>,
}

pub(crate) struct FilteredCollectionMatches {
    pub(crate) matches: Vec<MatchedRom>,
    pub(crate) original_count: usize,
}

pub(crate) fn load_collection_matches(
    inputs: CollectionMatchInputs<'_>,
) -> Result<FilteredCollectionMatches> {
    // Find all matched ROMs for this version. In split mode, a clone's
    // merge-tagged inherited ROMs are excluded here (they belong to the parent),
    // so the clone is placed with only its own unique ROMs.
    let matches = find_matched_roms(
        inputs.conn,
        inputs.version_id,
        inputs.collection_name,
        inputs.merge_mode == MergeMode::Split,
        inputs.location_cap,
    )?;

    Ok(apply_collection_filter(matches, inputs.cfg))
}

fn apply_collection_filter(
    matches: Vec<MatchedRom>,
    cfg: Option<&db_config::CollectionConfig>,
) -> FilteredCollectionMatches {
    let original_count = matches.len();
    let Some(extra) = cfg.and_then(|c| c.extra_config.as_ref()) else {
        return FilteredCollectionMatches {
            matches,
            original_count,
        };
    };

    if !extra.one_g_one_r {
        return FilteredCollectionMatches {
            matches,
            original_count,
        };
    }

    let prefs = extra.to_filter_preferences();
    let matches = apply_one_g_one_r_filter(&matches, &prefs);
    FilteredCollectionMatches {
        matches,
        original_count,
    }
}
