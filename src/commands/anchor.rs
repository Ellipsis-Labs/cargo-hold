//! Anchor, salvage, stow, and bilge command implementations.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;

use crate::discovery::discover_tracked_files;
use crate::error::{HoldError, Result};
use crate::hashing::{get_file_size, hash_file};
use crate::metadata::{clean_metadata, load_metadata, save_metadata};
use crate::state::{FileState, StateMetadata};
use crate::timestamp::{
    generate_monotonic_timestamp, restore_timestamps, saturating_duration_from_nanos,
};

/// Executes the anchor command - the main orchestrator.
///
/// This command anchors your build state by performing the complete workflow:
/// 1. Restores timestamps from the metadata
/// 2. Scans for changes and saves the new state
///
/// This is the recommended command for CI use.
pub fn anchor(metadata_path: &Path, verbose: u8, quiet: bool, working_dir: &Path) -> Result<()> {
    if !quiet {
        eprintln!("⚓ Anchoring build state...");
    }

    salvage(metadata_path, verbose, quiet, working_dir)?;
    stow(metadata_path, verbose, quiet, working_dir)?;

    if !quiet {
        eprintln!("⚓ Build state anchored successfully");
    }

    Ok(())
}

/// Executes the salvage command.
///
/// Restores timestamps based on metadata content, assigning monotonic
/// timestamps to new or modified files.
pub fn salvage(metadata_path: &Path, verbose: u8, quiet: bool, working_dir: &Path) -> Result<()> {
    if !quiet && verbose > 0 {
        eprintln!("Salvaging timestamps from metadata...");
    }

    let metadata = load_metadata(metadata_path)?;

    if metadata.is_empty() {
        if !quiet && verbose > 0 {
            eprintln!("Metadata is empty, nothing to restore");
        }
        return Ok(());
    }

    if !quiet && verbose > 0 {
        eprintln!("Metadata:");
        eprintln!("  Format version: {}", metadata.version);
        eprintln!("  Tracked files: {}", metadata.len());
        eprintln!("  Metadata file: {}", metadata_path.display());
        if let Ok(metadata_info) = std::fs::metadata(metadata_path) {
            eprintln!("  Metadata size: {} bytes", metadata_info.len());
        }
    }

    let new_mtime = generate_monotonic_timestamp(&metadata);

    let (repo_root, tracked_files, symlink_count) = discover_tracked_files(working_dir)?;

    if !quiet && symlink_count > 0 {
        eprintln!(
            "Warning: Skipped {} symbolic link{} (timestamps not needed for symlinks)",
            symlink_count,
            if symlink_count == 1 { "" } else { "s" }
        );
    }

    let (unchanged, modified, added) =
        analyze_files(&repo_root, &tracked_files, &metadata, verbose, quiet)?;

    if !quiet && verbose > 0 {
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

    if !quiet {
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

    Ok(())
}

/// Executes the stow command.
///
/// Scans all Git-tracked files, hashes them, and persists the state.
pub fn stow(metadata_path: &Path, verbose: u8, quiet: bool, working_dir: &Path) -> Result<()> {
    if !quiet && verbose > 0 {
        eprintln!("Stowing files in cargo hold...");
    }

    let (repo_root, tracked_files, symlink_count) = discover_tracked_files(working_dir)?;

    if !quiet && verbose > 0 {
        eprintln!("Found {} tracked files", tracked_files.len());
    }

    if !quiet && symlink_count > 0 {
        eprintln!(
            "Note: Skipped {} symbolic link{} (not stored in metadata)",
            symlink_count,
            if symlink_count == 1 { "" } else { "s" }
        );
    }

    let file_states: Vec<Result<FileState>> = tracked_files
        .par_iter()
        .map(|path| {
            let full_path = repo_root.join(path);
            let size = get_file_size(&full_path)?;
            let hash = hash_file(&full_path)?;
            let metadata = std::fs::symlink_metadata(&full_path).map_err(|source| {
                crate::error::HoldError::IoError {
                    path: path.clone(),
                    source,
                }
            })?;

            let mtime = metadata
                .modified()
                .map_err(|source| crate::error::HoldError::IoError {
                    path: path.clone(),
                    source,
                })?;

            Ok(FileState {
                path: path.clone(),
                size,
                hash,
                mtime_nanos: mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| crate::error::HoldError::IoError {
                        path: path.clone(),
                        source: std::io::Error::other("System time is before UNIX epoch"),
                    })?
                    .as_nanos(),
            })
        })
        .collect();

    let mut new_metadata = StateMetadata::new();
    let mut errors = 0;
    for result in file_states {
        match result {
            Ok(state) => {
                if let Err(e) = new_metadata.upsert(state) {
                    errors += 1;
                    if !quiet {
                        eprintln!("Warning: Failed to add file to metadata: {e:?}");
                    }
                }
            }
            Err(e) => {
                errors += 1;
                if !quiet {
                    eprintln!("Warning: Failed to analyze file: {e:?}");
                }
            }
        }
    }

    if errors > 0 && !quiet {
        eprintln!("Warning: Failed to analyze {errors} file(s)");
        if verbose == 0 {
            eprintln!("Run with -v for more details");
        }
    }

    let existing_metadata = match load_metadata(metadata_path) {
        Ok(metadata) => Some(metadata),
        Err(HoldError::DeserializationError { .. }) => None,
        Err(err) => return Err(err),
    };

    let existing_preservation = existing_metadata.as_ref().and_then(|existing| {
        existing
            .last_gc_mtime_nanos
            .or_else(|| existing.max_mtime_nanos())
    });

    let new_max_mtime = new_metadata.max_mtime_nanos();

    let preservation_nanos = match (existing_preservation, new_max_mtime) {
        (Some(existing), Some(new_max)) => existing.max(new_max),
        (Some(existing), None) => existing,
        (None, Some(new_max)) => new_max,
        (None, None) => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos(),
    };

    new_metadata.last_gc_mtime_nanos = Some(preservation_nanos);

    if !quiet && verbose > 0 {
        let (preserved_duration, saturated) = saturating_duration_from_nanos(preservation_nanos);
        if saturated {
            eprintln!(
                "Warning: preservation timestamp ({preservation_nanos}) exceeds representable \
                 range; clamping to ~year 2554.",
            );
        }
        let preserved_time = UNIX_EPOCH + preserved_duration;
        let elapsed = SystemTime::now()
            .duration_since(preserved_time)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        eprintln!("Preserving build timestamp for GC: {preservation_nanos} nanos ({elapsed}s ago)",);
    }

    if let Some(parent) = metadata_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| crate::error::HoldError::IoError {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    save_metadata(&new_metadata, metadata_path)?;

    if !quiet {
        eprintln!("File scan complete:");
        eprintln!("  Files tracked: {}", tracked_files.len());
        eprintln!("  Metadata entries: {}", new_metadata.len());
        if errors > 0 {
            eprintln!("  Files skipped: {errors} (errors)");
        }
        eprintln!("  Metadata saved to: {}", metadata_path.display());

        if let Ok(metadata) = std::fs::metadata(metadata_path) {
            eprintln!("  Metadata size: {} KB", metadata.len() / 1024);
        }
    }

    Ok(())
}

/// Executes the bilge command (remove metadata file).
pub fn bilge(metadata_path: &Path, verbose: u8, quiet: bool) -> Result<()> {
    if !quiet && verbose > 0 {
        eprintln!("Bilging out metadata at {metadata_path:?}");
    }

    clean_metadata(metadata_path)?;

    if !quiet && verbose > 0 {
        eprintln!("Metadata bilged successfully");
    }

    Ok(())
}

/// Analyze files to categorize them as unchanged, modified, or added.
fn analyze_files(
    repo_root: &Path,
    tracked_files: &[PathBuf],
    metadata: &StateMetadata,
    verbose: u8,
    quiet: bool,
) -> Result<(Vec<FileState>, Vec<PathBuf>, Vec<PathBuf>)> {
    let mut unchanged = Vec::new();
    let mut modified = Vec::new();
    let mut added = Vec::new();

    let results: Vec<(PathBuf, FileCategory)> = tracked_files
        .par_iter()
        .map(|path| {
            let full_path = repo_root.join(path);
            let category = match metadata.get(path) {
                Ok(Some(metadatad_state)) => match get_file_size(&full_path) {
                    Ok(size) if size != metadatad_state.size => FileCategory::Modified,
                    Ok(_) => match hash_file(&full_path) {
                        Ok(hash) if hash != metadatad_state.hash => FileCategory::Modified,
                        Ok(_) => FileCategory::Unchanged(metadatad_state.clone()),
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
                if !quiet && verbose > 1 {
                    eprintln!("Warning: Could not analyze file {path:?}");
                }
            }
        }
    }

    if !errors.is_empty() && !quiet {
        eprintln!("Warning: Failed to analyze {} file(s)", errors.len());
        if verbose == 0 {
            eprintln!("Run with -v for more details");
        }
    }

    Ok((unchanged, modified, added))
}

enum FileCategory {
    Unchanged(FileState),
    Modified,
    Added,
    Error,
}
