use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::cli::open_database;
use crate::db::{collections, dats};

pub(in crate::cli::dat) fn diff_versions(
    collection: &str,
    from: Option<&str>,
    to: Option<&str>,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Find the collection
    let coll = collections::get_collection_by_name(conn, collection)?
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", collection))?;

    // Get all versions for this collection
    let versions = collections::list_versions(conn, coll.id)?;
    if versions.len() < 2 && (from.is_none() || to.is_none()) {
        anyhow::bail!(
            "Collection '{}' only has {} version(s). Need at least 2 versions to diff.",
            collection,
            versions.len()
        );
    }

    // Determine the "from" version
    let from_version = if let Some(from_str) = from {
        versions
            .iter()
            .find(|v| v.version == from_str)
            .ok_or_else(|| anyhow::anyhow!("Version '{}' not found", from_str))?
    } else {
        // Find the second most recent (previous to active)
        let active_idx = versions.iter().position(|v| v.is_active);
        if let Some(idx) = active_idx {
            if idx + 1 < versions.len() {
                &versions[idx + 1]
            } else {
                anyhow::bail!("No previous version available to compare against");
            }
        } else if versions.len() >= 2 {
            &versions[1] // Second most recent
        } else {
            anyhow::bail!("No previous version available to compare against");
        }
    };

    // Determine the "to" version
    let to_version = if let Some(to_str) = to {
        versions
            .iter()
            .find(|v| v.version == to_str)
            .ok_or_else(|| anyhow::anyhow!("Version '{}' not found", to_str))?
    } else {
        // Use active version, or most recent
        versions
            .iter()
            .find(|v| v.is_active)
            .or(versions.first())
            .ok_or_else(|| anyhow::anyhow!("No version found"))?
    };

    println!("Comparing versions of '{}':", collection);
    println!(
        "  From: {} ({})",
        from_version.version, from_version.imported_at
    );
    println!(
        "  To:   {} ({})",
        to_version.version, to_version.imported_at
    );
    println!();

    // Get games from both versions
    let from_games = dats::get_games_for_version(conn, from_version.id)?;
    let to_games = dats::get_games_for_version(conn, to_version.id)?;

    let from_game_names: HashSet<_> = from_games.iter().map(|g| &g.name).collect();
    let to_game_names: HashSet<_> = to_games.iter().map(|g| &g.name).collect();

    // Calculate game changes
    let added_games: Vec<_> = to_game_names.difference(&from_game_names).collect();
    let removed_games: Vec<_> = from_game_names.difference(&to_game_names).collect();

    // Get ROMs and their SHA1s from both versions
    let from_roms = dats::get_roms_for_version(conn, from_version.id)?;
    let to_roms = dats::get_roms_for_version(conn, to_version.id)?;

    // Build sets of SHA1 hashes for comparison
    let from_sha1s: HashSet<_> = from_roms
        .iter()
        .filter_map(|(_, r)| r.sha1.as_ref())
        .collect();
    let to_sha1s: HashSet<_> = to_roms
        .iter()
        .filter_map(|(_, r)| r.sha1.as_ref())
        .collect();

    let new_sha1s: Vec<_> = to_sha1s.difference(&from_sha1s).collect();
    let removed_sha1s: Vec<_> = from_sha1s.difference(&to_sha1s).collect();

    // Print summary
    println!("Games:");
    println!(
        "  {} → {} ({}{})",
        from_games.len(),
        to_games.len(),
        if to_games.len() >= from_games.len() {
            "+"
        } else {
            ""
        },
        to_games.len() as i64 - from_games.len() as i64
    );

    println!("ROMs (unique SHA1s):");
    println!(
        "  {} → {} ({}{})",
        from_sha1s.len(),
        to_sha1s.len(),
        if to_sha1s.len() >= from_sha1s.len() {
            "+"
        } else {
            ""
        },
        to_sha1s.len() as i64 - from_sha1s.len() as i64
    );

    println!();

    // Print added games (up to 20)
    if !added_games.is_empty() {
        println!("Added games ({}):", added_games.len());
        for (i, name) in added_games.iter().take(20).enumerate() {
            println!("  + {}", name);
            if i == 19 && added_games.len() > 20 {
                println!("  ... and {} more", added_games.len() - 20);
            }
        }
        println!();
    }

    // Print removed games (up to 20)
    if !removed_games.is_empty() {
        println!("Removed games ({}):", removed_games.len());
        for (i, name) in removed_games.iter().take(20).enumerate() {
            println!("  - {}", name);
            if i == 19 && removed_games.len() > 20 {
                println!("  ... and {} more", removed_games.len() - 20);
            }
        }
        println!();
    }

    // Print ROM hash changes summary
    if !new_sha1s.is_empty() || !removed_sha1s.is_empty() {
        println!("ROM changes:");
        println!("  {} new ROM hashes", new_sha1s.len());
        println!("  {} removed ROM hashes", removed_sha1s.len());
    }

    if added_games.is_empty()
        && removed_games.is_empty()
        && new_sha1s.is_empty()
        && removed_sha1s.is_empty()
    {
        println!("No differences found between versions.");
    }

    Ok(())
}
