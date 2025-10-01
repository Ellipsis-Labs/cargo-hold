//! Garbage collection for build artifacts and cargo cache.
//!
//! This module provides functionality to clean up old build artifacts from:
//! - Target directories (build artifacts)
//! - `~/.cargo/registry/cache` (downloaded crates)
//! - `~/.cargo/git/checkouts` (git dependencies)
//! - `~/.cargo/bin` (installed binaries)
//!
//! # Features
//!
//! - Size-based cleanup: Remove artifacts when directory exceeds size limit
//! - Age-based cleanup: Remove artifacts older than threshold
//! - Smart grouping: Removes all related artifacts together (by crate)
//! - Preservation rules: Always keeps important files and recent artifacts
//! - Parallel processing: Uses rayon for efficient directory scanning
//!
//! # Example
//!
//! ```no_run
//! use std::path::PathBuf;
//!
//! use cargo_hold::gc::Gc;
//!
//! let config = Gc::builder()
//!     .target_dir("target")
//!     .max_target_size(5 * 1024 * 1024 * 1024) // 5GB
//!     .age_threshold_days(7)
//!     .dry_run(false)
//!     .debug(false)
//!     .preserve_binary("cargo-hold")
//!     .build();
//!
//! let stats = config.perform_gc(0)?;
//! println!("Freed {} bytes", stats.bytes_freed);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rayon::prelude::*;

use crate::error::{HoldError, Result};

/// Garbage collection
#[derive(Debug)]
pub struct Gc {
    /// Target directory to clean
    target_dir: PathBuf,
    /// Maximum target directory size in bytes (if None, use age-based cleanup)
    max_target_size: Option<u64>,
    /// Dry run mode - don't actually delete anything
    dry_run: bool,
    /// Enable debug output
    debug: bool,
    /// Age threshold for cleanup (default: 7 days)
    age_threshold_days: u32,
    /// Additional binaries to preserve in ~/.cargo/bin (on top of defaults)
    preserve_binaries: Vec<String>,
    /// Timestamp of the previous build to preserve artifacts from
    previous_build_mtime_nanos: Option<u128>,
    /// Suppress informational logging when true
    quiet: bool,
}

impl Gc {
    /// Creates a new builder for [`Gc`]
    pub fn builder() -> GcBuilder {
        GcBuilder::default()
    }

    /// Get the target directory
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// Get the maximum target size
    pub fn max_target_size(&self) -> Option<u64> {
        self.max_target_size
    }

    /// Check if dry run mode is enabled
    pub fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Check if debug mode is enabled
    pub fn debug(&self) -> bool {
        self.debug
    }

    /// Get the age threshold in days
    pub fn age_threshold_days(&self) -> u32 {
        self.age_threshold_days
    }

    /// Get the list of binaries to preserve
    pub fn preserve_binaries(&self) -> &[String] {
        &self.preserve_binaries
    }

    /// Get the previous build mtime in nanoseconds
    pub fn previous_build_mtime_nanos(&self) -> Option<u128> {
        self.previous_build_mtime_nanos
    }

    /// Check if quiet mode is enabled
    pub fn quiet(&self) -> bool {
        self.quiet
    }

