# ROMShelf Technical Specification

**Version 2.3 — December 2024**

## Executive Summary

ROMShelf is a cross-platform ROM collection manager inspired by tools like RomVault and CLRMamePro, but designed with modern sensibilities: a clean CLI interface following Terraform-style workflows, proper versioning of DAT files, and a shared core library enabling CLI, TUI, and GUI frontends.

The tool manages ROM collections against DAT files from sources including MAME, TOSEC, No-Intro, and Redump. It supports multiple archive formats, handles MAME's complex parent/clone relationships, and provides comprehensive status reporting with hierarchical rollups.

### Key Differentiators

- **Terraform-style workflow**: scan → plan → apply with forced two-step execution
- **DAT versioning**: collections maintain version history, upgrade between versions cleanly
- **TOSEC hierarchy injection**: flat DAT files automatically organised into correct tree structure
- **Diff archives**: generate update packs between DAT versions with torrent support
- **Content-addressed matching**: same ROM satisfies multiple DATs without duplication tracking
- **Cross-platform**: Rust core with CLI, TUI (ratatui), and future GUI (Tauri)

### Core Invariants

The following invariants are guaranteed throughout the system:

- **Path uniqueness**: No two active DAT entries can target the same destination path
- **Content addressing**: File identity is determined by SHA1 hash, never by path or name
- **Reversibility**: All apply operations are logged with reverse operations for rollback
- **Literal naming**: Archive entry names and ROM filenames are never normalised or case-folded
- **Determinism**: Same inputs always produce same outputs (plans, manifests, TorrentZIPs)
- **Idempotency**: Running scan, plan, or apply twice with unchanged inputs produces identical results
- **Atomicity**: Operations succeed or fail at file level; partial archives are never written
- **Isolation**: Inactive DAT versions have no effect on planning or status

---

## Processing Pipeline

The system processes data through distinct phases:

| Phase | Command | Description |
|-------|---------|-------------|
| 1 | `dat add` | Import DAT files → populate dat_nodes, dat_games, dat_roms |
| 2 | `source add` | Register source directories for scanning |
| 3 | `scan` | Hash source files → populate files, file_locations |
| 4 | `status` | Join DAT requirements with file catalog → compute completeness |
| 5 | `plan` | Compare current vs desired state → generate operations |
| 6 | `apply` | Execute operations → write files, update catalog |

Each phase is independent and repeatable. Scan can be run without plan. Status can be checked without planning. Plans can be regenerated without applying.

---

## Data Directory Structure

ROMShelf stores all state in a `.romshelf` directory, conceptually similar to Git's `.git` directory:

```
.romshelf/
├── db.sqlite              # All metadata, queries, state (authoritative)
├── config.toml            # User-editable configuration
├── objects/               # Content-addressed artifacts
│   ├── plans/             # Persisted plans by state hash
│   │   └── abc123.json
│   └── logs/              # Apply operation logs
│       └── def456.json
├── quarantine/            # Quarantined ROM files
└── cache/                 # Temporary staging data
```

**Design rationale**: SQLite provides ACID transactions, efficient queries, and indexing for the relational metadata. Content-addressed artifacts (plans, logs) are stored as files for inspectability and easy backup.

---

## Core Concepts

### Three-Phase Workflow

```
┌─────────┐      ┌─────────┐      ┌─────────┐
│  SCAN   │ ───▶ │  PLAN   │ ───▶ │  APPLY  │
└─────────┘      └─────────┘      └─────────┘
     │                │                │
     ▼                ▼                ▼
┌─────────┐      ┌─────────┐      ┌─────────┐
│ Update  │      │ Compare │      │ Execute │
│ file    │      │ current │      │ file    │
│ catalog │      │ vs      │      │ ops     │
│         │      │ desired │      │         │
└─────────┘      └─────────┘      └─────────┘

Safe &           No side         Idempotent
repeatable       effects         & logged
```

### Terraform Analogy

| Terraform | ROMShelf | Description |
|-----------|----------|-------------|
| HCL config | DAT files + config | Desired state definition |
| State file | SQLite database | Current state tracking |
| Providers | Format handlers | ZIP, 7Z, TorrentZIP, loose |
| Resources | ROMs/Games/Sets | Managed entities |
| refresh | scan | Discover current state |
| plan | plan | Compute diff, preview changes |
| apply | apply | Execute changes |

### Collections and Versions

ROMShelf groups DATs into collections (MAME, TOSEC, No-Intro) with explicit versions. Only one version is active at a time per collection, enabling:

- Version comparison and diff generation
- Clean upgrade paths between DAT versions
- Configuration inheritance across versions
- Storage efficiency (one complete TOSEC set is enough)

