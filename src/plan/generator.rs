//! Plan generation logic

use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use super::Plan;
use super::archive_planning::ContainerDrains;
use super::collection_planning::{
    CollectionPlanningContext, CollectionPlanningOutcome, plan_collection,
};
use super::collisions::check_unique_destinations;
pub use super::coverage::count_missing_roms;
use super::matching::{compute_shared_containers, compute_shared_content};
use super::options::PlanOptions;
use super::reporting;
use super::rules::glob_match;
use super::source_policy::load_source_dispositions;
pub use super::state_hash::compute_state_hash;
use crate::db::collections;
use crate::db::files::Disposition;

#[derive(Default)]
struct PlanningRun {
    planned_any: bool,
    filter_matched_any: bool,
    skipped_no_dest: Vec<String>,
}

struct PlanningSetup {
    plan: Plan,
    inputs: PlanningInputs,
}

struct PlanningInputs {
    shared: HashSet<String>,
    shared_containers: HashSet<String>,
    dispositions: HashMap<String, Disposition>,
    all_collections: Vec<collections::Collection>,
}

impl PlanningInputs {
    fn collection_context<'a>(
        &'a self,
        conn: &'a Connection,
        opts: &'a PlanOptions,
    ) -> CollectionPlanningContext<'a> {
        CollectionPlanningContext {
            conn,
            opts,
            default_dest: opts.default_dest.as_deref(),
            shared: &self.shared,
            shared_containers: &self.shared_containers,
            dispositions: &self.dispositions,
        }
    }
}

/// Generate a plan for all configured collections with default options.
pub fn generate_plan(conn: &Connection) -> Result<Plan> {
    generate_plan_filtered(conn, &PlanOptions::default())
}

/// Generate a plan from the given options.
///
/// `dat_filter` supports glob patterns (`*`, `?`, case-insensitive) over
/// collection names.
pub fn generate_plan_filtered(conn: &Connection, opts: &PlanOptions) -> Result<Plan> {
    let PlanningSetup { mut plan, inputs } = prepare_plan_generation(conn, opts)?;

    let mut container_drains = ContainerDrains::default();
    let collection_context = inputs.collection_context(conn, opts);

    let PlanningRun {
        planned_any,
        filter_matched_any,
        skipped_no_dest,
    } = plan_collections(
        &collection_context,
        &inputs.all_collections,
        &mut plan,
        &mut container_drains,
    )?;

    container_drains.emit_into(&mut plan);

    reporting::plan_completion(
        opts.dat_filter.as_deref(),
        planned_any,
        filter_matched_any,
        skipped_no_dest.len(),
        plan.skipped_oversized.len(),
    );
    plan.skipped_no_dest = skipped_no_dest;
    Ok(plan)
}

fn prepare_plan_generation(conn: &Connection, opts: &PlanOptions) -> Result<PlanningSetup> {
    let state_hash = compute_state_hash(conn)?;
    let plan = Plan::new(state_hash);

    // Content shared across distinct entries is copied to each destination, never
    // moved or deleted (see compute_shared_content). Computed once up front.
    let shared = compute_shared_content(conn)?;
    if !shared.is_empty() {
        reporting::shared_content(shared.len());
    }

    // Containers (archive files) whose entries serve more than one game must not
    // be relocated whole or deleted — each game repacks its own entries instead.
    let shared_containers = compute_shared_containers(conn)?;
    if !shared_containers.is_empty() {
        reporting::shared_containers(shared_containers.len());
    }

    // Each source's disposition decides, per operation, whether content is moved
    // (and its source freed) or copied. Built once; consulted at every placement.
    let dispositions = load_source_dispositions(conn)?;

    // Plan every collection, not only those with an explicit dest_path: a
    // library-wide `default_dest_path` should reach collections that were never
    // individually configured. Each collection's destination is resolved below.
    let all_collections = collections::list_collections(conn)?;

    // A destination must uniquely identify its source. Refuse before doing any
    // work if two collections in scope resolve to the same root — they would
    // silently overwrite each other's same-named games.
    check_unique_destinations(conn, opts, &all_collections)?;

    Ok(PlanningSetup {
        plan,
        inputs: PlanningInputs {
            shared,
            shared_containers,
            dispositions,
            all_collections,
        },
    })
}

fn plan_collections(
    ctx: &CollectionPlanningContext<'_>,
    all_collections: &[collections::Collection],
    plan: &mut Plan,
    container_drains: &mut ContainerDrains,
) -> Result<PlanningRun> {
    let mut run = PlanningRun::default();

    for collection in all_collections {
        if let Some(pattern) = ctx.opts.dat_filter.as_deref()
            && !glob_match(pattern, &collection.name)
        {
            continue;
        }
        run.filter_matched_any = true;

        match plan_collection(ctx, collection, plan, container_drains)? {
            CollectionPlanningOutcome::NoActiveVersion
            | CollectionPlanningOutcome::ExcludedBySet => {}
            CollectionPlanningOutcome::SkippedNoDest(collection_name) => {
                run.skipped_no_dest.push(collection_name);
            }
            CollectionPlanningOutcome::SkippedOversized(collection_name) => {
                plan.skipped_oversized.push(collection_name);
            }
            CollectionPlanningOutcome::Planned => {
                run.planned_any = true;
            }
        }
    }

    Ok(run)
}

#[cfg(test)]
#[path = "generator_tests.rs"]
mod tests;
