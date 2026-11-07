# Catalogue paths use `/` on every OS

**Status:** Accepted (2026-11-07).
**Scope:** Cat198x flagship — every path the catalogue stores, and every path the planner builds and compares.
**Related:** issue #102 (Windows plan, scan and reclaim failures); `src/paths.rs` (the conversion); `src/db/schema.rs` (schema v6).

## Bottom line

Catalogue and plan paths use `/` as the separator on every OS. On Windows, a path becomes a stored or compared string only through `paths::catalogue_path` or `paths::catalogue_str`, which strip the verbatim prefix and turn `\` into `/`. On every other platform those functions return the string unchanged: `\` is a legal filename character on Unix and must never be rewritten.

## Context

The catalogue compares paths as strings. Source roots are matched by prefix (`resolve_in_sources`), planned destinations are compared with catalogued locations to decide whether a file is already in place, and file paths are joined to roots with `format!("{root}/{rel}")`. Archive entry names and DAT hierarchies were already `/`-separated.

On Windows three sources of `\` broke those comparisons:

- Source roots and DAT paths went through `std::fs::canonicalize`, which on Windows returns a verbatim path such as `\\?\C:\ROMs` or `\\?\UNC\nas\share`. That string was stored as it came.
- The scanner stored source-relative paths with `\` (`Sinclair\game.rom`).
- The planner joined destinations with `PathBuf::push`, giving `/roms/nes\mario.nes`.

The result was 20 failing tests on Windows, and a planner that proposed operations it should not, such as moving a canonical archive that was already at its destination. Windows is a shipped release target, so this was a real bug, not a test artefact.

## Decision

- **One form.** Stored and planned paths are `/`-separated on every OS. Windows file APIs accept `/`, including `C:/x` and `//server/share/x`.
- **Convert only on Windows.** `paths::windows_to_catalogue` is a pure string function: `\\?\UNC\server\share\x` becomes `//server/share/x`, `\\?\C:\x` becomes `C:/x`, and then every `\` becomes `/`. A verbatim path that names neither a drive nor a share keeps its prefix (as `//?/`), because stripping it would leave a relative path. `catalogue_path` and `catalogue_str` apply it only under `cfg!(windows)`. The function is tested on every platform.
- **Convert at the edge.** Every place a filesystem path or a user-supplied path becomes a stored or compared string uses the helper: source roots, DAT paths, scanned relative paths, the journaled quarantine path, `--source` selectors, the config `default_dest_path` (on load and on set) and a collection's `dest_path` (on write). The planner joins destinations as strings with `/`.
- **No new dependency.** The verbatim-prefix stripping is a few lines; a crate such as `dunce` is not needed for it.

## Migration

Schema v6 rewrites the stored path columns of an existing catalogue to catalogue form, on Windows only, in one transaction: `sources.path`, `file_locations.path`, `collection_versions.dat_path`, `dat_config.dest_path`, `quarantine.original_path`, `quarantine.quarantine_path`, `operation_log.source_path` and `operation_log.dest_path`. On other platforms v6 only records the version, because a `\` there is part of a filename. `file_locations.archive_path` is left alone: it holds names from inside archives, which are the same on every OS and were never written with Windows separators.

If two rows would end up with the same unique key, the upgrade stops with an error naming the path and changes nothing. Loose `file_locations` rows are checked explicitly, because their NULL `archive_path` puts them outside the UNIQUE constraint.

Not migrated: `operation_log.reverse_operation` (JSON, and unused), and on-disk rollback journals and saved plans. Those hold paths that are only ever handed back to file APIs, which accept either separator.

## The trap to remember

`std::fs::canonicalize` on Windows returns a verbatim `\\?\` path. Storing its output as a string silently creates a second spelling of every path. Any new code that canonicalizes, walks a directory, or takes a path argument and then stores or compares the result as a string must go through `paths::catalogue_path` or `paths::catalogue_str`. Display-only output (`println!` of a `Path`) does not need it.
