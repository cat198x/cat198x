# Cat198x

[![CI](https://github.com/cat198x/cat198x/actions/workflows/ci.yml/badge.svg)](https://github.com/cat198x/cat198x/actions/workflows/ci.yml)

Catalogue, verify, and reorganise a retro-gaming ROM collection — safely.

Cat198x scans your ROM directories, matches every file against
[DAT](#dat-files) reference databases (No-Intro, Redump, MAME, FinalBurn Neo),
and tells you exactly what you have, what you're missing, and what's misnamed or
duplicated. When you want to tidy the collection, it generates a **plan** you can
inspect before anything moves — and every applied plan can be rolled back.

Think of it as Terraform for ROMs: nothing changes on disk until you say so, and
nothing is ever silently lost.

```console
$ cat198x status "Nintendo - Game Boy"
Collection Status:

Nintendo - Game Boy  v20240315  [83.7% complete]
  1412 games, 1412 ROMs required
  1182 have, 230 missing
```

## Why it exists

Retro collections drift. Files get renamed, re-compressed, half-downloaded, and
duplicated across drives. The reference DATs that define a "correct" set are
themselves versioned and updated. Keeping a large collection verified by hand is
tedious and error-prone, and most existing tools either mutate files in place
without a safety net or assume a single rigid layout.

Cat198x separates **knowing** from **changing**:

- **Scan** builds an inventory by content hash (SHA-1, plus CRC32/MD5), so a file
  is identified by what it *is*, not what it's called.
- **Status** compares that inventory against active DATs, accounting for headered
  ROMs (iNES, SMC, A78, LNX) and MAME merge modes.
- **Plan / apply** proposes copies, moves, repacks, and deletions, writes them to
  a reviewable plan, and only touches disk on `apply` — transactionally, with a
  rollback log.

## Install

Cat198x is a single self-contained binary with no runtime dependencies.

```bash
# From source (requires a recent stable Rust toolchain)
cargo install --path .

# Verify the install
cat198x doctor
```

## Quick start

The full loop is: tell Cat198x where the reference DATs and your ROMs live,
scan, review status, then plan and apply any reorganisation.

```bash
# 1. Initialise a catalogue in the current directory (~/.cat198x by default)
cat198x init

# 2. Add a reference DAT (or fetch a known one)
cat198x dat add ~/dats/Nintendo\ -\ Game\ Boy.dat
cat198x dat fetch mame            # download a known source; --list to see options

# 3. Point Cat198x at your ROM directories
cat198x source add /mnt/roms/gameboy

# 4. Scan — hashes every file, resumable, safe to re-run
cat198x scan

# 5. See where you stand
cat198x status "Nintendo - Game Boy" --detailed

# 6. Plan a reorganisation, review it, then apply
cat198x plan
cat198x apply --dry-run           # show exactly what would change
cat198x apply                     # do it (rollback-able)
```

Made a mistake? Undo the most recent apply:

```bash
cat198x apply --rollback
```

## Safety model

Cat198x is built so that a wrong command costs you time, never data.

- **Plan before apply.** `plan` writes an explicit operation list; `apply` is the
  only command that mutates ROM files, and `apply --dry-run` shows the operations
  without performing them.
- **Stale-plan detection.** If the catalogue changes after a plan is generated,
  `apply` refuses to run against the outdated plan.
- **Verified moves.** Files are hash-verified at the destination and flushed to
  disk before any source copy is removed, so an interrupted move can't drop data.
- **Rollback log.** Every applied operation records its reverse, so
  `apply --rollback` (and `--continue-rollback` for an interrupted one) can walk
  the collection back.
- **Quarantine, not delete.** Files that don't belong are moved to a quarantine
  store under their full content hash — reviewable, restorable, and only removed
  by an explicit `quarantine prune`.

## Command reference

| Command | What it does |
|---------|--------------|
| `init [path]` | Create a catalogue (data dir, default `~/.cat198x`). |
| `dat add\|remove\|list\|activate\|diff\|versions\|fetch\|upgrade` | Manage reference DATs and their versions. |
| `source add\|remove\|list` | Register ROM directories to scan (never deletes files). |
| `scan [--source] [--full]` | Hash files into the inventory; incremental by default, `--full` rehashes everything. |
| `status [collection] [--detailed] [--merge-mode]` | Show completeness against a DAT. |
| `stats` | Summary across all collections. |
| `config set\|get\|list` | Per-collection settings (destination path, output format, merge mode). |
| `plan [--dat]` | Generate a reorganisation plan. |
| `apply [--dry-run] [--skip-space-check] [--rollback] [--continue-rollback]` | Execute or undo a plan. |
| `quarantine status\|prune\|restore` | Manage files set aside as not-in-DAT. |
| `torrent create\|verify` | Create or verify `.torrent` files for a directory. |
| `export <collection> [--output] [--format] [--have] [--missing]` | Export status as txt/csv/json. |
| `doctor [--fix]` | Check (and optionally repair) the installation. |
| `completions <shell>` | Generate shell completions. |
| `update [--check] [--force]` | Self-update from GitHub releases. |

Global flags: `--verbose`, `--quiet`, `--config <file>` (`CAT198X_CONFIG`),
`--data-dir <dir>` (`CAT198X_DATA_DIR`).

## DAT files

A DAT is an XML database (Logiqx / clrmamepro format) describing a known-good set
of ROMs — each entry's name, size, and hashes. Cat198x matches your files
against the **active** DAT for each collection. DATs are versioned: import a newer
one with `dat upgrade`, compare with `dat diff`, and switch the active version
with `dat activate`.

Supported archive formats for scanning: ZIP (Deflate/Store) and 7z. Headered
console ROMs are hashed both with and without their header, so they match both
headered and headerless DATs.

## Project status

Cat198x is part of the 198x family of retro-computing tooling, alongside the
Code198x curriculum and the Emu198x emulator. It is under active development; the
command surface above is stable, and the safety guarantees are covered by the
test suite.

## License

MIT.