    /// Main entry point for garbage collection
    ///
    /// Performs comprehensive garbage collection on build artifacts using a
    /// combined size and age-based strategy:
    ///
    /// 1. **Size enforcement**: If max_target_size is specified and exceeded,
    ///    removes oldest artifacts first until the target directory is under
    ///    the limit
    /// 2. **Age cleanup**: Removes all artifacts older than age_threshold_days
    ///
    /// Both conditions are always applied together, ensuring consistent cleanup
    /// behavior. The function also cleans cargo registry cache, git checkouts,
    /// and other build directories.
    ///
    /// # Arguments
    ///
    /// * `config` - Garbage collection configuration
    /// * `verbose` - Verbosity level for output
    ///
    /// # Returns
    ///
    /// Statistics about the garbage collection operation
    pub fn perform_gc(&self, verbose: u8) -> Result<GcStats> {
        let mut stats = GcStats::default();

        if !self.quiet() && (verbose > 0 || self.debug()) {
            eprintln!("Starting garbage collection in {:?}", self.target_dir());
            eprintln!("Cleanup criteria:");
            if let Some(max_size) = self.max_target_size() {
                eprintln!("  - Target directory size: {}", format_size(max_size));
            }
            eprintln!(
                "  - Remove artifacts older than {} days",
                self.age_threshold_days()
            );
        }

        // Calculate initial size (return 0 if directory doesn't exist)
        stats.initial_size = if self.target_dir().exists() {
            calculate_directory_size(self.target_dir())?
        } else {
            0
        };

        if !self.quiet() {
            // Always provide feedback about the operation
            eprintln!("Cleanup status:");
            eprintln!("  Current size: {}", format_size(stats.initial_size));

            if let Some(max_size) = self.max_target_size() {
                eprintln!("  Target size: {}", format_size(max_size));
                if stats.initial_size > max_size {
                    eprintln!(
                        "  Need to free: {} (for size limit)",
                        format_size(stats.initial_size - max_size)
                    );
                } else {
                    eprintln!("  Already within target size");
                }
            }

            eprintln!("  Age threshold: {} days", self.age_threshold_days());
        }

        // Clean profile directories
        let profile_dirs = find_profile_directories(self.target_dir())?;
        for profile_dir in profile_dirs {
            if !self.quiet() && verbose > 0 {
                eprintln!("Cleaning profile directory: {profile_dir:?}");
            }
            let profile_stats = clean_profile_directory(&profile_dir, self, verbose, &stats)?;
            stats.bytes_freed += profile_stats.bytes_freed;
            stats.artifacts_removed += profile_stats.artifacts_removed;
            stats.crates_cleaned += profile_stats.crates_cleaned;
            stats.binaries_preserved += profile_stats.binaries_preserved;
        }

        // Clean other directories (doc, package, tmp)
        stats.bytes_freed += clean_misc_directories(self.target_dir(), self, verbose)?;

        // Clean cargo registry and downloads
        if !self.quiet() && verbose > 0 {
            eprintln!("Cleaning cargo registry...");
        }
        stats.bytes_freed += self.clean_cargo_registry(verbose)?;

        // Clean cargo binaries
        if !self.quiet() && verbose > 0 {
            eprintln!("Cleaning cargo binaries...");
        }
        stats.bytes_freed += self.clean_cargo_bin(verbose)?;

        // Calculate final size
        stats.final_size = calculate_directory_size(self.target_dir())?;

        Ok(stats)
    }

    /// Clean the cargo registry cache (~/.cargo/registry).
    ///
    /// Removes old cached crates and git checkouts based on age threshold.
    ///
    /// # Arguments
    ///
    /// * `verbose` - Verbosity level for output
    ///
    /// # Returns
    ///
    /// Number of bytes freed
    pub fn clean_cargo_registry(&self, verbose: u8) -> Result<u64> {
        let cargo_home = home::home_dir()
            .ok_or_else(|| HoldError::GcError {
                message: "Could not determine home directory".to_string(),
            })?
            .join(".cargo");

        self.clean_cargo_registry_with_home(&cargo_home, verbose)
    }

    /// Clean cargo registry with custom cargo home (for testing)
    pub fn clean_cargo_registry_with_home(&self, cargo_home: &Path, verbose: u8) -> Result<u64> {
        let mut bytes_freed = 0;

        // Remove credentials
        let credentials_file = cargo_home.join("credentials.toml");
        if credentials_file.exists() {
            if !self.quiet() && verbose > 0 {
                eprintln!("Removing cargo credentials");
            }
            let size = fs::metadata(&credentials_file)
                .map(|m| m.len())
                .unwrap_or(0);
            if !self.dry_run() {
                let _ = fs::remove_file(&credentials_file);
            }
            bytes_freed += size;
        }

        // Clean old registry cache files
        let registry_cache = cargo_home.join("registry").join("cache");
        if registry_cache.exists() {
            bytes_freed +=
                self.clean_old_files(&registry_cache, self.age_threshold_days(), verbose)?;
        }

        // Clean old git checkouts
        let git_checkouts = cargo_home.join("git").join("checkouts");
        if git_checkouts.exists() {
            bytes_freed += self.clean_old_directories(&git_checkouts, 30, verbose)?;
        }

        // Clean old git db entries
        let git_db = cargo_home.join("git").join("db");
        if git_db.exists() {
            bytes_freed += self.clean_old_directories(&git_db, 30, verbose)?; // 30 days for git db
        }

        // Clean old registry sources
        let registry_src = cargo_home.join("registry").join("src");
        if registry_src.exists() {
            bytes_freed += self.clean_old_directories(&registry_src, 30, verbose)?;
            // 30 days for sources
        }

        Ok(bytes_freed)
    }

