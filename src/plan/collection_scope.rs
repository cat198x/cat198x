use anyhow::Result;
use rusqlite::Connection;

use super::options::PlanOptions;
use super::scope::hierarchy_matches_set_filter;
use crate::db::{collections, dats};

pub(crate) enum ActiveCollectionResolution {
    NoActiveVersion,
    ExcludedBySet,
    Active(ActiveCollection),
}

pub(crate) struct ActiveCollection {
    pub(crate) name: String,
    pub(crate) version: collections::CollectionVersion,
    pub(crate) hierarchy: String,
}

pub(crate) fn resolve_active_collection(
    conn: &Connection,
    opts: &PlanOptions,
    collection: &collections::Collection,
) -> Result<ActiveCollectionResolution> {
    let version = match collections::get_active_version(conn, collection.id)? {
        Some(version) => version,
        None => return Ok(ActiveCollectionResolution::NoActiveVersion),
    };

    let hierarchy =
        dats::primary_node_path(conn, version.id)?.unwrap_or_else(|| collection.name.clone());

    if !hierarchy_matches_set_filter(&hierarchy, opts) {
        return Ok(ActiveCollectionResolution::ExcludedBySet);
    }

    Ok(ActiveCollectionResolution::Active(ActiveCollection {
        name: collection.name.clone(),
        version,
        hierarchy,
    }))
}
