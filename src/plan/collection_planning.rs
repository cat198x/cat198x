use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use super::archive_planning::{ArchivePlanInputs, ArchivePlanSinks, plan_archive_matches};
use super::collection_matches::{CollectionMatchInputs, load_collection_matches};
use super::collection_scope::{ScopedCollectionResolution, resolve_scoped_collection};
use super::collection_settings::resolve_collection_settings;
use super::container_drains::ContainerDrains;
use super::matching::MatchedRom;
use super::options::PlanOptions;
use super::placement_planning::{PlacementPlanCounts, plan_disk_matches, plan_loose_matches};
use super::reporting;
use super::rules::{
    MAX_MATCH_ROWS, OversizedDecision, archive_extension, archive_format_tag, oversized_decision,
};
use super::{CollectionPlanStat, Plan};
use crate::config::{MergeMode, OutputFormat};
use crate::db::collections;
use crate::db::files::Disposition;

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

enum CollectionPlanPreparation {
    Ready(PreparedCollectionPlan),
    Skipped(CollectionPlanningOutcome),
}

struct PreparedCollectionPlan {
    name: String,
    hierarchy: String,
    dest_root: String,
    matches: Vec<MatchedRom>,
    format: OutputFormat,
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
    let prepared = match prepare_collection_plan(ctx, collection)? {
        CollectionPlanPreparation::Ready(prepared) => prepared,
        CollectionPlanPreparation::Skipped(outcome) => return Ok(outcome),
    };

    // CHDs (<disk> entries) are always stored loose in a machine folder
    // (<dest>/<game>/<name>.chd) and never packed, even when the set's format is
    // an archive — so plan them on their own path and run the format branch over
    // the remaining <rom> entries only.
    let acc = plan_collection_matches(
        ctx,
        prepared.matches,
        prepared.format,
        &prepared.dest_root,
        plan,
        container_drains,
    )?;

    acc.record_on_plan(plan, prepared.name, prepared.hierarchy);

    Ok(CollectionPlanningOutcome::Planned)
}

fn prepare_collection_plan(
    ctx: &CollectionPlanningContext<'_>,
    collection: &collections::Collection,
) -> Result<CollectionPlanPreparation> {
    let scoped = match resolve_scoped_collection(ctx.conn, ctx.opts, ctx.default_dest, collection)?
    {
        ScopedCollectionResolution::Resolved(scoped) => *scoped,
        ScopedCollectionResolution::NoActiveVersion => {
            return Ok(CollectionPlanPreparation::Skipped(
                CollectionPlanningOutcome::NoActiveVersion,
            ));
        }
        ScopedCollectionResolution::ExcludedBySet => {
            return Ok(CollectionPlanPreparation::Skipped(
                CollectionPlanningOutcome::ExcludedBySet,
            ));
        }
    };

    let dest_root = match scoped.dest_root {
        Some(root) => root,
        None => {
            // No destination resolved — recorded and reported, never silent.
            return Ok(CollectionPlanPreparation::Skipped(
                CollectionPlanningOutcome::SkippedNoDest(scoped.name),
            ));
        }
    };

    let location_cap = match collection_size_guard(ctx, scoped.version.id, &scoped.name)? {
        CollectionSizeGuard::Skip(outcome) => {
            return Ok(CollectionPlanPreparation::Skipped(outcome));
        }
        CollectionSizeGuard::Plan { location_cap } => location_cap,
    };

    reporting::planning_collection(&scoped.name, &scoped.version.version);

    let settings =
        resolve_collection_settings(ctx.conn, ctx.opts, scoped.cfg.as_ref(), &scoped.hierarchy)?;
    if settings.merge_mode == MergeMode::Merged {
        reporting::merged_mode_not_implemented(&scoped.name);
    }
    let matches = load_collection_matches(CollectionMatchInputs {
        conn: ctx.conn,
        version_id: scoped.version.id,
        collection_name: &scoped.name,
        merge_mode: settings.merge_mode,
        cfg: scoped.cfg.as_ref(),
        location_cap,
    })?;
    if matches.matches.len() < matches.original_count {
        reporting::one_g_one_r(matches.original_count, matches.matches.len());
    }

    Ok(CollectionPlanPreparation::Ready(PreparedCollectionPlan {
        name: scoped.name,
        hierarchy: scoped.hierarchy,
        dest_root,
        matches: matches.matches,
        format: settings.format,
    }))
}

/// The size-guard outcome: either skip (over budget even bounded) or plan with
/// an optional per-content location cap threaded on to the match query.
enum CollectionSizeGuard {
    Skip(CollectionPlanningOutcome),
    Plan { location_cap: Option<i64> },
}

fn collection_size_guard(
    ctx: &CollectionPlanningContext<'_>,
    version_id: i64,
    collection_name: &str,
) -> Result<CollectionSizeGuard> {
    // Guard against pathological collections before materialising any matches: a
    // MAME-style meta-aggregate expands to tens of millions of match-rows and
    // would exhaust memory. When the uncapped expansion blows the budget, retry
    // with a per-content holder cap (dropping only redundant copies of
    // massively-duplicated content); skip-and-report only if it is still over
    // budget even bounded.
    match oversized_decision(ctx.conn, version_id)? {
        OversizedDecision::Plan(location_cap) => Ok(CollectionSizeGuard::Plan { location_cap }),
        OversizedDecision::Skip => {
            reporting::oversized_collection(collection_name);
            Ok(CollectionSizeGuard::Skip(
                CollectionPlanningOutcome::SkippedOversized(format!(
                    "{collection_name} (>{MAX_MATCH_ROWS} match-rows even capped)"
                )),
            ))
        }
    }
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