    /// Clean old binaries from ~/.cargo/bin.
    ///
    /// Removes binaries older than 30 days, except for preserved binaries
    /// and those in the default preservation list.
    ///
    /// # Arguments
    ///
    /// * `verbose` - Verbosity level for output
    ///
    /// # Returns
    ///
    /// Number of bytes freed
    fn clean_cargo_bin(&self, verbose: u8) -> Result<u64> {
        let cargo_home = home::home_dir()
            .ok_or_else(|| HoldError::GcError {
                message: "Could not determine home directory".to_string(),
            })?
            .join(".cargo");

        self.clean_cargo_bin_with_home(&cargo_home, verbose)
    }

    /// Clean cargo bin directory with custom cargo home.
    ///
    /// This variant allows specifying a custom cargo home directory,
    /// which is primarily used for testing.
    ///
    /// # Arguments
    ///
    /// * `cargo_home` - The cargo home directory (typically ~/.cargo)
    /// * `verbose` - Verbosity level for output
    ///
    /// # Returns
    ///
    /// Number of bytes freed
    pub fn clean_cargo_bin_with_home(&self, cargo_home: &Path, verbose: u8) -> Result<u64> {
        let cargo_bin = cargo_home.join("bin");

        if !cargo_bin.exists() {
            return Ok(0);
        }

        if !self.quiet() && verbose > 0 {
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

        let cutoff = SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(30 * 24 * 60 * 60)) // 30 days
            .unwrap_or(SystemTime::UNIX_EPOCH);

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
                        || self
                            .preserve_binaries()
                            .iter()
                            .any(|pattern| name.starts_with(pattern));

                    if !should_keep
                        && let Ok(metadata) = fs::metadata(path)
                        && let Ok(modified) = metadata.modified()
                        && modified < cutoff
                    {
                        let size = metadata.len();
                        if !self.quiet() && verbose > 1 {
                            eprintln!("  Removing old cargo binary: {name} (older than 30 days)");
                        }
                        if !self.dry_run() {
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
    fn clean_old_files(&self, dir: &Path, age_threshold_days: u32, verbose: u8) -> Result<u64> {
        let cutoff = SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(
                age_threshold_days as u64 * 24 * 60 * 60,
            ))
            .unwrap_or(SystemTime::UNIX_EPOCH);

        if !self.quiet() && verbose > 1 {
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
        let bytes_freed: u64 = files_to_check
            .par_iter()
            .map(|path| {
                if let Ok(metadata) = fs::metadata(path)
                    && let Ok(modified) = metadata.modified()
                    && modified < cutoff
                {
                    let size = metadata.len();
                    if !self.dry_run() {
                        let _ = fs::remove_file(path);
                    }
                    return size;
                }
                0
            })
            .sum();

        Ok(bytes_freed)
    }

    /// Clean old directories
    fn clean_old_directories(
        &self,
        dir: &Path,
        age_threshold_days: u32,
        verbose: u8,
    ) -> Result<u64> {
        let cutoff = SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(
                age_threshold_days as u64 * 24 * 60 * 60,
            ))
            .unwrap_or(SystemTime::UNIX_EPOCH);

        if !self.quiet() && verbose > 1 {
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
        let bytes_freed: u64 = entries
            .par_iter()
            .map(|path| {
                if let Ok(metadata) = fs::metadata(path)
                    && let Ok(modified) = metadata.modified()
                    && modified < cutoff
                    && let Ok(size) = calculate_directory_size(path)
                {
                    if !self.dry_run() {
                        let _ = fs::remove_dir_all(path);
                    }
                    return size;
                }
                0
            })
            .sum();

        Ok(bytes_freed)
    }
}

impl Default for Gc {
    fn default() -> Self {
        Self {
            target_dir: PathBuf::from("target"),
            max_target_size: None,
            dry_run: false,
            debug: false,
            age_threshold_days: 7,
            preserve_binaries: Vec::new(),
            previous_build_mtime_nanos: None,
            quiet: false,
        }
    }
}

/// Builder for [`Gc`]
#[derive(Debug, Default)]
pub struct GcBuilder {
    target_dir: Option<PathBuf>,
    max_target_size: Option<u64>,
    dry_run: bool,
    debug: bool,
    age_threshold_days: Option<u32>,
    preserve_binaries: Vec<String>,
    previous_build_mtime_nanos: Option<u128>,
    quiet: bool,
}

impl GcBuilder {
    /// Set the target directory
    pub fn target_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.target_dir = Some(dir.into());
        self
    }

    /// Set the maximum target size
    pub fn max_target_size(mut self, size: u64) -> Self {
        self.max_target_size = Some(size);
        self
    }

    /// Enable dry run mode
    pub fn dry_run(mut self, enabled: bool) -> Self {
        self.dry_run = enabled;
        self
    }

    /// Enable debug mode
    pub fn debug(mut self, enabled: bool) -> Self {
        self.debug = enabled;
        self
    }

    /// Set the age threshold in days
    pub fn age_threshold_days(mut self, days: u32) -> Self {
        self.age_threshold_days = Some(days);
        self
    }

    /// Set the list of binaries to preserve
    pub fn preserve_binaries(mut self, binaries: Vec<String>) -> Self {
        self.preserve_binaries = binaries;
        self
    }

    /// Add a single binary to preserve
    pub fn preserve_binary(mut self, binary: impl Into<String>) -> Self {
        self.preserve_binaries.push(binary.into());
        self
    }

    /// Set the previous build mtime in nanoseconds
    pub fn previous_build_mtime_nanos(mut self, nanos: u128) -> Self {
        self.previous_build_mtime_nanos = Some(nanos);
        self
    }

    /// Enable or disable quiet mode
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    /// Build the [`Gc`]
    pub fn build(self) -> Gc {
        Gc {
            target_dir: self.target_dir.unwrap_or_else(|| PathBuf::from("target")),
            max_target_size: self.max_target_size,
            dry_run: self.dry_run,
            debug: self.debug,
            age_threshold_days: self.age_threshold_days.unwrap_or(7),
            preserve_binaries: self.preserve_binaries,
            previous_build_mtime_nanos: self.previous_build_mtime_nanos,
            quiet: self.quiet,
        }
    }
}

/// Statistics about the garbage collection operation
#[derive(Debug, Default)]
pub struct GcStats {
    /// Total bytes freed
    pub bytes_freed: u64,
    /// Number of artifacts removed
    pub artifacts_removed: usize,
    /// Number of crates cleaned
    pub crates_cleaned: usize,
    /// Initial target directory size
    pub initial_size: u64,
    /// Final target directory size
    pub final_size: u64,
    /// Number of binaries preserved
    pub binaries_preserved: usize,
}

/// Information about a single artifact
#[derive(Debug, Clone)]
pub struct ArtifactInfo {
    pub path: PathBuf,
    pub size: u64,
    pub _modified: SystemTime,
}

/// A crate artifact group (all related files for a single crate)
#[derive(Debug)]
pub struct CrateArtifact {
    pub name: String,
    pub hash: String,
    pub artifacts: Vec<ArtifactInfo>,
    pub total_size: u64,
    pub newest_mtime: SystemTime,
}

/// Parse a size string like "5G", "500M", "1024K" into bytes
pub fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();

    // Try to parse as raw number first
    if let Ok(bytes) = s.parse::<u64>() {
        return Ok(bytes);
    }

    // Otherwise parse with suffix
    let (num_part, suffix) = split_number_suffix(s)?;
    let multiplier = match suffix.to_uppercase().as_str() {
        "B" | "" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        "T" | "TB" | "TIB" => 1024_u64.pow(4),
        _ => {
            return Err(HoldError::InvalidMetadataSize {
                value: s.to_string(),
                message: format!("Unknown size suffix: {suffix}"),
            });
        }
    };

    let base: f64 = num_part
        .parse()
        .map_err(|_| HoldError::InvalidMetadataSize {
            value: s.to_string(),
            message: "Invalid number format".to_string(),
        })?;

    Ok((base * multiplier as f64) as u64)
}

/// Split a size string into number and suffix parts
fn split_number_suffix(s: &str) -> Result<(&str, &str)> {
    let mut split_pos = s.len();
    for (i, ch) in s.char_indices() {
        if ch.is_alphabetic() {
            split_pos = i;
            break;
        }
    }

    let (num, suffix) = s.split_at(split_pos);
    if num.is_empty() {
        return Err(HoldError::InvalidMetadataSize {
            value: s.to_string(),
            message: "No number found".to_string(),
        });
    }

    Ok((num, suffix))
}

/// Format size in human-readable format
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}

