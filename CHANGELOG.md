# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
