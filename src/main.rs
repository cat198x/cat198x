use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::generate;
use std::io;

use cat198x::cli::args::{Cli, Commands};
use cat198x::cli::{
    apply as apply_cmd, catalogue as catalogue_cmd, clean_superseded as clean_superseded_cmd,
    config as config_cmd, dat as dat_cmd, doctor as doctor_cmd, export as export_cmd, init,
    mcp as mcp_cmd, plan as plan_cmd, prune as prune_cmd, quarantine as quarantine_cmd,
    reclaim as reclaim_cmd, scan, source, stats as stats_cmd, status, torrent as torrent_cmd,
    unknowns as unknowns_cmd, update as update_cmd,
};

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
