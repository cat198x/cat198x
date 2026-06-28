//! Destination path construction and validation for planned outputs.

use anyhow::Result;
use std::path::{Component, Path, PathBuf};

/// Resolve a collection's destination root, in order of precedence:
/// 1. an explicit per-collection `dest_path`, used as-is;
/// 2. otherwise the library-wide `default_dest` joined with the collection's
///    library path (`hierarchy`);
/// 3. otherwise `None`, so the caller skips the collection.
pub(crate) fn resolve_dest_root(
    explicit: Option<&str>,
    default_dest: Option<&str>,
    hierarchy: &str,
) -> Result<Option<String>> {
    match explicit {
        Some(p) => Ok(Some(p.to_string())),
        None => match default_dest {
            Some(base) => join_under_root(base, &[("collection hierarchy", hierarchy)]).map(Some),
            None => Ok(None),
        },
    }
}

/// Build the on-disk destination for one ROM under its collection's root.
///
/// Loose layout: a single-ROM game is placed flat as `dest_root/rom_name`; a
/// multi-ROM game gets its own folder, `dest_root/game_name/rom_name`.
pub(crate) fn build_dest_path(
    dest_root: &str,
    game_name: &str,
    rom_name: &str,
    multi_rom: bool,
) -> Result<String> {
    if multi_rom {
        join_under_root(
            dest_root,
            &[("game name", game_name), ("ROM name", rom_name)],
        )
    } else {
        join_under_root(dest_root, &[("ROM name", rom_name)])
    }
}

pub(crate) fn build_archive_dest_path(
    dest_root: &str,
    game_name: &str,
    ext: &str,
) -> Result<String> {
    validate_relative_path("game name", game_name)?;
    let file_name = format!("{game_name}.{ext}");
    join_under_root(dest_root, &[("archive filename", &file_name)])
}

pub(crate) fn build_disk_dest_path(
    dest_root: &str,
    game_name: &str,
    rom_name: &str,
) -> Result<String> {
    validate_relative_path("disk name", rom_name)?;
    let file_name = format!("{rom_name}.chd");
    join_under_root(
        dest_root,
        &[("game name", game_name), ("disk filename", &file_name)],
    )
}

pub(crate) fn validate_relative_path(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!("{label} is empty");
    }
    if value.contains('\\') {
        anyhow::bail!("{label} contains a backslash: {value}");
    }
    let path = Path::new(value);
    if path.is_absolute() {
        anyhow::bail!("{label} is absolute: {value}");
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if !part.is_empty() => {}
            _ => anyhow::bail!("{label} contains an unsafe path component: {value}"),
        }
    }
    Ok(())
}

fn join_under_root(root: &str, parts: &[(&str, &str)]) -> Result<String> {
    let mut out = PathBuf::from(root.trim_end_matches('/'));
    for (label, value) in parts {
        validate_relative_path(label, value)?;
        out.push(value);
    }
    Ok(out.to_string_lossy().into_owned())
}
