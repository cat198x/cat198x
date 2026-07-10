use anyhow::Result;
use std::path::PathBuf;

use crate::cli::get_data_dir;
use crate::config::Config;
use crate::db::collections::Collection;
use crate::db::dats;
use crate::plan::{PlanOptions, find_destination_collisions};

use super::report::Check;

/// Check collections colliding on a destination root.
///
/// Sibling DATs imported from one directory share a node path, so they resolve
/// to the same destination and a plan refuses because it would overwrite
/// same-named games. With `--fix`, each non-explicit collider is nested under
/// its own name.
pub(super) fn check(
    conn: &rusqlite::Connection,
    collections: &[Collection],
    fix: bool,
    data_dir: Option<PathBuf>,
) -> Result<Check> {
    let config_path = get_data_dir(data_dir).ok().map(|d| d.join("config.toml"));
    let file_config = match &config_path {
        Some(p) if p.exists() => Config::load(p).unwrap_or_default(),
        _ => Config::default(),
    };
    let opts = PlanOptions {
        dat_filter: None,
        set_filter: None,
        default_dest: file_config.default_dest_path,
        default_format: file_config.default_output_format,
        default_merge_mode: file_config.default_merge_mode,
    };
    let collisions = find_destination_collisions(conn, &opts, collections)?;

    if collisions.is_empty() {
        return Ok(Check::ok("No destination-root collisions"));
    }

    let nestable = collisions
        .iter()
        .flat_map(|c| &c.collections)
        .filter(|m| !m.has_explicit_dest)
        .count();
    // A group where every member has an explicit dest can't be nested away (the
    // explicit dest wins) and needs a manual config change.
    let manual_groups = collisions
        .iter()
        .filter(|c| c.collections.iter().all(|m| m.has_explicit_dest))
        .count();

    let mut detail = format!(
        "{} destination root(s) shared by multiple collections — a plan will refuse.\n",
        collisions.len()
    );
    for c in collisions.iter().take(5) {
        let names: Vec<&str> = c.collections.iter().map(|m| m.name.as_str()).collect();
        let shown = if names.len() > 6 {
            format!("{}, ... (+{} more)", names[..6].join(", "), names.len() - 6)
        } else {
            names.join(", ")
        };
        detail.push_str(&format!("         {} <- {}\n", c.root, shown));
    }
    if collisions.len() > 5 {
        detail.push_str(&format!("         ... and {} more\n", collisions.len() - 5));
    }
    detail.push_str(&format!(
        "         {nestable} collection(s) auto-nestable; run with --fix"
    ));
    if manual_groups > 0 {
        detail.push_str(&format!(
            ". {manual_groups} group(s) need a manual dest_path change \
                     (all members have explicit dests)"
        ));
    }

    if fix {
        let mut nested = 0usize;
        for c in &collisions {
            for member in &c.collections {
                if !member.has_explicit_dest
                    && let Some(new_path) =
                        dats::nest_primary_node_under_name(conn, member.version_id)?
                {
                    println!("  Fixed: nested '{}' -> {}", member.name, new_path);
                    nested += 1;
                }
            }
        }
        println!(
            "  Fixed: nested {nested} collection(s) under their own name \
                     (re-run 'cat198x doctor' to confirm)"
        );
    }

    Ok(Check::warning("No destination-root collisions", &detail))
}