/// Find all profile directories in the target directory
fn find_profile_directories(target_dir: &Path) -> Result<Vec<PathBuf>> {
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
fn clean_profile_directory(
    profile_dir: &Path,
    config: &Gc,
    verbose: u8,
    global_stats: &GcStats,
) -> Result<GcStats> {
    let mut stats = GcStats::default();

    // First, preserve binaries
    let binaries = preserve_binaries(profile_dir, verbose, config.quiet())?;
    stats.binaries_preserved = binaries.len();

    // Remove incremental compilation data
    let incremental_dir = profile_dir.join("incremental");
    if incremental_dir.exists() {
        if !config.quiet() && verbose > 0 {
            eprintln!("  Removing incremental compilation data");
        }
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

    if !config.quiet() && verbose > 1 {
        eprintln!("  Found {} crate artifacts", crate_artifacts.len());
    }

    // Determine which crates to remove using combined logic
    // Calculate the current total size (initial - already freed globally)
    let current_total_size = global_stats
        .initial_size
        .saturating_sub(global_stats.bytes_freed + stats.bytes_freed);
    if !config.quiet() && (verbose > 1 || config.debug()) {
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

    if !config.quiet() && (verbose > 1 || config.debug()) {
        eprintln!("  Selected {} crates for removal", to_remove.len());
    }

    // Remove selected crates
    for crate_artifact in to_remove {
        if !config.quiet() && verbose > 1 {
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
                        if !quiet && verbose > 1 {
                            eprintln!("  Preserving binary: {:?}", path.file_name().unwrap());
                        }
                        binaries.push(path);
                    }
                }
            }

            #[cfg(not(unix))]
            {
                // On Windows, check for .exe extension
                if path.extension().map_or(false, |ext| ext == "exe") {
                    if !quiet && verbose > 1 {
                        eprintln!("  Preserving binary: {:?}", path.file_name().unwrap());
                    }
                    binaries.push(path);
                }
            }
        }
    }

    Ok(binaries)
}

