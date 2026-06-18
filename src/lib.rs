//! Cat198x - A cross-platform CLI for managing retro gaming ROM collections
//!
//! This library provides the core functionality for managing ROM collections,
//! including DAT file parsing, file scanning, and database operations.

pub mod archive;
pub mod cli;
pub mod config;
pub mod dat;
pub mod db;
pub mod error;
pub mod filter;
pub mod ops;
pub mod plan;
pub mod scanner;
pub mod util;

// Re-export commonly used types at crate root for convenience
pub use dat::DatSourceType;

// Re-export command enums for use in tests
use clap::Subcommand;

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
    },

    /// Remove a source directory (does not delete files)
    Remove {
        /// Path to directory
        path: std::path::PathBuf,
    },

    /// List registered source directories
    List,
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
