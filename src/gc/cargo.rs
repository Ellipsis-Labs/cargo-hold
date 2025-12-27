use std::fs;
use std::path::Path;
use std::time::SystemTime;

use rayon::prelude::*;

use super::config::Gc;
use crate::error::{HoldError, Result};

#[derive(Debug, Default)]
pub struct CargoRegistryStats {
    pub bytes_freed: u64,
    pub files_removed: usize,
    pub dirs_removed: usize,
}

pub(crate) fn clean_cargo_registry_with_home(
    config: &Gc,
    cargo_home: &Path,
    verbose: u8,
) -> Result<CargoRegistryStats> {
    let mut stats = CargoRegistryStats::default();

    // Remove credentials
    let credentials_file = cargo_home.join("credentials.toml");
    if credentials_file.exists() {
        if !config.quiet() && verbose > 0 {
            eprintln!("Removing cargo credentials");
        }
        let size = fs::metadata(&credentials_file)
            .map(|m| m.len())
            .unwrap_or(0);
        if !config.dry_run() {
            let _ = fs::remove_file(&credentials_file);
        }
        stats.bytes_freed += size;
        stats.files_removed = stats.files_removed.saturating_add(1);
    }

    // Clean old registry cache files
    let registry_cache = cargo_home.join("registry").join("cache");
    if registry_cache.exists() {
        let cache_stats = clean_old_files(
            config,
            &registry_cache,
            config.age_threshold_days(),
            verbose,
        )?;
        stats.bytes_freed += cache_stats.bytes_freed;
        stats.files_removed += cache_stats.files_removed;
    }

    // Clean old git checkouts
    let git_checkouts = cargo_home.join("git").join("checkouts");
    if git_checkouts.exists() {
        let git_stats = clean_old_directories(config, &git_checkouts, 30, verbose)?;
        stats.bytes_freed += git_stats.bytes_freed;
        stats.dirs_removed += git_stats.dirs_removed;
    }

    // Clean old git db entries
    let git_db = cargo_home.join("git").join("db");
    if git_db.exists() {
        let git_stats = clean_old_directories(config, &git_db, 30, verbose)?;
        stats.bytes_freed += git_stats.bytes_freed;
        stats.dirs_removed += git_stats.dirs_removed;
    }

    // Clean old registry sources
    let registry_src = cargo_home.join("registry").join("src");
    if registry_src.exists() {
        let src_stats = clean_old_directories(config, &registry_src, 30, verbose)?;
        stats.bytes_freed += src_stats.bytes_freed;
        stats.dirs_removed += src_stats.dirs_removed;
        // 30 days for sources
    }

    Ok(stats)
}

pub(crate) fn clean_cargo_bin_with_home(
    config: &Gc,
    cargo_home: &Path,
    verbose: u8,
) -> Result<u64> {
    let cargo_bin = cargo_home.join("bin");

    if !cargo_bin.exists() {
        return Ok(0);
    }

    if !config.quiet() && verbose > 0 {
        eprintln!("Cleaning old cargo binaries...");
    }

    // Binaries to keep (prefix patterns)
    let keep_binaries = [
        "cargo",
        "rustc",
        "clippy",
        "sccache",
        "cargo-nextest",
        "cargo-make",
        "cargo-binstall",
        "wild",
        "rustdoc",
        "rustup",
        "rls",
        "rust-analyzer",
        "rust-gdbgui",
        "rust-lldb",
        "rustfmt",
        "rust-gdb",
        "cargo-hold", // Keep ourselves!
    ];

    let cutoff = age_cutoff(30);

    let entries: Vec<_> = fs::read_dir(&cargo_bin)
        .map_err(|source| HoldError::IoError {
            path: cargo_bin.clone(),
            source,
        })?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();

    let bytes_freed: u64 = entries
        .par_iter()
        .map(|path| {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                // Check if this binary should be kept
                let should_keep = keep_binaries.iter().any(|&prefix| name.starts_with(prefix))
                    || config
                        .preserve_binaries()
                        .iter()
                        .any(|pattern| name.starts_with(pattern));

                if !should_keep
                    && let Ok(metadata) = fs::metadata(path)
                    && let Ok(modified) = metadata.modified()
                    && modified < cutoff
                {
                    let size = metadata.len();
                    if !config.quiet() && verbose > 1 {
                        eprintln!("  Removing old cargo binary: {name} (older than 30 days)");
                    }
                    if !config.dry_run() {
                        let _ = fs::remove_file(path);
                    }
                    return size;
                }
            }
            0
        })
        .sum();

    Ok(bytes_freed)
}