### Content-Addressed Identity

ROMs are identified by hash (SHA1 primary, MD5/CRC32 fallback), not filename. The same physical file can satisfy requirements in multiple DATs.

---

## Behavioural Rules

### Version Filtering and Active Versions

Only one version per collection can be active at any time. The `--dat` filter operates on paths within active DATs only.

```bash
# If MAME 0.266 is active and 0.265 is inactive:
romshelf plan --dat "MAME 0.265"    # ERROR: No matching active DATs
romshelf plan --dat "MAME 0.266"    # OK: Matches active version
romshelf plan --dat "MAME/**"       # OK: Matches all paths in active MAME version
```

### Source Preference Order

When multiple sources contain the same hash, ROMShelf uses a deterministic preference order:

1. Loose file preferred over archive entry
2. ZIP preferred over 7Z (faster extraction)
3. Shorter filesystem path preferred
4. Alphabetically first path as final tiebreaker

Configurable via:
```toml
[defaults]
prefer_source_order = ["loose", "zip", "7z"]
```

### Destination Collisions

If two different files would write to the same destination path, the plan fails:

```
Error: Output conflicts detected:

  ~/ROMs/MAME/galaga.zip:
    Source 1: /sources/set-a/galaga.zip (sha1: abc123)
    Source 2: /sources/set-b/galaga.zip (sha1: def456)

    ERROR: Different files, same destination.
    Resolve manually or use --prefer-source <path>
```

### Case Conflict Resolution

On case-insensitive filesystems, filename conflicts are detected during planning:

```
⚠ Case conflicts detected (will cause issues on Windows/macOS):
  Pacman.zip vs PACMAN.zip
```

The `--rename-conflicts` flag appends suffixes deterministically.

### Archive Entry Case Sensitivity

Archive entry names (inside ZIP/7Z) are always matched case-sensitively, regardless of host filesystem.

### File Already Correct at Destination

If a file already exists at the destination path AND matches the required hash, no operation is generated.

### ROM Name Normalisation

ROM names are used exactly as specified in DAT files, with no normalisation. Case, spelling, and all characters are literal.

### REPACK Triggers

A REPACK operation is generated when the content hash is correct but the container is wrong:

- Wrong archive format (e.g., ZIP when TorrentZIP configured)
- Wrong merge mode structure (e.g., non-merged when split configured)
- Wrong internal filenames (case, extension, or spelling differs from DAT)
- Loose files when archive format configured
- Archive when loose format configured

### Games With Zero ROMs

Games with no ROM children are counted as complete by definition: 0 ROMs required = 100% complete.

---

## State and Lifecycle

### State Hash Definition

Plans include a `state_hash` computed as:

```
state_hash = SHA256(
    active_version_ids ||
    file_catalog_fingerprint ||
    dest_config_hash
)
```

Components:
- **active_version_ids**: Sorted list of currently active collection_version IDs
- **file_catalog_fingerprint**: Hash of (row_count, max_last_seen) from file_locations
- **dest_config_hash**: Hash of all destination path configurations

### Rollback Semantics

Apply operations are logged with reverse operations:

- `romshelf apply --rollback` executes reverse operations in reverse order
- `romshelf apply --rollback --continue` retries failed rollback operations

### Quarantine Triggers

Files are moved to quarantine when:

- **Set removed**: A file exists in destination matching OLD DAT but not NEW DAT
- **Content changed**: A file would be overwritten with different content
- **Path changed**: A file is no longer needed at its current location by any active DAT

**Important**: Quarantine is NOT a source for matching. It is a holding pen for potentially unwanted files.

---

## CLI Command Structure

### Command Overview

```
romshelf
├── init                    # Create new database
├── dat                     # DAT management
│   ├── add <path>          # Import DAT file or directory
│   ├── list                # List imported DATs
│   ├── remove <path>       # Remove a DAT
│   ├── activate <path>     # Include in plan/status
│   ├── deactivate <path>   # Exclude from plan/status
│   ├── diff <from> <to>    # Compare two versions
│   ├── upgrade <old> <new> # Convenience: add new, deactivate old
│   ├── fetch <source>      # Download DATs from known sources
│   └── versions <n>        # List versions of a collection
├── source                  # Source management
│   ├── add <path>          # Add scan source
│   ├── list                # List sources
│   └── remove <path>       # Remove source
├── config                  # Configuration
│   ├── set [path] <k> <v>  # Set config (global or per-DAT)
│   ├── get [path] <key>    # Get config
│   ├── unset <path> <key>  # Remove per-DAT override
│   └── list [path]         # Show config
├── scan                    # Hash sources, update catalog
├── status [path]           # Show completeness
├── plan [--dat <path>]     # Generate plan
├── apply                   # Execute pending plan
├── export                  # Export collection/diff
│   ├── <path>              # Export a DAT subtree
│   └── --diff <from> <to>  # Export diff archive
├── quarantine              # Manage quarantined files
│   ├── status              # Show quarantine contents
│   ├── prune [path]        # Delete quarantined files
│   └── restore [path]      # Move back to sources
├── torrent                 # Torrent operations
│   ├── create <path>       # Generate .torrent for folder
│   └── verify <torrent>    # Check collection against torrent
├── doctor                  # Health checks
├── update                  # Self-update
└── completions <shell>     # Generate shell completions
```