/// Collect all crate artifacts from a profile directory
fn collect_crate_artifacts(profile_dir: &Path) -> Result<Vec<CrateArtifact>> {
    let fingerprint_dir = profile_dir.join(".fingerprint");
    if !fingerprint_dir.exists() {
        return Ok(Vec::new());
    }

    let mut crate_map: HashMap<(String, String), CrateArtifact> = HashMap::new();

    // Scan fingerprint directory to identify crates
    let entries = fs::read_dir(&fingerprint_dir).map_err(|source| HoldError::IoError {
        path: fingerprint_dir.clone(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| HoldError::IoError {
            path: fingerprint_dir.clone(),
            source,
        })?;
        let path = entry.path();

        if path.is_dir()
            && let Some((name, hash)) = parse_crate_artifact_name(&path)
        {
            let key = (name.clone(), hash.clone());
            let crate_artifact = crate_map.entry(key).or_insert_with(|| CrateArtifact {
                name,
                hash,
                artifacts: Vec::new(),
                total_size: 0,
                newest_mtime: SystemTime::UNIX_EPOCH,
            });

            // Add the fingerprint directory itself as an artifact
            add_artifact_file(&path, crate_artifact)?;
        }
    }

    // Now find related artifacts in deps and build directories
    for (subdir, _patterns) in &[("deps", vec!["*"]), ("build", vec!["*"])] {
        let dir = profile_dir.join(subdir);
        if !dir.exists() {
            continue;
        }

        let entries = fs::read_dir(&dir).map_err(|source| HoldError::IoError {
            path: dir.clone(),
            source,
        })?;

        for entry in entries {
            let entry = entry.map_err(|source| HoldError::IoError {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();

            // Try to match this file to a crate
            if let Some((name, hash)) = parse_crate_artifact_name(&path) {
                let key = (name.clone(), hash.clone());
                if let Some(crate_artifact) = crate_map.get_mut(&key) {
                    add_artifact_file(&path, crate_artifact)?;
                } else {
                    // This file doesn't have a corresponding fingerprint entry
                    // Create a new crate artifact for orphaned files
                    let mut artifact = CrateArtifact {
                        name: name.clone(),
                        hash: hash.clone(),
                        artifacts: Vec::new(),
                        total_size: 0,
                        newest_mtime: SystemTime::UNIX_EPOCH,
                    };
                    add_artifact_file(&path, &mut artifact)?;
                    crate_map.insert(key, artifact);
                }
            }
        }
    }

    Ok(crate_map.into_values().collect())
}

/// Parse a crate artifact filename to extract name and hash
pub fn parse_crate_artifact_name(path: &Path) -> Option<(String, String)> {
    let filename = path.file_name()?.to_str()?;

    // Find the last hyphen followed by a hash (16 hex chars)
    let re = regex::Regex::new(r"^(.+)-([0-9a-f]{16})(?:\.|$)").ok()?;
    let captures = re.captures(filename)?;

    Some((captures[1].to_string(), captures[2].to_string()))
}

/// Add artifact files to a crate artifact
fn add_artifact_files(path: &Path, crate_artifact: &mut CrateArtifact) -> Result<()> {
    if path.is_file() {
        add_artifact_file(path, crate_artifact)?;
    } else if path.is_dir() {
        let entries = fs::read_dir(path).map_err(|source| HoldError::IoError {
            path: path.to_path_buf(),
            source,
        })?;

        for entry in entries {
            let entry = entry.map_err(|source| HoldError::IoError {
                path: path.to_path_buf(),
                source,
            })?;
            add_artifact_files(&entry.path(), crate_artifact)?;
        }
    }

    Ok(())
}

/// Add a single artifact file to a crate artifact
fn add_artifact_file(path: &Path, crate_artifact: &mut CrateArtifact) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|source| HoldError::IoError {
        path: path.to_path_buf(),
        source,
    })?;

    // If it's a directory, add all its contents but not the directory itself
    if metadata.is_dir() {
        add_artifact_files(path, crate_artifact)?;
        // Also add the directory itself as an artifact to ensure it gets removed
        let artifact_info = ArtifactInfo {
            path: path.to_path_buf(),
            size: 0,                           // Directories don't have meaningful size
            _modified: SystemTime::UNIX_EPOCH, // Don't use directory mtime for age calculation
        };
        crate_artifact.artifacts.push(artifact_info);
    } else {
        // For files, track their modification time
        let modified = metadata.modified().map_err(|source| HoldError::IoError {
            path: path.to_path_buf(),
            source,
        })?;

        let artifact_info = ArtifactInfo {
            path: path.to_path_buf(),
            size: metadata.len(),
            _modified: modified,
        };

        crate_artifact.total_size += artifact_info.size;
        if modified > crate_artifact.newest_mtime {
            crate_artifact.newest_mtime = modified;
        }

        crate_artifact.artifacts.push(artifact_info);
    }

    Ok(())
}

/// Select artifacts to remove based on both size and age constraints
///
/// This function implements a two-phase cleanup strategy:
/// 1. **Size enforcement**: If a size limit is specified and exceeded, removes
///    oldest artifacts first until the target directory is under the limit
/// 2. **Age cleanup**: After size compliance, removes any remaining artifacts
///    older than the specified age threshold
///
/// Both phases are always executed, ensuring consistent and predictable cleanup
/// behavior.
///
/// # Arguments
///
/// * `crate_artifacts` - List of crate artifacts to consider for removal
/// * `current_size` - Current total size of all artifacts in bytes
/// * `max_size` - Optional maximum size limit in bytes
/// * `age_threshold_days` - Age threshold in days (artifacts older than this
///   are removed)
/// * `previous_build_mtime_nanos` - Optional timestamp of the previous build to
///   preserve
/// * `verbose` - Verbosity level for debug output
///
/// # Returns
///
/// A vector of references to artifacts that should be removed
pub fn select_artifacts_for_removal(
    crate_artifacts: &[CrateArtifact],
    current_size: u64,
    max_size: Option<u64>,
    age_threshold_days: u32,
    previous_build_mtime_nanos: Option<u128>,
    verbose: u8,
    quiet: bool,
) -> Vec<&CrateArtifact> {
    let mut to_remove = Vec::new();
    let mut remaining_artifacts: Vec<&CrateArtifact> = crate_artifacts.iter().collect();

    // First, filter out artifacts from the previous build if we have that timestamp
    if let Some(previous_mtime_nanos) = previous_build_mtime_nanos {
        // Convert to SystemTime for comparison
        let previous_mtime =
            SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(previous_mtime_nanos as u64);

        // Add a small buffer (1 second) to account for timestamp precision/drift
        let buffer = std::time::Duration::from_secs(1);
        let cutoff_time = previous_mtime
            .checked_sub(buffer)
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let (preserved, eligible): (Vec<_>, Vec<_>) = remaining_artifacts
            .into_iter()
            .partition(|artifact| artifact.newest_mtime >= cutoff_time);

        if !quiet && !preserved.is_empty() {
            let preserved_size: u64 = preserved.iter().map(|a| a.total_size).sum();
            eprintln!(
                "  Preserving {} artifacts ({}) from previous build",
                preserved.len(),
                format_size(preserved_size)
            );
            if verbose > 1 {
                for artifact in &preserved {
                    eprintln!("    Preserving: {}-{}", artifact.name, artifact.hash);
                }
            }
        }

        remaining_artifacts = eligible;
    }

    // Step 1: Apply size-based cleanup if needed
    if let Some(max_size) = max_size {
        if !quiet {
            eprintln!(
                "  Size-based cleanup: current={}, max={}",
                format_size(current_size),
                format_size(max_size)
            );
        }

        if current_size > max_size {
            let needed = current_size - max_size;
            if !quiet {
                eprintln!("  Need to free: {}", format_size(needed));
            }

            // Sort by age (oldest first)
            remaining_artifacts.sort_by_key(|a| a.newest_mtime);

            let mut freed = 0u64;
            let mut kept_artifacts = Vec::new();

            for artifact in remaining_artifacts {
                if freed < needed {
                    to_remove.push(artifact);
                    freed += artifact.total_size;
                } else {
                    kept_artifacts.push(artifact);
                }
            }

            remaining_artifacts = kept_artifacts;

            if !quiet {
                eprintln!(
                    "  Size cleanup will remove {} crates, freeing {}",
                    to_remove.len(),
                    format_size(freed)
                );
            }
        } else if !quiet {
            eprintln!("  Already within target size");
        }
    }

    // Step 2: Apply age-based cleanup on remaining artifacts
    if !quiet {
        eprintln!("  Age-based cleanup: removing artifacts older than {age_threshold_days} days");
    }

    let cutoff = SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(
            age_threshold_days as u64 * 24 * 60 * 60,
        ))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let now = SystemTime::now();
    let mut age_removed_count = 0;
    let mut age_removed_size = 0u64;

    for artifact in remaining_artifacts {
        let age_days = now
            .duration_since(artifact.newest_mtime)
            .map(|d| d.as_secs() / (24 * 60 * 60))
            .unwrap_or(0);

        if artifact.newest_mtime < cutoff {
            if !quiet && verbose > 1 {
                eprintln!(
                    "    Removing old crate {}: age={} days",
                    artifact.name, age_days
                );
            }
            age_removed_count += 1;
            age_removed_size += artifact.total_size;
            to_remove.push(artifact);
        }
    }

    if !quiet {
        eprintln!(
            "  Age cleanup will remove {} additional crates, freeing {}",
            age_removed_count,
            format_size(age_removed_size)
        );
    }

    to_remove
}

