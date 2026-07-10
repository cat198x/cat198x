//! Clap argument types for the CLI adapter.

use clap::{Parser, Subcommand};
use clap_complete::Shell;

/// Cat198x - A cross-platform CLI for managing retro gaming ROM collections
#[derive(Parser)]
#[command(name = "cat198x")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Suppress non-essential output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Path to configuration file
    #[arg(long, global = true, env = "CAT198X_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    /// Path to data directory (default: ~/.cat198x)
    #[arg(long, global = true, env = "CAT198X_DATA_DIR")]
    pub data_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize Cat198x in the current or specified directory
    Init {
        /// Directory to initialize (default: current directory)
        path: Option<std::path::PathBuf>,
    },

    /// Manage DAT files
    #[command(subcommand)]
    Dat(DatCommands),

    /// Manage source directories
    #[command(subcommand)]
    Source(SourceCommands),

    /// Scan source directories for ROM files
    Scan {
        /// Only scan specific sources (by path or ID)
        #[arg(short, long)]
        source: Option<Vec<String>>,

        /// Force full rescan (ignore cached hashes)
        #[arg(short, long)]
        full: bool,

        /// Only scan a subtree within the source, relative to its root (e.g.
        /// "Sinclair"). Lets a huge source be scanned in bounded chunks over a
        /// slow mount; files are still catalogued under the source. Requires a
        /// single source.
        #[arg(long, value_name = "SUBPATH")]
        path: Option<String>,
    },

    /// Show collection status and completeness
    Status {
        /// Collection name or pattern to show status for
        collection: Option<String>,

        /// Show detailed per-game status
        #[arg(short, long)]
        detailed: bool,

        /// Merge mode for MAME-style ROM sets (non-merged, split, merged)
        #[arg(short, long)]
        merge_mode: Option<String>,
    },

    /// List scanned files matched by no active DAT (written to a file for review)
    Unknowns,

    /// Record into the catalogue the library files a completed plan placed, and
    /// register the library as a source, so re-plans converge without re-hashing
    /// or re-transferring already-placed content. Reports by default.
    CataloguePlacements {
        /// Preview the count without writing to the catalogue.
        #[arg(long)]
        dry_run: bool,

        /// Restrict to saved plans whose file name contains this value (e.g. a
        /// plan hash). Default: every saved plan.
        #[arg(long)]
        plan: Option<String>,
    },

    /// Remove directories left empty after a `--move` tidy (e.g. emptied
    /// `ToSort/…` folders). Reports by default; only `fs::remove_dir` is used, so
    /// a non-empty directory can never be deleted.
    PruneEmpty {
        /// Limit to source roots whose id or path contains this value
        /// (repeatable). Default: every registered source.
        #[arg(long = "source", value_name = "ID|PATH")]
        source: Vec<String>,

        /// Actually remove the directories (default: report only).
        #[arg(long)]
        remove: bool,

        /// Also prune a directory holding only OS cruft (`.DS_Store`, `._*`,
        /// `Thumbs.db`, `desktop.ini`), deleting that cruft with it.
        #[arg(long)]
        ignore_os_junk: bool,
    },

    /// Remove loose files stranded under the library beside the canonical
    /// archive that already holds their content (e.g. the MAME loose layer left
    /// after the per-machine-zip split). A file goes only when its content is
    /// preserved in the canonical archive the active DAT assigns it, that archive
    /// is a desired-state member, the file is not itself a canonical destination,
    /// and a surviving copy is verified on disk. Dry-run unless `--execute`.
    CleanSuperseded {
        /// Limit the candidate scan to these sets - the first path segment under
        /// the library (e.g. `MAME`). Repeatable. Default: the whole library. The
        /// safety checks always consider every collection, whatever the scope.
        #[arg(long = "set", value_name = "SET")]
        set: Option<Vec<String>>,

        /// Actually remove the files (default: report only). Each is a
        /// verify-before-delete hard delete and is journaled for audit.
        #[arg(long)]
        execute: bool,
    },

    /// Free space by deleting a source's files whose every content is already
    /// held in another source (e.g. a `ToSort/…` staging input after its set was
    /// moved into the library). Dry-run unless `--execute`.
    Reclaim {
        /// The source to reclaim from: a source id or a path substring.
        #[arg(value_name = "ID|PATH")]
        source: Option<String>,

        /// Actually delete the redundant files (default: report only). Each is an
        /// existence-verified hard delete and is journaled for audit.
        #[arg(long)]
        execute: bool,
    },

    /// Show overall statistics across all collections
    Stats {
        /// Roll collections up by a dimension: "system" (leading name segment,
        /// e.g. all "Sinclair ZX Spectrum - *") or "set" (top of the library
        /// path, e.g. all of TOSEC-PIX). Flat if omitted.
        #[arg(short = 'g', long = "group-by", value_name = "BY")]
        group_by: Option<String>,
    },

    /// Configure collection settings (destination path, output format)
    #[command(subcommand)]
    Config(ConfigCommands),

    /// Generate a plan for reorganising ROMs
    Plan {
        /// Only plan for specific DAT paths (glob patterns supported)
        #[arg(long)]
        dat: Option<String>,

        /// Only plan these sets - the top segment of the library path (e.g.
        /// "TOSEC", "TOSEC-PIX"). Repeatable; scopes a phase to chosen sets.
        #[arg(long)]
        set: Option<Vec<String>>,
    },

    /// Apply a previously generated plan
    Apply {
        /// Dry run - show what would be done without making changes
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Skip disk space check before applying
        #[arg(long)]
        skip_space_check: bool,

        /// Defer repack operations (the expensive read-and-recompress ones),
        /// applying only the cheap moves and quarantines now. Run `apply` again
        /// without this flag to complete the deferred repacks.
        #[arg(long)]
        skip_repack: bool,

        /// Number of placement (copy/move/relocate) and repack operations to run
        /// concurrently. These are latency-bound over a network mount, so keeping
        /// several in flight overlaps the round trips; deletes and quarantines
        /// still run one at a time, in plan order. 1 reproduces serial apply.
        #[arg(short = 'j', long, default_value_t = 6, value_parser = clap::value_parser!(u8).range(1..=64))]
        jobs: u8,

        /// Rollback the most recent apply operation
        #[arg(long)]
        rollback: bool,

        /// Continue a previously failed rollback
        #[arg(long, requires = "rollback")]
        continue_rollback: bool,

        /// After a `--move` tidy, remove the now-empty source directories it left
        /// behind (e.g. emptied `ToSort/…` folders). Runs once when the apply
        /// finishes; only `fs::remove_dir` is used, so a non-empty directory is
        /// never deleted. For finer control (preview, OS-cruft) use `prune-empty`.
        #[arg(long)]
        prune_empty: bool,
    },

    /// Manage quarantined files
    #[command(subcommand)]
    Quarantine(QuarantineCommands),

    /// Create and verify torrent files
    #[command(subcommand)]
    Torrent(TorrentCommands),

    /// Check Cat198x installation health
    Doctor {
        /// Attempt to fix problems automatically
        #[arg(long)]
        fix: bool,
    },

    /// Export collection status to file
    Export {
        /// Collection name to export
        collection: String,

        /// Output file path (default: stdout)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,

        /// Output format (txt, csv, json) - auto-detected from extension if not specified
        #[arg(short, long)]
        format: Option<String>,

        /// Only export ROMs you have
        #[arg(long)]
        have: bool,

        /// Only export ROMs you're missing
        #[arg(long)]
        missing: bool,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Run a Model Context Protocol server over stdio, exposing the read-only
    /// operations (status, plan-as-diff, collection/source lists) as MCP tools
    /// so an agent can drive Cat198x headlessly. stdout carries JSON-RPC; logs
    /// go to stderr.
    Mcp,

    /// Update Cat198x to the latest version
    Update {
        /// Only check for updates, don't install
        #[arg(long)]
        check: bool,

        /// Force update even if already at latest version
        #[arg(long)]
        force: bool,
    },
}

