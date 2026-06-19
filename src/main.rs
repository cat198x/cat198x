use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use std::io;

use cat198x::cli::{
    apply as apply_cmd, catalogue as catalogue_cmd, clean_superseded as clean_superseded_cmd,
    config as config_cmd, dat as dat_cmd, doctor as doctor_cmd, export as export_cmd, init,
    mcp as mcp_cmd, plan as plan_cmd, prune as prune_cmd, quarantine as quarantine_cmd,
    reclaim as reclaim_cmd, scan, source, stats as stats_cmd, status, torrent as torrent_cmd,
    unknowns as unknowns_cmd, update as update_cmd,
};
use cat198x::{ConfigCommands, DatCommands, QuarantineCommands, SourceCommands, TorrentCommands};

/// Cat198x - A cross-platform CLI for managing retro gaming ROM collections
#[derive(Parser)]
#[command(name = "cat198x")]
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
    #[arg(long, global = true, env = "CAT198X_CONFIG")]
    config: Option<std::path::PathBuf>,

    /// Path to data directory (default: ~/.cat198x)
    #[arg(long, global = true, env = "CAT198X_DATA_DIR")]
    data_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
        /// Limit the candidate scan to these sets — the first path segment under
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

        /// Only plan these sets — the top segment of the library path (e.g.
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

    // The MCP server uses stdout for the JSON-RPC transport, so its logs must go
    // to stderr; every other command logs to stdout as before.
    if matches!(cli.command, Commands::Mcp) {
        tracing_subscriber::fmt()
            .with_max_level(log_level)
            .with_target(false)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_max_level(log_level)
            .with_target(false)
            .init();
    }

    // Handle commands
    match cli.command {
        Commands::Init { path } => init::run(path, cli.data_dir),
        Commands::Dat(cmd) => dat_cmd::run(cmd, cli.data_dir),
        Commands::Source(cmd) => source::run(cmd, cli.data_dir),
        Commands::Scan { source, full, path } => scan::run(source, full, path, cli.data_dir),
        Commands::Status {
            collection,
            detailed,
            merge_mode,
        } => status::run(collection, detailed, merge_mode, cli.data_dir),
        Commands::Unknowns => unknowns_cmd::run(cli.data_dir),
        Commands::CataloguePlacements { dry_run, plan } => {
            catalogue_cmd::run(dry_run, plan, cli.data_dir)
        }
        Commands::PruneEmpty {
            source,
            remove,
            ignore_os_junk,
        } => prune_cmd::run(source, remove, ignore_os_junk, cli.data_dir),
        Commands::CleanSuperseded { set, execute } => {
            clean_superseded_cmd::run(set, execute, cli.data_dir)
        }
        Commands::Reclaim { source, execute } => reclaim_cmd::run(source, execute, cli.data_dir),
        Commands::Stats { group_by } => stats_cmd::run(group_by.as_deref(), cli.data_dir),
        Commands::Config(cmd) => config_cmd::run(cmd, cli.data_dir),
        Commands::Plan { dat, set } => plan_cmd::run(dat, set, cli.data_dir),
        Commands::Apply {
            dry_run,
            skip_space_check,
            skip_repack,
            jobs,
            rollback,
            continue_rollback,
            prune_empty,
        } => {
            if rollback {
                apply_cmd::run_rollback(dry_run, continue_rollback, cli.data_dir)
            } else {
                apply_cmd::run(
                    dry_run,
                    skip_space_check,
                    skip_repack,
                    jobs as usize,
                    prune_empty,
                    cli.data_dir,
                )
            }
        }
        Commands::Mcp => mcp_cmd::run(cli.data_dir),
        Commands::Quarantine(cmd) => quarantine_cmd::run(cmd, cli.data_dir),
        Commands::Torrent(cmd) => torrent_cmd::run(cmd),
        Commands::Doctor { fix } => doctor_cmd::run(fix, cli.data_dir),
        Commands::Export {
            collection,
            output,
            format,
            have,
            missing,
        } => export_cmd::run(
            &collection,
            output,
            format.as_deref(),
            have,
            missing,
            cli.data_dir,
        ),
        Commands::Completions { shell } => {
            generate(shell, &mut Cli::command(), "cat198x", &mut io::stdout());
            Ok(())
        }
        Commands::Update { check, force } => update_cmd::run(check, force),
    }
}
