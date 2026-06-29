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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_dest_path_single_rom_is_flat() {
        // A single-ROM game is placed flat, with no redundant game folder.
        assert_eq!(
            build_dest_path("/roms/nes", "Super Mario Bros", "mario.nes", false).unwrap(),
            "/roms/nes/mario.nes"
        );
        // A trailing slash on the root is normalised away.
        assert_eq!(
            build_dest_path("/roms/nes/", "Game", "game.rom", false).unwrap(),
            "/roms/nes/game.rom"
        );
    }

    #[test]
    fn build_dest_path_multi_rom_gets_game_folder() {
        assert_eq!(
            build_dest_path("/roms/nes", "Multi Disk Game", "disk1.img", true).unwrap(),
            "/roms/nes/Multi Disk Game/disk1.img"
        );
        assert_eq!(
            build_dest_path("/roms/nes", "Multi Disk Game", "disk2.img", true).unwrap(),
            "/roms/nes/Multi Disk Game/disk2.img"
        );
    }

    #[test]
    fn destination_building_rejects_unsafe_dat_names() {
        for unsafe_name in [
            "../escape.rom",
            "dir/../../escape.rom",
            "/tmp/escape.rom",
            r"dir\escape.rom",
        ] {
            assert!(
                build_dest_path("/roms/nes", "Game", unsafe_name, false).is_err(),
                "unsafe ROM name should be rejected: {unsafe_name}"
            );
            assert!(
                build_dest_path("/roms/nes", unsafe_name, "disk1.img", true).is_err(),
                "unsafe game name should be rejected: {unsafe_name}"
            );
        }
        assert!(
            resolve_dest_root(None, Some("/roms"), "../Collection").is_err(),
            "unsafe hierarchy should be rejected"
        );
    }

    #[test]
    fn resolve_dest_root_prefers_explicit_path() {
        // An explicit per-collection dest_path wins and is used verbatim,
        // ignoring both the default and the hierarchy.
        assert_eq!(
            resolve_dest_root(Some("/explicit/here"), Some("/lib"), "Acorn/BBC").unwrap(),
            Some("/explicit/here".to_string())
        );
    }

    #[test]
    fn resolve_dest_root_falls_back_to_default_plus_hierarchy() {
        assert_eq!(
            resolve_dest_root(None, Some("/Volumes/Data"), "TOSEC-PIX/Acorn/BBC").unwrap(),
            Some("/Volumes/Data/TOSEC-PIX/Acorn/BBC".to_string())
        );
        // A trailing slash on the default base is normalised away.
        assert_eq!(
            resolve_dest_root(None, Some("/Volumes/Data/"), "TOSEC/Sinclair").unwrap(),
            Some("/Volumes/Data/TOSEC/Sinclair".to_string())
        );
    }

    #[test]
    fn resolve_dest_root_is_none_without_explicit_or_default() {
        // Neither an explicit path nor a default: no destination, caller skips.
        assert_eq!(resolve_dest_root(None, None, "Acorn/BBC").unwrap(), None);
    }
}