### Exit Codes

| Code | Name | Meaning |
|------|------|---------|
| 0 | Success | Operation completed successfully |
| 1 | Error | General error |
| 2 | UsageError | Invalid arguments |
| 3 | DatabaseError | DB locked, corrupt, migration failed |
| 4 | IoError | File not found, permission denied |
| 5 | PartialSuccess | Some operations succeeded, some failed |
| 6 | NoPlan | Apply called without plan |
| 7 | PlanStale | Plan invalidated by state change |
| 130 | Interrupted | SIGINT (Ctrl+C) |

---

## MAME-Specific Handling

### Merge Modes

**Non-merged** (every game self-contained):
```
pacman.zip
├── pacman.5e
├── pacman.5f
└── prom.7f
mspacman.zip
├── mspacman.5e
├── mspacman.5f
└── prom.7f          ← duplicated
```

**Split** (clones only have unique ROMs):
```
pacman.zip
├── pacman.5e
├── pacman.5f
└── prom.7f          ← shared ROMs live here only
mspacman.zip
├── mspacman.5e
└── mspacman.5f      ← no prom.7f, inherited from parent
```

**Merged** (parent contains everything):
```
pacman.zip
├── pacman.5e
├── pacman.5f
├── prom.7f
├── mspacman.5e      ← clone ROMs merged in
└── mspacman.5f
(no mspacman.zip exists)
```

### Mechanical Sets

Mechanical sets (is_mechanical flag) are arcade redemption games, slot machines, etc.

Default behaviour:
- Excluded from completeness calculations
- Status displays mechanical count separately
- Configurable via `[mame] include_mechanicals = false`

### Completion Scoring Formula

```
completion = have / (total - nodumps)
```

Where:
- **have** = ROMs with status 'good' or 'baddump' that we possess
- **total** = all ROMs in DAT (including baddump and nodump)
- **nodumps** = ROMs with 'nodump' status (excluded from denominator)

---

## TOSEC-Specific Handling

### Filename Parsing

TOSEC DAT filenames encode hierarchy:

```
Commodore Amiga - Games - [ADF] (TOSEC-v2024-01-15).dat
│         │       │       │     └── Version
│         │       │       └── Format specifier
│         │       └── Category
│         └── System
└── Manufacturer
```

---

## Header-Aware Matching (Future Phase)

### The Problem

Some ROM formats include metadata headers:
- NES/Famicom: 16-byte iNES header
- Famicom Disk System: 16-byte FDS header
- Atari 7800: 128-byte A78 header
- Atari Lynx: 64-byte LNX header

No-Intro DATs list hashes of the payload only (without header), but emulators require headers.

### The Solution

Header-aware matching computes multiple hashes without modifying files:

1. During scan: Detect header magic bytes
2. If header detected: Compute `raw_sha1` AND `payload_sha1`
3. Store both hashes in the files table
4. During matching: Use appropriate hash based on DAT source

---

## Implementation Phases

### Phase 1: Foundation ✅
- Workspace setup
- Database schema and migrations
- Configuration loading (TOML parsing, platform paths)
- CLI structure with clap
- `romshelf init` command
- `romshelf config get/set/list` commands

**Deliverable**: Installable CLI that initialises database and manages config

### Phase 2: DAT Import ✅
- Logiqx XML parser
- ClrMamePro parser
- Format auto-detection
- TOSEC filename parsing and hierarchy injection
- MAME parent/clone/BIOS/device extraction
- Collection versioning logic
- `romshelf dat add/list/remove/activate/deactivate` commands

**Deliverable**: Can import MAME and TOSEC DATs, view hierarchy

### Phase 3: Scanning ✅
- Filesystem walker with symlink following (traverses into symlinked directories)
- Hash computation (CRC32, MD5, SHA1)
- ZIP and 7Z archive reading
- Case sensitivity detection per source
- Parallel scanning with rayon
- Incremental scanning (skip unchanged files based on modification time)
- Scan interruption with graceful progress saving
- `romshelf source add/list/remove` and `scan` commands

