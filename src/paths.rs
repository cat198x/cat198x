//! Catalogue path form: the one string shape every stored and planned path takes.
//!
//! The catalogue compares paths as strings (source roots, source-relative file
//! paths, planned destinations, archive entry names), so they must agree on a
//! separator. They use `/` on every OS; Windows file APIs accept it. See
//! `decisions/catalogue-path-separator.md`.
//!
//! On Windows a path string is converted: the verbatim prefix that
//! `std::fs::canonicalize` adds is stripped and every `\` becomes `/`. On every
//! other platform the string is returned unchanged, because `\` is a legal
//! filename character there and must never be rewritten.

use std::path::Path;

const VERBATIM_UNC_PREFIX: &str = r"\\?\UNC\";
const VERBATIM_PREFIX: &str = r"\\?\";

/// Convert a Windows path string to catalogue form.
///
/// `\\?\UNC\server\share\x` becomes `//server/share/x`, `\\?\C:\x` becomes
/// `C:/x`, and then every `\` becomes `/`. A verbatim path that names neither a
/// drive nor a UNC share (such as `\\?\Volume{…}\`) keeps its prefix, written
/// as `//?/`, because stripping it would leave a relative path.
///
/// Pure and platform-independent so it can be tested everywhere; production
/// code calls [`catalogue_path`] or [`catalogue_str`], which apply it only on
/// Windows.
pub fn windows_to_catalogue(path: &str) -> String {
    let unprefixed = if let Some(rest) = path.strip_prefix(VERBATIM_UNC_PREFIX) {
        format!(r"\\{rest}")
    } else if let Some(rest) = path
        .strip_prefix(VERBATIM_PREFIX)
        .filter(|rest| starts_with_drive(rest))
    {
        rest.to_string()
    } else {
        path.to_string()
    };
    unprefixed.replace('\\', "/")
}

/// Whether `s` starts with a drive designator such as `C:`.
fn starts_with_drive(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

/// A filesystem path as a catalogue string: converted on Windows, unchanged
/// elsewhere.
pub fn catalogue_path(path: &Path) -> String {
    catalogue_str(&path.to_string_lossy())
}

/// A user-supplied path string (a config root, a collection `dest_path`, a
/// `--source` selector) in catalogue form: converted on Windows, unchanged
/// elsewhere.
pub fn catalogue_str(path: &str) -> String {
    if cfg!(windows) {
        windows_to_catalogue(path)
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_drive_path_loses_its_prefix() {
        assert_eq!(
            windows_to_catalogue(r"\\?\C:\Users\me\ROMs"),
            "C:/Users/me/ROMs"
        );
        assert_eq!(windows_to_catalogue(r"\\?\d:\"), "d:/");
    }

    #[test]
    fn verbatim_unc_path_becomes_a_double_slash_share() {
        assert_eq!(
            windows_to_catalogue(r"\\?\UNC\nas\media\ROMs\NES"),
            "//nas/media/ROMs/NES"
        );
    }

    #[test]
    fn plain_drive_path_uses_forward_slashes() {
        assert_eq!(
            windows_to_catalogue(r"C:\lib\ROMs\mario.nes"),
            "C:/lib/ROMs/mario.nes"
        );
    }

    #[test]
    fn plain_unc_path_uses_forward_slashes() {
        assert_eq!(
            windows_to_catalogue(r"\\nas\media\ROMs"),
            "//nas/media/ROMs"
        );
    }

    #[test]
    fn relative_path_uses_forward_slashes() {
        assert_eq!(
            windows_to_catalogue(r"Sinclair\ZX Spectrum\game.tap"),
            "Sinclair/ZX Spectrum/game.tap"
        );
        assert_eq!(windows_to_catalogue("game.rom"), "game.rom");
    }

    #[test]
    fn forward_slash_path_is_unchanged() {
        for path in ["C:/lib/ROMs", "//nas/media", "/lib/ROMs/a.zip", "a/b.rom"] {
            assert_eq!(windows_to_catalogue(path), path);
        }
    }

    #[test]
    fn mixed_separators_are_unified() {
        assert_eq!(
            windows_to_catalogue(r"\\?\C:\lib\ROMs/SET/Sys\Game.zip"),
            "C:/lib/ROMs/SET/Sys/Game.zip"
        );
    }

    #[test]
    fn verbatim_path_without_a_drive_or_share_keeps_its_prefix() {
        assert_eq!(
            windows_to_catalogue(r"\\?\Volume{1234}\ROMs"),
            "//?/Volume{1234}/ROMs"
        );
    }

    #[test]
    fn catalogue_str_rewrites_only_on_windows() {
        let input = r"lib\odd\name.rom";
        let expected = if cfg!(windows) {
            "lib/odd/name.rom"
        } else {
            // A backslash is a legal filename character outside Windows.
            input
        };
        assert_eq!(catalogue_str(input), expected);
        assert_eq!(catalogue_path(Path::new(input)), expected);
    }
}
