use anyhow::Result;
use rusqlite::Connection;

use super::options::PlanOptions;
use super::rules::{effective_format, effective_merge_mode};
use crate::config::{MergeMode, OutputFormat};
use crate::db::config as db_config;

pub(crate) struct CollectionSettings {
    pub(crate) merge_mode: MergeMode,
    pub(crate) format: OutputFormat,
}

pub(crate) fn resolve_collection_settings(
    conn: &Connection,
    opts: &PlanOptions,
    cfg: Option<&db_config::CollectionConfig>,
    hierarchy: &str,
) -> Result<CollectionSettings> {
    Ok(CollectionSettings {
        merge_mode: effective_merge_mode(conn, opts, cfg, hierarchy)?,
        format: effective_format(conn, opts, cfg, hierarchy)?,
    })
}
