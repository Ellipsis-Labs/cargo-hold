//! Anchor command implementation.

use std::path::Path;

use super::salvage::{SalvageReport, salvage_with_report};
use super::stow::stow;
use crate::error::Result;
use crate::logging::Logger;

/// Executes the anchor command - the main orchestrator.
///
/// This command anchors your build state by performing the complete workflow:
/// 1. Restores timestamps from the metadata
/// 2. Scans for changes and saves the new state
///
/// This is the recommended command for CI use.
pub fn anchor(metadata_path: &Path, verbose: u8, quiet: bool, working_dir: &Path) -> Result<()> {
    anchor_with_report(metadata_path, verbose, quiet, working_dir).map(|_| ())
}

pub(crate) fn anchor_with_report(
    metadata_path: &Path,
    verbose: u8,
    quiet: bool,
    working_dir: &Path,
) -> Result<SalvageReport> {
    let log = Logger::new(verbose, quiet);
    log.info("⚓ Anchoring build state...");

    let report = salvage_with_report(metadata_path, verbose, quiet, working_dir)?;
    stow(metadata_path, verbose, quiet, working_dir)?;

    log.info("⚓ Build state anchored successfully");

    Ok(report)
}