/// Remove all artifacts for a crate
fn remove_crate_artifacts(crate_artifact: &CrateArtifact) -> Result<()> {
    for artifact in &crate_artifact.artifacts {
        if artifact.path.exists() {
            if artifact.path.is_dir() {
                fs::remove_dir_all(&artifact.path).map_err(|source| HoldError::IoError {
                    path: artifact.path.clone(),
                    source,
                })?;
            } else {
                fs::remove_file(&artifact.path).map_err(|source| HoldError::IoError {
                    path: artifact.path.clone(),
                    source,
                })?;
            }
        }
    }

    Ok(())
}

/// Clean miscellaneous directories (doc, package, tmp)
fn clean_misc_directories(target_dir: &Path, config: &Gc, verbose: u8) -> Result<u64> {
    let mut bytes_freed = 0;

    for dir_name in &["doc", "package", "tmp"] {
        let dir = target_dir.join(dir_name);
        if dir.exists() {
            if verbose > 0 && !config.quiet() {
                eprintln!("Removing directory: {}", dir.display());
            }

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
fn calculate_directory_size(path: &Path) -> Result<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("100").unwrap(), 100);
        assert_eq!(parse_size("100B").unwrap(), 100);
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("1KiB").unwrap(), 1024);
        assert_eq!(parse_size("2M").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("2MB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("2MiB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("3G").unwrap(), 3 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("3GB").unwrap(), 3 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("3GiB").unwrap(), 3 * 1024 * 1024 * 1024);
        assert_eq!(
            parse_size("1.5G").unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );

        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("100X").is_err());
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(100), "100 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(1024 * 1024), "1.0 MiB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(format_size(1024_u64.pow(4)), "1.0 TiB");
    }

    #[test]
    fn test_parse_crate_artifact_name() {
        let path = Path::new("libfoo-123456789abcdef0");
        let (name, hash) = parse_crate_artifact_name(path).unwrap();
        assert_eq!(name, "libfoo");
        assert_eq!(hash, "123456789abcdef0");

        let path = Path::new("serde-1.0.136-78d1b3f8c7b8e0a2");
        let (name, hash) = parse_crate_artifact_name(path).unwrap();
        assert_eq!(name, "serde-1.0.136");
        assert_eq!(hash, "78d1b3f8c7b8e0a2");

        let path = Path::new("foo-bar-baz-0123456789abcdef.d");
        let (name, hash) = parse_crate_artifact_name(path).unwrap();
        assert_eq!(name, "foo-bar-baz");
        assert_eq!(hash, "0123456789abcdef");

        // Invalid cases
        assert!(parse_crate_artifact_name(Path::new("foo")).is_none());
        assert!(parse_crate_artifact_name(Path::new("foo-123")).is_none());
        assert!(parse_crate_artifact_name(Path::new("foo-gggggggggggggggg")).is_none());
    }

    #[test]
    fn test_select_artifacts_with_previous_build_timestamp() {
        use std::time::{Duration, SystemTime};

        let now = SystemTime::now();
        let five_minutes_ago = now - Duration::from_secs(5 * 60);
        let ten_minutes_ago = now - Duration::from_secs(10 * 60);
        let one_hour_ago = now - Duration::from_secs(60 * 60);
        let two_days_ago = now - Duration::from_secs(2 * 24 * 60 * 60);

        // Create test artifacts
        let artifacts = vec![
            CrateArtifact {
                name: "recent-crate".to_string(),
                hash: "0000000000000001".to_string(),
                artifacts: vec![],
                total_size: 1024 * 1024, // 1MB
                newest_mtime: five_minutes_ago,
            },
            CrateArtifact {
                name: "previous-build-crate".to_string(),
                hash: "0000000000000002".to_string(),
                artifacts: vec![],
                total_size: 2 * 1024 * 1024, // 2MB
                newest_mtime: ten_minutes_ago,
            },
            CrateArtifact {
                name: "old-crate".to_string(),
                hash: "0000000000000003".to_string(),
                artifacts: vec![],
                total_size: 3 * 1024 * 1024, // 3MB
                newest_mtime: one_hour_ago,
            },
            CrateArtifact {
                name: "very-old-crate".to_string(),
                hash: "0000000000000004".to_string(),
                artifacts: vec![],
                total_size: 4 * 1024 * 1024, // 4MB
                newest_mtime: two_days_ago,
            },
        ];

        // Convert ten_minutes_ago to nanos for previous build timestamp
        let previous_build_nanos = ten_minutes_ago
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        // Test 1: With previous build timestamp, recent artifacts should be preserved
        let to_remove = select_artifacts_for_removal(
            &artifacts,
            10 * 1024 * 1024,      // 10MB total
            Some(5 * 1024 * 1024), // 5MB max
            1,                     // 1 day age threshold
            Some(previous_build_nanos),
            0, // verbose
            false,
        );

        // Should preserve artifacts from ten_minutes_ago and five_minutes_ago
        // Should remove very-old-crate (age) and old-crate (size)
        assert_eq!(to_remove.len(), 2);
        assert!(to_remove.iter().any(|a| a.name == "very-old-crate"));
        assert!(to_remove.iter().any(|a| a.name == "old-crate"));

        // Test 2: Without previous build timestamp, all old artifacts can be removed
        let to_remove_no_preserve = select_artifacts_for_removal(
            &artifacts,
            10 * 1024 * 1024,      // 10MB total
            Some(5 * 1024 * 1024), // 5MB max
            1,                     // 1 day age threshold
            None,                  // No previous build timestamp
            0,                     // verbose
            false,
        );

        // Should remove very-old-crate (age) and others for size
        assert!(to_remove_no_preserve.len() >= 2);
        assert!(
            to_remove_no_preserve
                .iter()
                .any(|a| a.name == "very-old-crate")
        );
    }
}