**Fully implemented**

**Deliverable**: Can scan directories and archives, catalog files by hash

### Phase 4: Status and Matching ✅
- Hash-based matching (SHA1 primary)
- Hierarchical rollup calculations
- `romshelf status` command with drill-down
- MAME merge-mode aware completeness (non-merged, split, merged)
- BIOS/device set tracking and exclusion options
- Mechanical set exclusion
- Nodump ROM handling (excluded from completeness calculations)

**Fully implemented**

**Deliverable**: Can see collection completeness with hierarchical display and MAME-aware statistics

### Phase 5: Planning and Applying ✅
- Plan generation with state hash
- Plan persistence (JSON format)
- Plan validation (state hash checking)
- ZIP writing
- File copy/move operations with verification
- Operation logging for rollback
- Rollback implementation with `--continue` option
- Disk space pre-check
- `romshelf plan` and `apply` commands

**Fully implemented**: All Phase 5 items complete

**Deliverable**: Full end-to-end workflow functioning

### Phase 6: Polish and Extras ❌
- DAT diff command
- Export command with partial torrent handling
- Deterministic manifest generation
- Torrent generation with auto piece size
- Doctor command with health checks and `--fix`
- Self-update mechanism
- Shell completions generation
- DAT fetching with env var credential fallback

**Deliverable**: Feature-complete CLI (v1.0.0)

---

## Database Schema

### Collections and Versions

```sql
CREATE TABLE collections (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    collection_type TEXT NOT NULL
);

CREATE TABLE collection_versions (
    id INTEGER PRIMARY KEY,
    collection_id INTEGER NOT NULL REFERENCES collections(id),
    version TEXT NOT NULL,
    imported_at TIMESTAMP NOT NULL,
    active BOOLEAN DEFAULT FALSE,
    UNIQUE(collection_id, version)
);

CREATE TABLE dat_nodes (
    id INTEGER PRIMARY KEY,
    version_id INTEGER NOT NULL REFERENCES collection_versions(id) ON DELETE CASCADE,
    parent_id INTEGER REFERENCES dat_nodes(id),
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    depth INTEGER NOT NULL,
    dat_file_path TEXT,
    UNIQUE(version_id, path)
);
```

### Games and ROMs

```sql
CREATE TABLE dat_games (
    id INTEGER PRIMARY KEY,
    node_id INTEGER NOT NULL REFERENCES dat_nodes(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    clone_of TEXT,
    rom_of TEXT,
    is_bios BOOLEAN DEFAULT FALSE,
    is_device BOOLEAN DEFAULT FALSE,
    is_mechanical BOOLEAN DEFAULT FALSE,
    UNIQUE(node_id, name)
);

CREATE TABLE dat_roms (
    id INTEGER PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES dat_games(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    crc32 TEXT,
    md5 TEXT,
    sha1 TEXT,
    merge TEXT,
    status TEXT DEFAULT 'good',
    UNIQUE(game_id, name)
);
```

### File Catalog

```sql
CREATE TABLE files (
    sha1 TEXT PRIMARY KEY,
    md5 TEXT,
    crc32 TEXT,
    size INTEGER NOT NULL,
    payload_sha1 TEXT,
    header_type TEXT
);

CREATE TABLE file_locations (
    id INTEGER PRIMARY KEY,
    sha1 TEXT NOT NULL REFERENCES files(sha1),
    path TEXT NOT NULL,
    archive_path TEXT,
    source_id INTEGER REFERENCES sources(id),
    last_seen TIMESTAMP NOT NULL,
    UNIQUE(path, archive_path)
);

CREATE TABLE sources (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    last_scanned TIMESTAMP,
    scan_cursor TEXT,
    scan_state TEXT,
    case_sensitive BOOLEAN
);
```

---

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| clap | CLI parsing with derive macros |
| rusqlite | SQLite with bundled feature |
| serde | Serialisation for config, plans, JSON |
| toml | Config file format |
| quick-xml | Logiqx DAT format parsing |
| zip | ZIP read and write |
| sevenz-rust | 7Z reading (read only, pure Rust) |
| sha1/md5/crc32fast | Hashing |
| indicatif | Progress bars |
| tracing | Structured logging |
| tempfile | Tests, atomic writes |

---

## Future Roadmap

Items deferred from MVP:

- **TUI Application**: Interactive terminal interface using ratatui
- **GUI Application**: Cross-platform GUI using Tauri
- **Content-Addressed Torrent Seeding**: Seed torrents by matching file hashes
- **Metadata Scraping**: Fetch game metadata from ScreenScraper, TheGamesDB, etc.
- **Emulator Launching**: Configuration-driven emulator integration
