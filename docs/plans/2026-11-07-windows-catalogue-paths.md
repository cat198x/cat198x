# Windows catalogue paths

Issue #102: on Windows the plan, scan and reclaim tests fail because stored and
planned paths mix separators. Source roots come from `canonicalize` as verbatim
`\\?\C:\…` strings, the scanner stores `Sinclair\game.rom`, and the planner joins
with `PathBuf::push`, while archive entries, DAT hierarchies and every string
join in the planner use `/`. String comparisons between them miss, so the
planner proposes operations it should not.

Catalogue and plan paths use `/` on every OS. On Unix nothing changes: `\` is a
legal filename character there and is never rewritten.

1. Add `src/paths.rs`: a pure `windows_to_catalogue(&str)` that strips the
   verbatim prefix (`\\?\UNC\server\share` → `//server/share`, `\\?\C:\` →
   `C:/`) and turns every `\` into `/`; `catalogue_path(&Path)` and
   `catalogue_str(&str)` apply it on Windows only. No new dependency. Unit-test
   the pure function on every platform (verbatim drive, verbatim UNC, plain
   drive, plain UNC, relative, already-`/`).
2. Join planned destinations as strings with `/` in
   `src/plan/destinations.rs`, keeping `validate_relative_path`.
3. Use the helper wherever a filesystem path becomes a stored or compared
   string: source roots (`src/cli/source.rs`), DAT paths (`src/cli/dat/add.rs`,
   `upgrade.rs`, `maintenance.rs`), scanned relative paths
   (`src/cli/scan/processing.rs`, `planning.rs`), the journaled quarantine path
   (`src/plan/executor/safety_ops.rs`), `--source` selectors
   (`src/cli/source_selector.rs`, `src/cli/prune.rs`), the config
   `default_dest_path` (`src/config/types.rs`, `src/cli/config/defaults.rs`) and
   collection `dest_path` (`src/db/config.rs`). Tests that build a path from a
   tempdir and compare it with production output use the helper too.
4. Schema v6 in `src/db/schema.rs`: on Windows only, rewrite every stored path
   column to catalogue form in one transaction; elsewhere only record the
   version. A UNIQUE collision (including loose `file_locations` rows, whose
   NULL `archive_path` the constraint does not cover) aborts with a clear error.
   The rewrite takes the conversion as a parameter so its tests run on every
   platform against an in-memory database seeded with Windows-style rows.
5. Turn on `platform-test` in `.github/workflows/ci.yml` and drop the #102
   comment. Record the convention in `decisions/catalogue-path-separator.md`.
6. Verify with `cargo fmt --all --check`, `cargo clippy --workspace
   --all-targets -- -D warnings` and `cargo test --workspace` (judged by exit
   status), plus `cargo check`/`clippy` for `x86_64-pc-windows-msvc` if the
   target builds locally. The real Windows run is CI with `platform-test`.
