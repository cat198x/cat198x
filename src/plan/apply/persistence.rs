use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::plan::{OperationLog, Plan};

/// Persist the updated plan and completed rollback log for a real apply run.
pub(super) fn persist_apply_run(
    plan: &Plan,
    plan_path: &Path,
    dry_run: bool,
    op_log: Option<OperationLog>,
) -> Result<Option<PathBuf>> {
    if dry_run {
        return Ok(None);
    }

    let plan_json = serde_json::to_string_pretty(plan).context("Failed to serialize plan")?;
    fs::write(plan_path, &plan_json).context("Failed to update plan file")?;

    if let Some(mut log) = op_log {
        log.complete();
        let logs_dir = plan_path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("logs"))
            .unwrap_or_else(|| PathBuf::from("objects/logs"));
        return Ok(Some(log.save(&logs_dir)?));
    }

    Ok(None)
}