/// DAT file management commands
#[derive(Subcommand, Clone, Debug)]
pub enum DatCommands {
    /// Add a DAT file to the database
    Add {
        /// Path to a DAT file, or a directory when used with --recursive
        path: std::path::PathBuf,

        /// Collection name (auto-detected from DAT if not specified).
        /// Ignored with --recursive, where each DAT names its own collection.
        #[arg(short, long)]
        collection: Option<String>,

        /// Add every .dat/.xml file found under the given directory
        #[arg(short, long)]
        recursive: bool,
    },

    /// Remove a DAT file/collection
    Remove {
        /// Collection name or DAT path to remove
        target: String,

        /// Remove all versions, not just the active one
        #[arg(long)]
        all_versions: bool,
    },

    /// Re-point registrations whose DAT file has moved, by finding a same-named
    /// DAT under the given directory
    Relink {
        /// Directory to search for the moved DAT files (searched recursively)
        dir: std::path::PathBuf,
    },

    /// Sort a flat DAT pack into a nested tree by collection name, ready for a
    /// recursive `dat add` that records the hierarchy
    Sort {
        /// Flat directory of DAT files to sort (searched recursively)
        pack: std::path::PathBuf,

        /// Destination root for the nested tree
        dest: std::path::PathBuf,
    },

    /// Re-parse stored DAT files and correct collection names mangled by an
    /// earlier parser that mishandled XML entities (e.g. "Shoot&apos;em Up"
    /// stored as "em Up"). Surgical: only names are rewritten, in place.
    RepairNames,

    /// List imported DAT files
    List {
        /// Show all versions, not just active
        #[arg(short, long)]
        all: bool,
    },

