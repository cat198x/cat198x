use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;

use super::destinations::resolve_dest_root;
use super::generator::PlanOptions;
use super::rules::glob_match;
use crate::db::{collections, config as db_config, dats};

/// Whether a version is disk-only: it has at least one `<disk>` and no `<rom>`.
/// Such a collection places loose `<game>/<name>.chd` and never a `<game>.zip`,
/// so it can share a destination root with a ROM collection without colliding.
fn version_is_disk_only(conn: &Connection, version_id: i64) -> Result<bool> {
    let has_disk: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dat_roms r
                         JOIN dat_games g ON g.id = r.game_id
                         JOIN dat_nodes n ON n.id = g.node_id
                        WHERE n.version_id = ?1 AND r.is_disk = 1)",
        [version_id],
        |row| row.get(0),
    )?;
    let has_rom: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dat_roms r
                         JOIN dat_games g ON g.id = r.game_id
                         JOIN dat_nodes n ON n.id = g.node_id
                        WHERE n.version_id = ?1 AND r.is_disk = 0)",
        [version_id],
        |row| row.get(0),
    )?;
    Ok(has_disk && !has_rom)
}

/// A group of in-scope collections that resolve to the same destination root
/// and output namespace -- they would overwrite each other's same-named games,
/// which is the condition that makes a plan refuse.
#[derive(Debug, Clone)]
pub struct DestinationCollision {
    /// The shared destination root.
    pub root: String,
    /// The disk-only (CHD) output namespace. CHDs and ROMs at one root don't
    /// collide (a game's `.zip` and its `.chd` live together), so the namespace
    /// is part of the key.
    pub disk_only: bool,
    /// The collections sharing this (root, namespace).
    pub collections: Vec<CollidingCollection>,
}

/// One collection participating in a [`DestinationCollision`].
#[derive(Debug, Clone)]
pub struct CollidingCollection {
    pub name: String,
    /// The active version, so a caller can re-path its node to break the tie.
    pub version_id: i64,
    /// Whether it has an explicit `dest_path`. Such a collision can't be fixed by
    /// re-pathing the node (the explicit dest wins) -- it needs a config change.
    pub has_explicit_dest: bool,
}

/// Find collections that collide on a destination root + output namespace.
///
/// Scope follows `opts`'s dat/set filters, exactly as planning does.
pub fn find_destination_collisions(
    conn: &Connection,
    opts: &PlanOptions,
    all_collections: &[collections::Collection],
) -> Result<Vec<DestinationCollision>> {
    let mut owners: BTreeMap<(String, bool), Vec<CollidingCollection>> = BTreeMap::new();
    for collection in all_collections {
        if let Some(pattern) = opts.dat_filter.as_deref()
            && !glob_match(pattern, &collection.name)
        {
            continue;
        }
        let version = match collections::get_active_version(conn, collection.id)? {
            Some(v) => v,
            None => continue,
        };
        let hierarchy =
            dats::primary_node_path(conn, version.id)?.unwrap_or_else(|| collection.name.clone());
        if let Some(sets) = opts.set_filter.as_ref() {
            let set = hierarchy.split('/').next().unwrap_or(hierarchy.as_str());
            if !sets.iter().any(|s| s == set) {
                continue;
            }
        }
        let cfg = db_config::get_collection_config(conn, &collection.name)?;
        let explicit = cfg.as_ref().and_then(|c| c.dest_path.as_deref());
        if let Some(root) = resolve_dest_root(explicit, opts.default_dest.as_deref(), &hierarchy)? {
            let disk_only = version_is_disk_only(conn, version.id)?;
            owners
                .entry((root, disk_only))
                .or_default()
                .push(CollidingCollection {
                    name: collection.name.clone(),
                    version_id: version.id,
                    has_explicit_dest: explicit.is_some(),
                });
        }
    }

    let mut collisions: Vec<DestinationCollision> = owners
        .into_iter()
        .filter(|(_, c)| c.len() > 1)
        .map(|((root, disk_only), collections)| DestinationCollision {
            root,
            disk_only,
            collections,
        })
        .collect();
    collisions.sort_by(|a, b| (a.root.as_str(), a.disk_only).cmp(&(b.root.as_str(), b.disk_only)));
    Ok(collisions)
}

/// Refuse to plan when two collections in scope resolve to the same destination
/// root.
///
/// A destination must uniquely identify its source. Two collections sharing a
/// root silently overwrite each other's same-named games: `Arcade/klax` and
/// `Game Gear/klax` both writing `<root>/klax.zip`, last-writer-wins.
///
/// Collections are grouped by `(root, output namespace)`, not root alone. A
/// disk-only (CHD) collection writes loose `<game>/<name>.chd`, which never
/// collides with a ROM collection's `<game>.zip` at the same root.
pub(crate) fn check_unique_destinations(
    conn: &Connection,
    opts: &PlanOptions,
    all_collections: &[collections::Collection],
) -> Result<()> {
    let collisions = find_destination_collisions(conn, opts, all_collections)?;
    if collisions.is_empty() {
        return Ok(());
    }

    let mut msg = String::from(
        "Refusing to plan: collections share a destination root, so their \
         same-named games would overwrite each other. Give each collection a \
         distinct dest_path (e.g. a per-machine subfolder), or run \
         'cat198x doctor --fix' to nest them under their own names.\n",
    );
    for c in &collisions {
        let kind = if c.disk_only { "CHD" } else { "ROM" };
        let names: Vec<&str> = c.collections.iter().map(|x| x.name.as_str()).collect();
        msg.push_str(&format!(
            "  {} ({} outputs) <- {}\n",
            c.root,
            kind,
            names.join(", ")
        ));
    }
    anyhow::bail!(msg);
}
