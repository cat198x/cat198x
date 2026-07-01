use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::plan::{Plan, compute_state_hash};

/// The most recent saved plan — the plan-as-diff the UI renders — or `None` when
/// no plan has been generated. Reads the newest plan JSON under
/// `<data_dir>/objects/plans`; the plan already *is* the diff, so no reconcile
/// model is needed (see the decision record).
pub fn latest_plan(data_dir: &Path) -> Result<Option<Plan>> {
    match newest_plan_file(data_dir)? {
        Some(path) => {
            let contents = std::fs::read_to_string(&path)?;
            Ok(Some(serde_json::from_str(&contents)?))
        }
        None => Ok(None),
    }
}

/// The path of the most recently written plan under `<data_dir>/objects/plans`,
/// or `None` when none exists. Shared by [`latest_plan`] and [`apply`] — the
/// latter needs the path to drive the apply engine.
pub(super) fn newest_plan_file(data_dir: &Path) -> Result<Option<std::path::PathBuf>> {
    let plans_dir = data_dir.join("objects/plans");
    if !plans_dir.is_dir() {
        return Ok(None);
    }
    let mut latest: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(&plans_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json")
            && let Ok(modified) = entry.metadata().and_then(|m| m.modified())
        {
            let newer = match &latest {
                Some((_, prev)) => modified > *prev,
                None => true,
            };
            if newer {
                latest = Some((path, modified));
            }
        }
    }
    Ok(latest.map(|(path, _)| path))
}

/// One collection's pending reorganise work, from the saved plan.
#[derive(Debug, Clone, Serialize)]
pub struct PendingItem {
    pub collection: String,
    /// The collection's library path, for rolling up the tree.
    pub node_path: String,
    /// Operations the plan would perform here (copy/move/repack/relocate).
    pub to_write: usize,
    /// Bytes the plan would transfer here.
    pub bytes: u64,
}

/// The reorganise work the saved plan implies, per collection, plus whether the
/// plan has gone stale against the current catalogue.
#[derive(Debug, Clone, Serialize)]
pub struct PendingWork {
    /// `true` when the catalogue has changed since the plan was generated, so the
    /// numbers may be out of date and the plan should be re-run.
    pub stale: bool,
    /// When the underlying plan was generated.
    pub plan_created_at: String,
    /// Collections with at least one pending operation.
    pub items: Vec<PendingItem>,
}

/// The pending reorganise work from the most recent saved plan, or `None` when
/// no plan has been generated.
///
/// This is a *read* of the saved plan's per-collection breakdown — it does not
/// run the planner. The `stale` flag (the saved plan's state hash vs the current
/// catalogue's) tells a caller when those numbers predate the catalogue and the
/// plan should be regenerated. Clean-up work (removals, husks) is not included
/// here; only the additive/reorganise operations the plan carries.
pub fn pending_work(conn: &Connection, data_dir: &Path) -> Result<Option<PendingWork>> {
    let Some(plan) = latest_plan(data_dir)? else {
        return Ok(None);
    };
    let stale = compute_state_hash(conn)? != plan.state_hash;
    let items = plan
        .per_collection
        .iter()
        .filter(|c| c.to_write > 0)
        .map(|c| PendingItem {
            collection: c.name.clone(),
            node_path: c.node_path.clone(),
            to_write: c.to_write,
            bytes: c.bytes,
        })
        .collect();
    Ok(Some(PendingWork {
        stale,
        plan_created_at: plan.created_at,
        items,
    }))
}
