use std::path::{Path, PathBuf};

use super::cargo;
use super::cleanup::{
    calculate_directory_size, clean_misc_directories, clean_profile_directory,
    find_profile_directories,
};
use super::size::format_size;
use crate::error::{HoldError, Result};
use crate::logging::Logger;

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
        let log = Logger::new(verbose, self.quiet());

        if !log.quiet() && (log.level() > 0 || self.debug()) {
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

        if !log.quiet() {
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
            log.verbose(1, format!("Cleaning profile directory: {profile_dir:?}"));
            let profile_stats = clean_profile_directory(&profile_dir, self, verbose, &stats)?;
            stats.bytes_freed += profile_stats.bytes_freed;
            stats.artifacts_removed += profile_stats.artifacts_removed;
            stats.crates_cleaned += profile_stats.crates_cleaned;
            stats.binaries_preserved += profile_stats.binaries_preserved;
        }

        // Clean other directories (doc, package, tmp)
        stats.bytes_freed += clean_misc_directories(self.target_dir(), self, verbose)?;

        // Clean cargo registry and downloads
        log.verbose(1, "Cleaning cargo registry...");
        stats.bytes_freed += self.clean_cargo_registry(verbose)?;

        // Clean cargo binaries
        log.verbose(1, "Cleaning cargo binaries...");
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
        let cargo_home = self.cargo_home()?;

        self.clean_cargo_registry_with_home(&cargo_home, verbose)
    }

    /// Clean cargo registry with custom cargo home (for testing)
    pub fn clean_cargo_registry_with_home(&self, cargo_home: &Path, verbose: u8) -> Result<u64> {
        cargo::clean_cargo_registry_with_home(self, cargo_home, verbose)
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
        let cargo_home = self.cargo_home()?;

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
        cargo::clean_cargo_bin_with_home(self, cargo_home, verbose)
    }

    fn cargo_home(&self) -> Result<PathBuf> {
        Ok(home::home_dir()
            .ok_or_else(|| HoldError::GcError("Could not determine home directory".to_string()))?
            .join(".cargo"))
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
