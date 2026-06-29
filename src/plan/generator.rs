//! Plan generation logic

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;

use super::Plan;
use super::archive_planning::{ContainerDrain, emit_container_drains};
use super::collection_planning::{
    CollectionPlanningContext, CollectionPlanningOutcome, plan_collection,
};
use super::collisions::check_unique_destinations;
pub use super::coverage::count_missing_roms;
use super::matching::{compute_shared_containers, compute_shared_content};
use super::reporting;
use super::rules::glob_match;
use super::source_policy::load_source_dispositions;
pub use super::state_hash::compute_state_hash;
use crate::config::{MergeMode, OutputFormat};
use crate::db::collections;

/// Options controlling plan generation.
#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    /// Glob over collection names; `None` plans every collection.
    pub dat_filter: Option<String>,
    /// Restrict planning to these sets — the top segment of a collection's
    /// library path (e.g. `TOSEC`, `TOSEC-PIX`, `FinalBurn Neo`). `None` plans
    /// every set; useful to scope one set's work (e.g. ingest TOSEC without the
    /// arcade sets) without listing every collection.
    pub set_filter: Option<Vec<String>>,
    /// Library-wide destination root for collections without their own dest_path.
    pub default_dest: Option<String>,
    /// Output format for collections without their own setting.
    pub default_format: OutputFormat,
    /// Merge mode for collections without their own setting. Controls MAME-style
    /// parent/clone placement: `Split` (the implemented target) drops a clone's
    /// merge-tagged inherited ROMs from its placement — they live in the parent —
    /// so the clone's archive/folder holds only its own unique ROMs. `NonMerged`
    /// (the default) places every ROM a game's DAT entry lists, parent or clone.
    pub default_merge_mode: MergeMode,
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
    let dat_filter = opts.dat_filter.as_deref();
    let default_dest = opts.default_dest.as_deref();

    // Calculate state hash
    let state_hash = compute_state_hash(conn)?;
    let mut plan = Plan::new(state_hash);

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

    let mut planned_any = false;
    let mut filter_matched_any = false;
    let mut skipped_no_dest: Vec<String> = Vec::new();

    // Source containers a repack rebuilt from and that are safe to lose afterwards
    // — recorded here and emitted as deletes *after* every repack, so the apply
    // runs the rebuilds first and the verify-before-delete net sees each entry
    // surviving at its destination before removing the container. Draining these
    // is what lets `consume` staging empty for recompressed archive sets (a shared
    // .cue/.sub forces a rebuild over a whole-file relocate). Safety rests on the
    // net, not a plan-time guess: a container still needed elsewhere is refused,
    // sticky.
    //
    // Keyed by container path so a container feeding several games is drained
    // once; the accumulated `entries` gather, across those games, where each of
    // the container's entries was repacked to — the rollback spec that rebuilds
    // the container before those destinations are deleted. `reason_dest` is just a
    // representative destination for the human-readable reason.
    let mut drain_after_repack: BTreeMap<String, ContainerDrain> = BTreeMap::new();
    let collection_context = CollectionPlanningContext {
        conn,
        opts,
        default_dest,
        shared: &shared,
        shared_containers: &shared_containers,
        dispositions: &dispositions,
    };

    for collection in &all_collections {
        if let Some(pattern) = dat_filter
            && !glob_match(pattern, &collection.name)
        {
            continue;
        }
        filter_matched_any = true;

        match plan_collection(
            &collection_context,
            collection,
            &mut plan,
            &mut drain_after_repack,
        )? {
            CollectionPlanningOutcome::NoActiveVersion
            | CollectionPlanningOutcome::ExcludedBySet => {}
            CollectionPlanningOutcome::SkippedNoDest(collection_name) => {
                skipped_no_dest.push(collection_name);
            }
            CollectionPlanningOutcome::SkippedOversized(collection_name) => {
                plan.skipped_oversized.push(collection_name);
            }
            CollectionPlanningOutcome::Planned => {
                planned_any = true;
            }
        }
    }

    emit_container_drains(&mut plan, drain_after_repack);

    // Never skip silently: report collections left out because no destination
    // could be resolved, and how to include them. The full list rides on the
    // plan so the caller can write it out for review.
    if !skipped_no_dest.is_empty() {
        reporting::skipped_no_dest(skipped_no_dest.len());
    }

    // Report collections left out because their match expansion is too large to
    // plan safely (a meta-aggregate, not a romset). Already named individually
    // above as they were hit; this is the rollup.
    if !plan.skipped_oversized.is_empty() {
        reporting::skipped_oversized_rollup(plan.skipped_oversized.len());
    }

    if let Some(pattern) = dat_filter
        && !filter_matched_any
    {
        reporting::no_matching_filter(pattern);
    } else if !planned_any && skipped_no_dest.is_empty() && plan.skipped_oversized.is_empty() {
        reporting::no_active_collections();
    }

    plan.skipped_no_dest = skipped_no_dest;
    Ok(plan)
}

#[cfg(test)]
#[path = "generator_tests.rs"]
mod tests;