/// Clean old files in a directory using walkdir and rayon
#[derive(Debug, Default)]
struct CleanupStats {
    bytes_freed: u64,
    files_removed: usize,
    dirs_removed: usize,
}

fn clean_old_files(
    config: &Gc,
    dir: &Path,
    age_threshold_days: u32,
    verbose: u8,
) -> Result<CleanupStats> {
    let cutoff = age_cutoff(age_threshold_days);

    if !config.quiet() && verbose > 1 {
        eprintln!("  Cleaning old files in {dir:?} (>{age_threshold_days} days)");
    }

    // Collect all files that need to be checked
    let files_to_check: Vec<_> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();

    // Process files in parallel using rayon
    let stats = files_to_check
        .par_iter()
        .map(|path| remove_file_if_older(config, path, cutoff))
        .reduce(CleanupStats::default, |mut acc, item| {
            acc.bytes_freed += item.bytes_freed;
            acc.files_removed += item.files_removed;
            acc
        });

    Ok(stats)
}

/// Clean old directories
fn clean_old_directories(
    config: &Gc,
    dir: &Path,
    age_threshold_days: u32,
    verbose: u8,
) -> Result<CleanupStats> {
    let cutoff = age_cutoff(age_threshold_days);

    if !config.quiet() && verbose > 1 {
        eprintln!("  Cleaning old directories in {dir:?} (>{age_threshold_days} days)");
    }

    // Collect directories to check
    let entries: Vec<_> = fs::read_dir(dir)
        .map_err(|source| HoldError::IoError {
            path: dir.to_path_buf(),
            source,
        })?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();

    // Process directories in parallel
    let stats = entries
        .par_iter()
        .map(|path| remove_dir_if_older(config, path, cutoff))
        .reduce(CleanupStats::default, |mut acc, item| {
            acc.bytes_freed += item.bytes_freed;
            acc.dirs_removed += item.dirs_removed;
            acc
        });

    Ok(stats)
}

fn age_cutoff(age_threshold_days: u32) -> SystemTime {
    SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(
            age_threshold_days as u64 * 24 * 60 * 60,
        ))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn remove_file_if_older(config: &Gc, path: &Path, cutoff: SystemTime) -> CleanupStats {
    if let Ok(metadata) = fs::metadata(path)
        && let Ok(modified) = metadata.modified()
        && modified < cutoff
    {
        let size = metadata.len();
        if !config.dry_run() {
            let _ = fs::remove_file(path);
        }
        return CleanupStats {
            bytes_freed: size,
            files_removed: 1,
            dirs_removed: 0,
        };
    }
    CleanupStats::default()
}

fn remove_dir_if_older(config: &Gc, path: &Path, cutoff: SystemTime) -> CleanupStats {
    if let Ok(metadata) = fs::metadata(path)
        && let Ok(modified) = metadata.modified()
        && modified < cutoff
        && let Ok(size) = super::cleanup::calculate_directory_size(path)
    {
        if !config.dry_run() {
            let _ = fs::remove_dir_all(path);
        }
        return CleanupStats {
            bytes_freed: size,
            files_removed: 0,
            dirs_removed: 1,
        };
    }
    CleanupStats::default()
}
