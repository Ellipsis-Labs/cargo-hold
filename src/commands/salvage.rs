//! Salvage command implementation.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::discovery::discover_tracked_files;
use crate::error::{HoldError, Result};
use crate::hashing::{get_file_size, hash_file};
use crate::logging::Logger;
use crate::metadata::load_metadata;
use crate::state::{FileState, StateMetadata};
use crate::timestamp::{generate_monotonic_timestamp, restore_timestamps};

#[derive(Debug, Default)]
pub(crate) struct SalvageReport {
    pub(crate) unchanged_files: usize,
    pub(crate) modified_files: usize,
    pub(crate) added_files: usize,
}

impl SalvageReport {
    pub(crate) fn has_source_changes(&self) -> bool {
        self.modified_files > 0 || self.added_files > 0
    }
}

/// Executes the salvage command.
///
/// Restores timestamps based on metadata content, assigning monotonic
/// timestamps to new or modified files.
pub fn salvage(metadata_path: &Path, verbose: u8, quiet: bool, working_dir: &Path) -> Result<()> {
    salvage_with_report(metadata_path, verbose, quiet, working_dir).map(|_| ())
}

pub(crate) fn salvage_with_report(
    metadata_path: &Path,
    verbose: u8,
    quiet: bool,
    working_dir: &Path,
) -> Result<SalvageReport> {
    let log = Logger::new(verbose, quiet);
    log.verbose(1, "Salvaging timestamps from metadata...");

    let metadata = load_metadata(metadata_path)?;

    if metadata.is_empty() {
        log.verbose(1, "Metadata is empty, nothing to restore");
        return Ok(SalvageReport::default());
    }

    if !log.quiet() && log.level() > 0 {
        eprintln!("Metadata:");
        eprintln!("  Format version: {}", metadata.version);
        eprintln!("  Tracked files: {}", metadata.len());
        eprintln!("  Metadata file: {}", metadata_path.display());
        if let Ok(metadata_info) = std::fs::metadata(metadata_path) {
            eprintln!("  Metadata size: {} bytes", metadata_info.len());
        }
    }

    let new_mtime = generate_monotonic_timestamp(&metadata);

    let discovered = discover_tracked_files(working_dir)?;
    let total_processable = discovered.processable_count();
    let repo_root = discovered.repo_root;
    let tracked_files = discovered.files;
    let symlink_count = discovered.symlink_count;
    let inaccessible_files = discovered.inaccessible_files;

    if !log.quiet() && symlink_count > 0 {
        eprintln!(
            "Warning: Skipped {} symbolic link{} (timestamps not needed for symlinks)",
            symlink_count,
            if symlink_count == 1 { "" } else { "s" }
        );
    }

    if !inaccessible_files.is_empty() {
        if !log.quiet() {
            eprintln!(
                "Warning: Failed to access {} tracked file(s)",
                inaccessible_files.len()
            );
            for path in &inaccessible_files {
                log.verbose(
                    1,
                    format!("  Could not access tracked file: {}", path.display()),
                );
            }
            if log.level() == 0 {
                eprintln!("Run with -v for more details");
            }
        }
        return Err(HoldError::PartialFileProcessing {
            failed: inaccessible_files.len(),
            total: total_processable,
        });
    }

    let (unchanged, modified, added) =
        analyze_files(&repo_root, &tracked_files, &metadata, verbose, quiet)?;

    if !log.quiet() && log.level() > 0 {
        eprintln!(
            "Found {} unchanged, {} modified, {} added files",
            unchanged.len(),
            modified.len(),
            added.len()
        );
    }

    let unchanged_refs: Vec<&FileState> = unchanged.iter().collect();
    let modified_refs: Vec<&Path> = modified.iter().map(|p| p.as_path()).collect();
    let added_refs: Vec<&Path> = added.iter().map(|p| p.as_path()).collect();

    restore_timestamps(
        &repo_root,
        &unchanged_refs,
        &modified_refs,
        &added_refs,
        new_mtime,
    )?;

    if !log.quiet() {
        eprintln!("Timestamp restoration complete:");
        eprintln!("  Files analyzed: {}", tracked_files.len());
        eprintln!(
            "  Unchanged files (timestamps restored): {}",
            unchanged.len()
        );
        eprintln!(
            "  Modified files (new timestamp applied): {}",
            modified.len()
        );
        eprintln!("  New files (new timestamp applied): {}", added.len());
    }

    Ok(SalvageReport {
        unchanged_files: unchanged.len(),
        modified_files: modified.len(),
        added_files: added.len(),
    })
}

/// Analyze files to categorize them as unchanged, modified, or added.
fn analyze_files(
    repo_root: &Path,
    tracked_files: &[PathBuf],
    metadata: &StateMetadata,
    verbose: u8,
    quiet: bool,
) -> Result<(Vec<FileState>, Vec<PathBuf>, Vec<PathBuf>)> {
    let log = Logger::new(verbose, quiet);
    let mut unchanged = Vec::new();
    let mut modified = Vec::new();
    let mut added = Vec::new();

    let results: Vec<(PathBuf, FileCategory)> = tracked_files
        .par_iter()
        .map(|path| {
            let full_path = repo_root.join(path);
            let category = match metadata.get(path) {
                Ok(Some(metadata_state)) => match get_file_size(&full_path) {
                    Ok(size) if size != metadata_state.size => FileCategory::Modified,
                    Ok(_) => match hash_file(&full_path) {
                        Ok(hash) if hash != metadata_state.hash => FileCategory::Modified,
                        Ok(_) => FileCategory::Unchanged(metadata_state.clone()),
                        Err(_) => FileCategory::Error,
                    },
                    Err(_) => FileCategory::Error,
                },
                Ok(None) => FileCategory::Added,
                Err(_) => FileCategory::Error,
            };
            (path.clone(), category)
        })
        .collect();

    let mut errors = Vec::new();
    for (path, category) in results {
        match category {
            FileCategory::Unchanged(state) => unchanged.push(state),
            FileCategory::Modified => modified.push(path),
            FileCategory::Added => added.push(path),
            FileCategory::Error => {
                errors.push(path.clone());
                log.verbose(2, format!("Warning: Could not analyze file {path:?}"));
            }
        }
    }

    if !errors.is_empty() {
        if !log.quiet() {
            eprintln!("Warning: Failed to analyze {} file(s)", errors.len());
            if log.level() == 0 {
                eprintln!("Run with -v for more details");
            }
        }
        return Err(crate::error::HoldError::PartialFileProcessing {
            failed: errors.len(),
            total: tracked_files.len(),
        });
    }

    Ok((unchanged, modified, added))
}

enum FileCategory {
    Unchanged(FileState),
    Modified,
    Added,
    Error,
}
