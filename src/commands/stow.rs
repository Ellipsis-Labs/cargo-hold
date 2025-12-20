//! Stow command implementation.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;

use crate::discovery::discover_tracked_files;
use crate::error::{HoldError, Result};
use crate::hashing::{get_file_mtime_nanos, get_file_size, hash_file};
use crate::logging::Logger;
use crate::metadata::{load_metadata, save_metadata};
use crate::state::{FileState, StateMetadata};
use crate::timestamp::saturating_duration_from_nanos;

/// Executes the stow command.
///
/// Scans all Git-tracked files, hashes them, and persists the state.
pub fn stow(metadata_path: &Path, verbose: u8, quiet: bool, working_dir: &Path) -> Result<()> {
    let log = Logger::new(verbose, quiet);
    log.verbose(1, "Stowing files in cargo hold...");

    let (repo_root, tracked_files, symlink_count) = discover_tracked_files(working_dir)?;

    log.verbose(1, format!("Found {} tracked files", tracked_files.len()));

    if !log.quiet() && symlink_count > 0 {
        eprintln!(
            "Note: Skipped {} symbolic link{} (not stored in metadata)",
            symlink_count,
            if symlink_count == 1 { "" } else { "s" }
        );
    }

    let file_states: Vec<Result<FileState>> = tracked_files
        .par_iter()
        .map(|path| build_file_state(&repo_root, path))
        .collect();

    let mut new_metadata = StateMetadata::new();
    let mut errors = 0;
    for result in file_states {
        match result {
            Ok(state) => {
                if let Err(e) = new_metadata.upsert(state) {
                    errors += 1;
                    if !log.quiet() {
                        eprintln!("Warning: Failed to add file to metadata: {e:?}");
                    }
                }
            }
            Err(e) => {
                errors += 1;
                if !log.quiet() {
                    eprintln!("Warning: Failed to analyze file: {e:?}");
                }
            }
        }
    }

    if errors > 0 && !log.quiet() {
        eprintln!("Warning: Failed to analyze {errors} file(s)");
        if log.level() == 0 {
            eprintln!("Run with -v for more details");
        }
    }

    let existing_metadata = match load_metadata(metadata_path) {
        Ok(metadata) => Some(metadata),
        Err(HoldError::DeserializationError { .. }) => None,
        Err(err) => return Err(err),
    };

    if let Some(existing) = existing_metadata.as_ref() {
        new_metadata.gc_metrics = existing.gc_metrics.clone();
    }

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

    if !log.quiet() && log.level() > 0 {
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

    save_metadata(&new_metadata, metadata_path)?;

    if !log.quiet() {
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

fn build_file_state(repo_root: &Path, path: &PathBuf) -> Result<FileState> {
    let full_path = repo_root.join(path);
    let size = get_file_size(&full_path)?;
    let hash = hash_file(&full_path)?;
    let mtime_nanos = get_file_mtime_nanos(&full_path)?;

    Ok(FileState {
        path: path.clone(),
        size,
        hash,
        mtime_nanos,
    })
}
