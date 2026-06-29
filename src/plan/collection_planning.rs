use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::archive_planning::{ContainerDrain, plan_archive_matches};
use super::destinations::resolve_dest_root;
use super::generator::PlanOptions;
use super::matching::{MatchedRom, count_match_rows_capped, find_matched_roms};
use super::placement_planning::{plan_disk_matches, plan_loose_matches};
use super::reporting;
use super::rules::{
    MAX_MATCH_ROWS, apply_one_g_one_r_filter, archive_extension, archive_format_tag,
    effective_format, effective_merge_mode,
};
use super::{CollectionPlanStat, Plan};
use crate::config::MergeMode;
use crate::db::files::Disposition;
use crate::db::{collections, config as db_config, dats};

pub(crate) struct CollectionPlanningContext<'a> {
    pub(crate) conn: &'a Connection,
    pub(crate) opts: &'a PlanOptions,
    pub(crate) default_dest: Option<&'a str>,
    pub(crate) shared: &'a HashSet<String>,
    pub(crate) shared_containers: &'a HashSet<String>,
    pub(crate) dispositions: &'a HashMap<String, Disposition>,
}

pub(crate) enum CollectionPlanningOutcome {
    NoActiveVersion,
    ExcludedBySet,
    SkippedNoDest(String),
    SkippedOversized(String),
    Planned,
}

