# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0](https://github.com/cat198x/cat198x/compare/v0.3.1...v0.4.0) - 2026-06-15

### Added

- *(plan)* honour split merge-mode so clone archives drop inherited ROMs
- *(catalogue)* add catalogue-placements to converge after a reorg
- *(apply)* add --prune-empty to self-clean emptied source dirs
- *(prune)* add prune-empty to clear directories left by a --move tidy
- *(plan)* skip collections whose match expansion would exhaust memory
- *(plan)* store CHDs loose in a machine folder, never packed
- *(scanner)* identify CHDs by their internal header SHA1
- *(dat)* parse <disk> (CHD) entries into dat_roms
- *(apply)* run repacks concurrently (-j/--jobs, default 8)
- *(plan)* delete exact-content duplicates instead of quarantining them
- *(apply)* move-mode repack deletes its loose sources, reversibly
- *(apply)* defer repacks with --skip-repack, and resume partial plans
- *(plan)* relocate complete staged archives instead of repacking them
- *(quarantine)* add a Duplicate reason so deduped copies group correctly
- *(plan)* dedupe ROM copies by destination, quarantining duplicates
- *(plan)* add 7z as an output format
- *(dat)* add `dat sort` to nest a flat DAT pack by collection name
- *(cli)* add `unknowns` to report files matched by no active DAT
- *(plan)* `plan --move` for a true in-place tidy
- *(plan)* repack when an archive is the wrong container format
- *(plan)* per-set breakdown of pending operations
- *(stats)* generalise grouping to `stats --group-by system|set`
- *(plan)* write the skipped-collection list to a file
- *(doctor)* point at `dat relink` when DAT files are missing
- *(config)* add `config get-default` and show defaults in `config list`
- *(plan)* emit archives for zip/torrentzip output formats
- *(plan)* let SourceRef carry a canonical archive entry name
- *(dat)* add `dat relink` to re-point moved DAT files
- *(stats)* roll collections up by group with `stats --group`
- *(config)* add `config set-default` for library-wide defaults
- *(plan)* resolve destinations from a library-wide default + hierarchy
- *(plan)* lay multi-ROM games out in their own folder
- *(dat)* record each collection's library path on recursive add

### Fixed

- *(db)* replace a loose file's hash on re-scan instead of accumulating a second row
- *(plan)* detect shared CRC-only arcade content so containers aren't relocated whole
- *(plan)* never delete a placed library file as a duplicate
- *(apply)* verify CHD copies by internal header SHA1, not file bytes
- *(apply)* make the disk-space guard move-aware and stat-free
- *(scan)* honour numeric --source selectors as source ids
- *(repack)* collapse duplicate entry names instead of aborting the build
- *(plan)* never relocate or delete a container that sources multiple games
- *(plan)* copy content shared across entries, never consume its source
- *(util)* truncate_path must not panic on multi-byte UTF-8 paths
- *(quarantine)* make the store location configurable, default on-volume
- *(plan)* show quarantine operations in the plan summary
- *(plan)* repack loose files into archives instead of renaming them
- *(apply)* keep the catalogue in step with file operations
- *(dat)* preserve XML entities in DAT names and survive duplicate games
- *(dat)* make re-adding an existing DAT version a no-op

### Other

- *(plan)* resolve archive completeness by lookup, not nested scan
- *(plan)* index files(crc32, size) for CRC-only DAT matching
- *(scanner)* read only the CHD header, not the whole file
- Find ZIP entries by decoded name, not the crate's by_name map
- *(apply)* same-filesystem loose move renames without re-hashing
- *(plan)* make planning viable on large libraries and add per-set format
- *(apply)* rename same-volume loose-file moves instead of copy+delete
- *(plan)* trust the catalogue instead of re-hashing destination files
- *(plan)* end-to-end hierarchical reorganise plan

## [0.3.1](https://github.com/cat198x/cat198x/compare/v0.3.0...v0.3.1) - 2026-06-03

### Other

- add shared House198x Vale prose style

## [0.3.0](https://github.com/cat198x/cat198x/compare/v0.2.1...v0.3.0) - 2026-06-03

### Added

- *(fetch)* generate a ZXDB DAT and add a guided TOSEC source
- *(dat)* match ROMs by MD5 as a fallback hash key

### Fixed

- *(stats)* match ROMs by SHA1 or CRC so CRC-only sets aren't reported as 0%
- *(dat)* dedupe duplicate ROM names within a game instead of aborting import

### Other

- *(scanner)* decode 7z via the system binary, falling back to Rust

## [0.2.1](https://github.com/cat198x/cat198x/compare/v0.2.0...v0.2.1) - 2026-06-02

### Other

- *(release-plz)* enable git_only to bump versions from git tags

## [0.2.0](https://github.com/cat198x/cat198x/releases/tag/v0.2.0) - 2026-06-01

### Added

- **`dat add --recursive`** — point it at a directory and every `.dat`/`.xml`
  underneath is imported. Each DAT imports in its own transaction, so one
  malformed file is reported and skipped without losing the batch; the database
  is opened once for the whole run. `--collection` is ignored in recursive mode
  (each DAT names its own collection).
- **Textual scan progress off a terminal.** When stderr is not a TTY (piped,
  redirected, backgrounded, CI), `scan` now logs `hashing N files`, periodic
  `hashed X/Y (N%)` lines, and the database-write phase, where before it printed
  nothing until the end. Interactive runs keep the live progress bar.

## [0.1.0](https://github.com/cat198x/cat198x/releases/tag/v0.1.0) - 2026-06-01

Initial release of Cat198x — the 198x family's binary-asset cataloguing tool,
rescued and rebranded from Romshelf. It catalogues a ROM/disk collection by
content hash, verifies it against DAT databases, and reorganises it through a
plan you review before anything moves.

### Added

- **Catalogue by content.** Scan directories and identify every file by hash
  (SHA-1, MD5, CRC32), not by name. Detects and strips console headers (iNES,
  SMC, A78, LNX) so headered ROMs match both headered and headerless DATs.
- **Verify against DATs.** Match collections against Logiqx / clrmamepro
  databases (No-Intro, Redump, MAME, FinalBurn Neo), honouring MAME merge modes
  and matching ROMs by SHA-1 or by CRC+size.
- **Reorganise safely.** A `plan` → `apply` → `rollback` cycle: `plan` writes an
  explicit operation list, `apply` is the only command that touches files (with
  `--dry-run`), moves verify-and-fsync the destination before removing the
  source, and every operation is journalled so `apply --rollback` can walk it
  back. Files that don't belong are quarantined under their content hash, never
  silently deleted.
- **Archives and formats.** Read ZIP and 7z; write ZIP and reproducible
  TorrentZIP; create and verify `.torrent` files; export status as txt/csv/json.
- **CLI niceties.** `doctor` health checks, shell completions, and self-update
  from GitHub releases.

### Notes

This first release hardens the Romshelf baseline against the data-integrity
issues found in audit: DAT import and scanning now write transactionally, moves
are verified and flushed before the source is removed, quarantine uses the full
content hash (no truncation collisions), and DAT matching no longer drops
CRC-only entries. The plan-execution engine lives in the library so other 198x
tools can drive the same audited file operations.
