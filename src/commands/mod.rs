//! Implementation of cargo-hold subcommands.
//!
//! `mod.rs` now serves as a thin dispatcher and re-export hub; command logic
//! lives in dedicated modules (`anchor`, `heave`, `voyage`).

use std::path::{Path, PathBuf};

use crate::cli::{Cli, Commands};
use crate::error::{HoldError, Result};

pub(crate) mod anchor;
pub(crate) mod heave;
pub(crate) mod voyage;

pub use anchor::{anchor, bilge, salvage, stow};
pub use heave::{Heave, HeaveBuilder};
pub use voyage::{Voyage, VoyageBuilder};

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
            max_target_size,
            auto_max_target_size,
            dry_run,
            debug,
            preserve_cargo_binaries,
            age_threshold_days,
        } => Heave::builder()
            .target_dir(&target_dir)
            .max_target_size(max_target_size.as_deref())
            .auto_max_target_size(*auto_max_target_size)
            .dry_run(*dry_run)
            .debug(*debug)
            .preserve_cargo_binaries(preserve_cargo_binaries)
            .age_threshold_days(*age_threshold_days)
            .verbose(verbose)
            .metadata_path(&metadata_path)
            .quiet(quiet)
            .build()
            .heave(),
        Commands::Voyage {
            max_target_size,
            gc_dry_run,
            gc_debug,
            preserve_cargo_binaries,
            gc_age_threshold_days,
            gc_auto_max_target_size,
        } => Voyage::builder()
            .metadata_path(&metadata_path)
            .target_dir(&target_dir)
            .max_target_size(max_target_size.as_deref())
            .gc_dry_run(*gc_dry_run)
            .gc_debug(*gc_debug)
            .preserve_cargo_binaries(preserve_cargo_binaries)
            .gc_age_threshold_days(*gc_age_threshold_days)
            .gc_auto_max_target_size(*gc_auto_max_target_size)
            .verbose(verbose)
            .quiet(quiet)
            .working_dir(&current_dir)
            .build()?
            .run(),
    }
}