pub(crate) fn plan_collection(
    ctx: &CollectionPlanningContext<'_>,
    collection: &collections::Collection,
    plan: &mut Plan,
    drain_after_repack: &mut BTreeMap<String, ContainerDrain>,
) -> Result<CollectionPlanningOutcome> {
    // Only collections with an active version can be planned.
    let version = match collections::get_active_version(ctx.conn, collection.id)? {
        Some(v) => v,
        None => return Ok(CollectionPlanningOutcome::NoActiveVersion),
    };

    let cfg = db_config::get_collection_config(ctx.conn, &collection.name)?;

    // The collection's library path (set by recursive `dat add`), used when
    // falling back to the library-wide default destination.
    let hierarchy =
        dats::primary_node_path(ctx.conn, version.id)?.unwrap_or_else(|| collection.name.clone());

    // Restrict to requested sets (the top segment of the library path), so a
    // phase can target e.g. just TOSEC without the arcade sets. Checked before
    // the match query so excluded collections cost nothing.
    if let Some(sets) = ctx.opts.set_filter.as_ref() {
        let set = hierarchy.split('/').next().unwrap_or(hierarchy.as_str());
        if !sets.iter().any(|s| s == set) {
            return Ok(CollectionPlanningOutcome::ExcludedBySet);
        }
    }

    let explicit = cfg.as_ref().and_then(|c| c.dest_path.as_deref());

    let dest_root = match resolve_dest_root(explicit, ctx.default_dest, &hierarchy)? {
        Some(root) => root,
        None => {
            // No destination resolved — recorded and reported, never silent.
            return Ok(CollectionPlanningOutcome::SkippedNoDest(
                collection.name.clone(),
            ));
        }
    };

    // Guard against pathological collections before materialising any matches:
    // a MAME-style meta-aggregate expands to tens of millions of match-rows and
    // would exhaust memory. Skip-and-report instead of OOM.
    let match_rows = count_match_rows_capped(ctx.conn, version.id, MAX_MATCH_ROWS)?;
    if match_rows > MAX_MATCH_ROWS {
        reporting::oversized_collection(&collection.name);
        return Ok(CollectionPlanningOutcome::SkippedOversized(format!(
            "{} (>{} match-rows)",
            collection.name, MAX_MATCH_ROWS
        )));
    }

    reporting::planning_collection(&collection.name, &version.version);

    // Effective merge mode (explicit per-collection → per-set rule →
    // library-wide default). Split mode drops a clone's inherited (merge-tagged)
    // ROMs from its placement so they live only in the parent; non-merged places
    // every ROM the DAT lists per game. Merged is not yet wired in the planner.
    // Shared with `compute_desired_state`.
    let merge_mode = effective_merge_mode(ctx.conn, ctx.opts, cfg.as_ref(), &hierarchy)?;
    if merge_mode == MergeMode::Merged {
        reporting::merged_mode_not_implemented(&collection.name);
    }

    // Find all matched ROMs for this version. In split mode, a clone's
    // merge-tagged inherited ROMs are excluded here (they belong to the parent),
    // so the clone is placed with only its own unique ROMs.
    let matches = find_matched_roms(
        ctx.conn,
        version.id,
        &collection.name,
        merge_mode == MergeMode::Split,
    )?;

    // Apply 1G1R filtering if enabled for this collection.
    let matches = match cfg.as_ref().and_then(|c| c.extra_config.as_ref()) {
        Some(extra) if extra.one_g_one_r => {
            let prefs = extra.to_filter_preferences();
            let original_count = matches.len();
            let filtered = apply_one_g_one_r_filter(&matches, &prefs);
            if filtered.len() < original_count {
                reporting::one_g_one_r(original_count, filtered.len());
            }
            filtered
        }
        _ => matches,
    };

    // Effective output format (explicit per-collection → per-set rule →
    // library-wide default). The per-set tier lets whole sets diverge — TOSEC
    // kept as zip, TOSEC-PIX left loose for later PDF/collateral extraction —
    // without configuring every collection. Loose copies each ROM into place;
    // zip/torrentzip packs each game into one archive. Shared with
    // `compute_desired_state`.
    let format = effective_format(ctx.conn, ctx.opts, cfg.as_ref(), &hierarchy)?;

    let mut already_correct = 0;
    let mut to_write = 0;
    let mut relocated = 0;
    let mut deduped = 0;
    let mut bytes = 0u64;

    // CHDs (<disk> entries) are always stored loose in a machine folder
    // (<dest>/<game>/<name>.chd) and never packed, even when the set's format is
    // an archive — so plan them on their own path and run the format branch over
    // the remaining <rom> entries only.
    let (disk_matches, matches): (Vec<MatchedRom>, Vec<MatchedRom>) =
        matches.into_iter().partition(|m| m.is_disk);

    match archive_format_tag(format) {
        None => {
            let c = plan_loose_matches(
                matches,
                &dest_root,
                ctx.default_dest,
                ctx.shared,
                ctx.dispositions,
                plan,
            )?;
            already_correct += c.already_correct;
            to_write += c.to_write;
            bytes += c.bytes;
            deduped += c.deduped;
            reporting::loose_summary(already_correct, to_write, deduped);
        }
        Some(tag) => {
            let ext = archive_extension(tag);
            let c = plan_archive_matches(
                matches,
                tag,
                ext,
                &dest_root,
                ctx.default_dest,
                ctx.shared,
                ctx.shared_containers,
                ctx.dispositions,
                plan,
                drain_after_repack,
            )?;
            already_correct += c.already_correct;
            relocated += c.relocated;
            to_write += c.to_write;
            bytes += c.bytes;
            deduped += c.deduped;
            reporting::archive_summary(already_correct, relocated, to_write, deduped);
        }
    }

    // Plan any CHDs loose, regardless of the set's format. (Disk dedups are
    // reported within the helper, like the other branches' own counts.)
    if !disk_matches.is_empty() {
        let d = plan_disk_matches(
            disk_matches,
            &dest_root,
            ctx.opts,
            ctx.shared,
            ctx.dispositions,
            plan,
        )?;
        already_correct += d.already_correct;
        to_write += d.to_write;
        bytes += d.bytes;
    }

    plan.summary.already_correct += already_correct;
    plan.per_collection.push(CollectionPlanStat {
        name: collection.name.clone(),
        node_path: hierarchy,
        to_write,
        already_correct,
        bytes,
    });

    Ok(CollectionPlanningOutcome::Planned)
}
