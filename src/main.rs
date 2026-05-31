use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use std::io;

use romshelf::cli::{apply as apply_cmd, config as config_cmd, dat as dat_cmd, doctor as doctor_cmd, export as export_cmd, init, plan as plan_cmd, quarantine as quarantine_cmd, scan, source, stats as stats_cmd, status, torrent as torrent_cmd, update as update_cmd};
use romshelf::{ConfigCommands, DatCommands, QuarantineCommands, SourceCommands, TorrentCommands};

/// ROMShelf - A cross-platform CLI for managing retro gaming ROM collections
#[derive(Parser)]
#[command(name = "romshelf")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Suppress non-essential output
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Path to configuration file
    #[arg(long, global = true, env = "ROMSHELF_CONFIG")]
    config: Option<std::path::PathBuf>,

    /// Path to data directory (default: ~/.romshelf)
    #[arg(long, global = true, env = "ROMSHELF_DATA_DIR")]
    data_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize ROMShelf in the current or specified directory
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

    /// Show overall statistics across all collections
    Stats,

    /// Configure collection settings (destination path, output format)
    #[command(subcommand)]
    Config(ConfigCommands),

    /// Generate a plan for reorganising ROMs
    Plan {
        /// Only plan for specific DAT paths (glob patterns supported)
        #[arg(long)]
        dat: Option<String>,
    },

    /// Apply a previously generated plan
    Apply {
        /// Dry run - show what would be done without making changes
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Skip disk space check before applying
        #[arg(long)]
        skip_space_check: bool,

        /// Rollback the most recent apply operation
        #[arg(long)]
        rollback: bool,

        /// Continue a previously failed rollback
        #[arg(long, requires = "rollback")]
        continue_rollback: bool,
    },

    /// Manage quarantined files
    #[command(subcommand)]
    Quarantine(QuarantineCommands),

    /// Create and verify torrent files
    #[command(subcommand)]
    Torrent(TorrentCommands),

    /// Check ROMShelf installation health
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

    /// Update ROMShelf to the latest version
    Update {
        /// Only check for updates, don't install
        #[arg(long)]
        check: bool,

        /// Force update even if already at latest version
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set up logging based on verbosity
    let log_level = if cli.quiet {
        tracing::Level::ERROR
    } else if cli.verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };

    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false)
        .init();

    // Handle commands
    match cli.command {
        Commands::Init { path } => init::run(path, cli.data_dir),
        Commands::Dat(cmd) => dat_cmd::run(cmd, cli.data_dir),
        Commands::Source(cmd) => source::run(cmd, cli.data_dir),
        Commands::Scan { source, full } => scan::run(source, full, cli.data_dir),
        Commands::Status {
            collection,
            detailed,
            merge_mode,
        } => status::run(collection, detailed, merge_mode, cli.data_dir),
        Commands::Stats => stats_cmd::run(cli.data_dir),
        Commands::Config(cmd) => config_cmd::run(cmd, cli.data_dir),
        Commands::Plan { dat } => plan_cmd::run(dat, cli.data_dir),
        Commands::Apply { dry_run, skip_space_check, rollback, continue_rollback } => {
            if rollback {
                apply_cmd::run_rollback(dry_run, continue_rollback, cli.data_dir)
            } else {
                apply_cmd::run(dry_run, skip_space_check, cli.data_dir)
            }
        }
        Commands::Quarantine(cmd) => quarantine_cmd::run(cmd, cli.data_dir),
        Commands::Torrent(cmd) => torrent_cmd::run(cmd),
        Commands::Doctor { fix } => doctor_cmd::run(fix, cli.data_dir),
        Commands::Export { collection, output, format, have, missing } => {
            export_cmd::run(&collection, output, format.as_deref(), have, missing, cli.data_dir)
        }
        Commands::Completions { shell } => {
            generate(shell, &mut Cli::command(), "romshelf", &mut io::stdout());
            Ok(())
        }
        Commands::Update { check, force } => update_cmd::run(check, force),
    }
}
