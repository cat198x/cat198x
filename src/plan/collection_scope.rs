use anyhow::Result;
use rusqlite::Connection;

use super::destinations::resolve_dest_root;
use super::options::PlanOptions;
use super::scope::hierarchy_matches_set_filter;
use crate::db::{collections, config as db_config, dats};

pub(crate) enum ActiveCollectionResolution {
    NoActiveVersion,
    ExcludedBySet,
    Active(ActiveCollection),
}

pub(crate) enum ScopedCollectionResolution {
    NoActiveVersion,
    ExcludedBySet,
    Resolved(Box<ScopedCollection>),
}

pub(crate) struct ActiveCollection {
    pub(crate) name: String,
    pub(crate) version: collections::CollectionVersion,
    pub(crate) hierarchy: String,
}

pub(crate) struct ScopedCollection {
    pub(crate) name: String,
    pub(crate) version: collections::CollectionVersion,
    pub(crate) hierarchy: String,
    pub(crate) cfg: Option<db_config::CollectionConfig>,
    pub(crate) dest_root: Option<String>,
    pub(crate) has_explicit_dest: bool,
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

pub(crate) fn resolve_scoped_collection(
    conn: &Connection,
    opts: &PlanOptions,
    default_dest: Option<&str>,
    collection: &collections::Collection,
) -> Result<ScopedCollectionResolution> {
    let active = match resolve_active_collection(conn, opts, collection)? {
        ActiveCollectionResolution::Active(active) => active,
        ActiveCollectionResolution::NoActiveVersion => {
            return Ok(ScopedCollectionResolution::NoActiveVersion);
        }
        ActiveCollectionResolution::ExcludedBySet => {
            return Ok(ScopedCollectionResolution::ExcludedBySet);
        }
    };

    let cfg = db_config::get_collection_config(conn, &active.name)?;
    let explicit = cfg.as_ref().and_then(|c| c.dest_path.as_deref());
    let has_explicit_dest = explicit.is_some();
    let dest_root = resolve_dest_root(explicit, default_dest, &active.hierarchy)?;

    Ok(ScopedCollectionResolution::Resolved(Box::new(
        ScopedCollection {
            name: active.name,
            version: active.version,
            hierarchy: active.hierarchy,
            cfg,
            dest_root,
            has_explicit_dest,
        },
    )))
}
