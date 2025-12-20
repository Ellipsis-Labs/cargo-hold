use std::fs;
use std::path::{Path, PathBuf};

use super::artifacts::{
    collect_crate_artifacts, remove_crate_artifacts, select_artifacts_for_removal,
};
use super::config::{Gc, GcStats};
use super::size::format_size;
use crate::error::{HoldError, Result};
use crate::logging::Logger;

/// Find all profile directories in the target directory
pub(crate) fn find_profile_directories(target_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut profile_dirs = Vec::new();

    if !target_dir.exists() {
        return Ok(profile_dirs);
    }

    // Check if target_dir itself is a profile directory
    if is_profile_directory(target_dir) {
        profile_dirs.push(target_dir.to_path_buf());
        return Ok(profile_dirs);
    }

    // Look for profile directories in subdirectories
    let entries = fs::read_dir(target_dir).map_err(|source| HoldError::IoError {
        path: target_dir.to_path_buf(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| HoldError::IoError {
            path: target_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();

        if path.is_dir() {
            // Skip special files
            if let Some(name) = path.file_name() {
                let name = name.to_string_lossy();
                if name == "CACHEDIR.TAG" || name == ".rustc_info.json" {
                    continue;
                }
            }

            if is_profile_directory(&path) {
                profile_dirs.push(path);
            } else {
                // Check subdirectories (for target triple directories)
                if let Ok(subdirs) = find_profile_directories(&path) {
                    profile_dirs.extend(subdirs);
                }
            }
        }
    }

    Ok(profile_dirs)
}

/// Check if a directory is a Cargo profile directory
fn is_profile_directory(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }

    // Check for standard Cargo build artifacts
    let artifact_dirs = ["build", "deps", ".fingerprint"];
    artifact_dirs.iter().any(|&dir| path.join(dir).exists())
}

/// Clean a single profile directory
pub(crate) fn clean_profile_directory(
    profile_dir: &Path,
    config: &Gc,
    verbose: u8,
    global_stats: &GcStats,
) -> Result<GcStats> {
    let log = Logger::new(verbose, config.quiet());
    let mut stats = GcStats::default();

    // First, preserve binaries
    let binaries = preserve_binaries(profile_dir, verbose, config.quiet())?;
    stats.binaries_preserved = binaries.len();

    // Remove incremental compilation data
    let incremental_dir = profile_dir.join("incremental");
    if incremental_dir.exists() {
        log.verbose(1, "  Removing incremental compilation data");
        let size = calculate_directory_size(&incremental_dir)?;
        if !config.dry_run() {
            fs::remove_dir_all(&incremental_dir).map_err(|source| HoldError::IoError {
                path: incremental_dir,
                source,
            })?;
        }
        stats.bytes_freed += size;
    }

    // Collect and analyze crate artifacts
    let crate_artifacts = collect_crate_artifacts(profile_dir)?;

    log.verbose(
        2,
        format!("  Found {} crate artifacts", crate_artifacts.len()),
    );

    // Determine which crates to remove using combined logic
    // Calculate the current total size (initial - already freed globally)
    let current_total_size = global_stats
        .initial_size
        .saturating_sub(global_stats.bytes_freed + stats.bytes_freed);
    if !log.quiet() && (log.level() > 1 || config.debug()) {
        eprintln!(
            "  Initial: {}, Freed globally: {}, Freed locally: {}, Current total: {}",
            format_size(global_stats.initial_size),
            format_size(global_stats.bytes_freed),
            format_size(stats.bytes_freed),
            format_size(current_total_size)
        );
    }

    let to_remove = select_artifacts_for_removal(
        &crate_artifacts,
        current_total_size,
        config.max_target_size(),
        config.age_threshold_days(),
        config.previous_build_mtime_nanos(),
        verbose,
        config.quiet(),
    );

    if !log.quiet() && (log.level() > 1 || config.debug()) {
        eprintln!("  Selected {} crates for removal", to_remove.len());
    }

    // Remove selected crates
    for crate_artifact in to_remove {
        if !log.quiet() && log.level() > 1 {
            eprintln!(
                "  Removing {}-{} ({})",
                crate_artifact.name,
                crate_artifact.hash,
                format_size(crate_artifact.total_size)
            );
        }

        if !config.dry_run() {
            remove_crate_artifacts(crate_artifact)?;
        }

        stats.bytes_freed += crate_artifact.total_size;
        stats.artifacts_removed += crate_artifact.artifacts.len();
        stats.crates_cleaned += 1;
    }

    Ok(stats)
}

/// Preserve binary files in the profile directory
fn preserve_binaries(profile_dir: &Path, verbose: u8, quiet: bool) -> Result<Vec<PathBuf>> {
    let log = Logger::new(verbose, quiet);
    let mut binaries = Vec::new();

    let entries = fs::read_dir(profile_dir).map_err(|source| HoldError::IoError {
        path: profile_dir.to_path_buf(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| HoldError::IoError {
            path: profile_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();

        if path.is_file() {
            // Check if file is executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = path.metadata() {
                    let permissions = metadata.permissions();
                    let is_executable = permissions.mode() & 0o111 != 0;
                    let has_no_extension = path.extension().is_none();

                    if is_executable && has_no_extension {
                        log.verbose(
                            2,
                            format!("  Preserving binary: {:?}", path.file_name().unwrap()),
                        );
                        binaries.push(path);
                    }
                }
            }

            #[cfg(not(unix))]
            {
                // On Windows, check for .exe extension
                if path.extension().map_or(false, |ext| ext == "exe") {
                    log.verbose(
                        2,
                        format!("  Preserving binary: {:?}", path.file_name().unwrap()),
                    );
                    binaries.push(path);
                }
            }
        }
    }

    Ok(binaries)
}

/// Clean miscellaneous directories (doc, package, tmp)
pub(crate) fn clean_misc_directories(target_dir: &Path, config: &Gc, verbose: u8) -> Result<u64> {
    let mut bytes_freed = 0;
    let log = Logger::new(verbose, config.quiet());

    for dir_name in &["doc", "package", "tmp"] {
        let dir = target_dir.join(dir_name);
        if dir.exists() {
            log.verbose(1, format!("Removing directory: {}", dir.display()));

            let size = calculate_directory_size(&dir)?;
            if !config.dry_run() {
                fs::remove_dir_all(&dir)
                    .map_err(|source| HoldError::IoError { path: dir, source })?;
            }
            bytes_freed += size;
        }
    }

    Ok(bytes_freed)
}

/// Calculate the total size of a directory
pub(crate) fn calculate_directory_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    let mut total_size = 0;

    if path.is_file() {
        let metadata = fs::metadata(path).map_err(|source| HoldError::IoError {
            path: path.to_path_buf(),
            source,
        })?;
        return Ok(metadata.len());
    }

    let entries = fs::read_dir(path).map_err(|source| HoldError::IoError {
        path: path.to_path_buf(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| HoldError::IoError {
            path: path.to_path_buf(),
            source,
        })?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            total_size += calculate_directory_size(&entry_path)?;
        } else if entry_path.is_file() {
            let metadata = fs::metadata(&entry_path).map_err(|source| HoldError::IoError {
                path: entry_path.clone(),
                source,
            })?;
            total_size += metadata.len();
        }
    }

    Ok(total_size)
}
