//! Implementation of cargo-hold subcommands.

use std::path::{Path, PathBuf};

use crate::cli::{Cli, Commands};
use crate::error::{HoldError, Result};

pub mod anchor;
pub mod bilge;
pub mod gc_options;
pub mod heave;
pub mod salvage;
pub mod stow;
pub mod voyage;

use anchor::anchor;
use bilge::bilge;
use heave::Heave;
use salvage::salvage;
use stow::stow;
use voyage::Voyage;

#[cfg(test)]
mod tests;

/// Execute commands based on the parsed CLI arguments.
pub fn execute(cli: &Cli) -> Result<()> {
    execute_with_dir(cli, None)
}

/// Execute commands with an explicit working directory.
pub fn execute_with_dir(cli: &Cli, working_dir: Option<&Path>) -> Result<()> {
    let quiet = cli.global_opts().quiet();
    let verbose = if quiet {
        0
    } else {
        cli.global_opts().verbose()
    };

    let current_dir = if let Some(dir) = working_dir {
        dir.to_path_buf()
    } else {
        std::env::current_dir().map_err(|source| HoldError::IoError {
            path: PathBuf::from("."),
            source,
        })?
    };

    let metadata_path = cli.global_opts().get_metadata_path();
    let target_dir = cli.global_opts().get_target_dir();

    match cli.command() {
        Commands::Anchor => anchor(&metadata_path, verbose, quiet, &current_dir),
        Commands::Salvage => salvage(&metadata_path, verbose, quiet, &current_dir),
        Commands::Stow => stow(&metadata_path, verbose, quiet, &current_dir),
        Commands::Bilge => bilge(&metadata_path, verbose, quiet),
        Commands::Heave {
            gc,
            auto_max_target_size,
            dry_run,
            debug,
            age_threshold_days,
        } => Heave::builder()
            .target_dir(&target_dir)
            .max_target_size(gc.max_target_size())
            .auto_max_target_size(*auto_max_target_size)
            .dry_run(*dry_run)
            .debug(*debug)
            .preserve_cargo_binaries(gc.preserve_cargo_binaries())
            .age_threshold_days(*age_threshold_days)
            .verbose(verbose)
            .metadata_path(&metadata_path)
            .quiet(quiet)
            .build()?
            .heave(),
        Commands::Voyage {
            gc,
            gc_dry_run,
            gc_debug,
            gc_age_threshold_days,
            gc_auto_max_target_size,
        } => Voyage::builder()
            .metadata_path(&metadata_path)
            .target_dir(&target_dir)
            .max_target_size(gc.max_target_size())
            .gc_dry_run(*gc_dry_run)
            .gc_debug(*gc_debug)
            .preserve_cargo_binaries(gc.preserve_cargo_binaries())
            .gc_age_threshold_days(*gc_age_threshold_days)
            .gc_auto_max_target_size(*gc_auto_max_target_size)
            .verbose(verbose)
            .quiet(quiet)
            .working_dir(&current_dir)
            .build()?
            .run(),
    }
}
