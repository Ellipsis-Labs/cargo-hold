//! Bilge command implementation.

use std::path::Path;

use crate::error::Result;
use crate::logging::Logger;
use crate::metadata::clean_metadata;

/// Executes the bilge command (remove metadata file).
pub fn bilge(metadata_path: &Path, verbose: u8, quiet: bool) -> Result<()> {
    let log = Logger::new(verbose, quiet);
    log.verbose(1, format!("Bilging out metadata at {metadata_path:?}"));

    clean_metadata(metadata_path)?;

    log.verbose(1, "Metadata bilged successfully");

    Ok(())
}