    /// Activate a specific DAT version
    #[command(disable_version_flag = true)]
    Activate {
        /// Collection name
        collection: String,

        /// Version to activate
        version: String,
    },

    /// Show differences between DAT versions
    Diff {
        /// Collection name
        collection: String,

        /// First version (default: previous active)
        #[arg(short, long)]
        from: Option<String>,

        /// Second version (default: current active)
        #[arg(short, long)]
        to: Option<String>,
    },

    /// List all versions of a collection
    Versions {
        /// Collection name
        collection: String,
    },

    /// Download DAT files from known sources
    Fetch {
        /// Source name (e.g., "mame", "fbneo") - use --list to see options
        source: Option<String>,

        /// Download from a custom URL instead of known source
        #[arg(long)]
        url: Option<String>,

        /// Output path for downloaded file
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,

        /// List available DAT sources
        #[arg(short, long)]
        list: bool,
    },

    /// Upgrade a collection: add new DAT and deactivate old version
    Upgrade {
        /// Path to new DAT file
        path: std::path::PathBuf,

        /// Collection name (auto-detected from DAT if not specified)
        #[arg(short, long)]
        collection: Option<String>,
    },
}

/// Source directory management commands
#[derive(Subcommand, Clone, Debug)]
pub enum SourceCommands {
    /// Add a source directory
    Add {
        /// Path to directory
        path: std::path::PathBuf,
        /// Preserve this source - its content is never removed (reference
        /// master). Overrides the role-based default.
        #[arg(long, conflicts_with = "consume")]
        preserve: bool,
        /// Consume this source - staging that may be emptied as content is
        /// placed. Overrides the role-based default.
        #[arg(long)]
        consume: bool,
    },

    /// Remove a source directory (does not delete files)
    Remove {
        /// Path to directory
        path: std::path::PathBuf,
    },

    /// List registered source directories
    List,

    /// Set whether a source is consumed (emptied) or preserved
    SetDisposition {
        /// Path to a registered source directory
        path: std::path::PathBuf,
        /// `consume` (staging, may be emptied) or `preserve` (content kept)
        disposition: String,
    },
}

/// Configuration management commands
#[derive(Subcommand, Clone, Debug)]
pub enum ConfigCommands {
    /// Set a configuration value for a collection
    Set {
        /// Collection name
        collection: String,

        /// Configuration key (dest_path, output_format, merge_mode)
        key: String,

        /// Value to set
        value: String,
    },

    /// Set a library-wide default (applies to collections without their own value)
    SetDefault {
        /// Configuration key (dest_path, output_format, merge_mode)
        key: String,

        /// Value to set
        value: String,
    },

    /// Show the library-wide defaults (all, or a specific key)
    GetDefault {
        /// Configuration key (dest_path, output_format, merge_mode); all if omitted
        key: Option<String>,
    },

    /// Get a configuration value for a collection
    Get {
        /// Collection name
        collection: String,

        /// Configuration key (optional, shows all if omitted)
        key: Option<String>,
    },

    /// List all collection configurations
    List {
        /// Collection name (optional, shows all if omitted)
        collection: Option<String>,
    },
}

/// Torrent file operations
#[derive(Subcommand, Clone, Debug)]
pub enum TorrentCommands {
    /// Generate a .torrent file for a directory
    Create {
        /// Path to directory to create torrent from
        path: std::path::PathBuf,

        /// Output path for .torrent file (default: <dirname>.torrent)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,

        /// Piece size in bytes (auto-calculated if not specified)
        #[arg(long)]
        piece_size: Option<u64>,

        /// Tracker announce URL(s) - can be specified multiple times
        #[arg(short, long)]
        tracker: Vec<String>,

        /// Comment to include in torrent
        #[arg(short, long)]
        comment: Option<String>,

        /// Mark as private torrent (disables DHT/PEX)
        #[arg(long)]
        private: bool,
    },

    /// Verify files against a .torrent file
    Verify {
        /// Path to .torrent file
        torrent: std::path::PathBuf,

        /// Directory containing files to verify
        #[arg(short, long)]
        path: Option<std::path::PathBuf>,
    },
}

/// Quarantine management commands
#[derive(Subcommand, Clone, Debug)]
pub enum QuarantineCommands {
    /// Show quarantine status and contents
    Status {
        /// Collection name pattern to filter (optional)
        collection: Option<String>,

        /// Show detailed per-file listing
        #[arg(short, long)]
        detailed: bool,
    },

    /// Permanently delete quarantined files
    Prune {
        /// Collection name pattern to filter (optional)
        collection: Option<String>,

        /// Delete without confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Restore quarantined files back to a source directory
    Restore {
        /// Collection name pattern to filter (optional)
        collection: Option<String>,

        /// Target source directory to restore to
        #[arg(short, long)]
        target: Option<std::path::PathBuf>,

        /// Restore without confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },
}
