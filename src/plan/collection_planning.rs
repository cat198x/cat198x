use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use super::archive_planning::{ArchivePlanInputs, ArchivePlanSinks, plan_archive_matches};
use super::container_drains::ContainerDrains;
use super::destinations::resolve_dest_root;
use super::matching::{MatchedRom, count_match_rows_capped, find_matched_roms};
use super::options::PlanOptions;
use super::placement_planning::{PlacementPlanCounts, plan_disk_matches, plan_loose_matches};
use super::reporting;
use super::rules::{
    MAX_MATCH_ROWS, apply_one_g_one_r_filter, archive_extension, archive_format_tag,
    effective_format, effective_merge_mode,
};
use super::{CollectionPlanStat, Plan};
use crate::config::{MergeMode, OutputFormat};
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

#[derive(Default)]
struct CollectionPlanAccumulator {
    already_correct: usize,
    to_write: usize,
    relocated: usize,
    deduped: usize,
    bytes: u64,
}

impl CollectionPlanAccumulator {
    fn add_loose(&mut self, counts: PlacementPlanCounts) {
        self.already_correct += counts.already_correct;
        self.to_write += counts.to_write;
        self.deduped += counts.deduped;
        self.bytes += counts.bytes;
    }

    fn add_archive(&mut self, counts: PlacementPlanCounts) {
        self.already_correct += counts.already_correct;
        self.relocated += counts.relocated;
        self.to_write += counts.to_write;
        self.deduped += counts.deduped;
        self.bytes += counts.bytes;
    }

    fn add_disk(&mut self, counts: PlacementPlanCounts) {
        self.already_correct += counts.already_correct;
        self.to_write += counts.to_write;
        self.bytes += counts.bytes;
    }

    fn record_on_plan(self, plan: &mut Plan, name: String, node_path: String) {
        plan.summary.already_correct += self.already_correct;
        plan.per_collection.push(CollectionPlanStat {
            name,
            node_path,
            to_write: self.to_write,
            already_correct: self.already_correct,
            bytes: self.bytes,
        });
    }
}

pub(crate) fn plan_collection(
    ctx: &CollectionPlanningContext<'_>,
    collection: &collections::Collection,
    plan: &mut Plan,
    container_drains: &mut ContainerDrains,
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

    // CHDs (<disk> entries) are always stored loose in a machine folder
    // (<dest>/<game>/<name>.chd) and never packed, even when the set's format is
    // an archive — so plan them on their own path and run the format branch over
    // the remaining <rom> entries only.
    let acc = plan_collection_matches(ctx, matches, format, &dest_root, plan, container_drains)?;

    acc.record_on_plan(plan, collection.name.clone(), hierarchy);

    Ok(CollectionPlanningOutcome::Planned)
}

fn plan_collection_matches(
    ctx: &CollectionPlanningContext<'_>,
    matches: Vec<MatchedRom>,
    format: OutputFormat,
    dest_root: &str,
    plan: &mut Plan,
    container_drains: &mut ContainerDrains,
) -> Result<CollectionPlanAccumulator> {
    let mut acc = CollectionPlanAccumulator::default();

    let (disk_matches, matches): (Vec<MatchedRom>, Vec<MatchedRom>) =
        matches.into_iter().partition(|m| m.is_disk);

    match archive_format_tag(format) {
        None => {
            let c = plan_loose_matches(
                matches,
                dest_root,
                ctx.default_dest,
                ctx.shared,
                ctx.dispositions,
                plan,
            )?;
            acc.add_loose(c);
            reporting::loose_summary(acc.already_correct, acc.to_write, acc.deduped);
        }
        Some(tag) => {
            let ext = archive_extension(tag);
            let c = plan_archive_matches(
                matches,
                ArchivePlanInputs {
                    tag,
                    ext,
                    dest_root,
                    default_dest: ctx.default_dest,
                    shared: ctx.shared,
                    shared_containers: ctx.shared_containers,
                    dispositions: ctx.dispositions,
                },
                ArchivePlanSinks {
                    plan,
                    container_drains,
                },
            )?;
            acc.add_archive(c);
            reporting::archive_summary(
                acc.already_correct,
                acc.relocated,
                acc.to_write,
                acc.deduped,
            );
        }
    }

    // Plan any CHDs loose, regardless of the set's format. Disk dedups are
    // reported within the helper, like the other branches' own counts.
    if !disk_matches.is_empty() {
        let d = plan_disk_matches(
            disk_matches,
            dest_root,
            ctx.opts,
            ctx.shared,
            ctx.dispositions,
            plan,
        )?;
        acc.add_disk(d);
    }

    Ok(acc)
}
