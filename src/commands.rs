//! Implementation of cargo-hold subcommands.
//!
//! This module contains the core logic for executing each cargo-hold command.
//! The main entry point is the [`execute`] function which dispatches to the
//! appropriate command handler.
//!
//! # Commands
//!
//! - [`anchor`]: Main CI command - combines salvage and stow operations
//! - [`salvage`]: Restores timestamps based on content changes
//! - [`stow`]: Saves current file state to metadata
//! - [`bilge`]: Removes metadata file
//! - [`Heave`]: Garbage collection for build artifacts
//! - [`Voyage`]: Combines anchor and heave operations
//!
//! # Example
//!
//! ```no_run
//! use cargo_hold::cli::Cli;
//! use cargo_hold::commands;
//!
//! // Parse CLI arguments and execute the command
//! let cli = Cli::parse_args();
//! let result = commands::execute(&cli);
//! if let Err(e) = result {
//!     eprintln!("Error: {e:?}");
//! }
//! ```

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::cli::{Cli, Commands};
use crate::discovery::discover_tracked_files;
use crate::error::{HoldError, Result};
use crate::gc::{self, Gc};
use crate::hashing::{get_file_size, hash_file};
use crate::metadata::{clean_metadata, load_metadata, save_metadata};
use crate::state::{FileState, StateMetadata};
use crate::timestamp::{generate_monotonic_timestamp, restore_timestamps};

/// Executes the anchor command - the main orchestrator.
///
/// This command anchors your build state by performing the complete workflow:
/// 1. Restores timestamps from the metadata
/// 2. Scans for changes and saves the new state
///
/// This is the recommended command for CI use.
///
/// # Arguments
///
/// * `metadata_path` - Path to the metadata file
/// * `verbose` - Verbosity level (0 = quiet, 1 = normal, 2+ = detailed)
/// * `working_dir` - Working directory to operate from
pub fn anchor(metadata_path: &Path, verbose: u8, working_dir: &Path) -> Result<()> {
    eprintln!("⚓ Anchoring build state...");

    // Execute the full workflow
    salvage(metadata_path, verbose, working_dir)?;
    stow(metadata_path, verbose, working_dir)?;

    eprintln!("⚓ Build state anchored successfully");

    Ok(())
}

/// Executes the salvage command.
///
/// This command salvages file timestamps from the metadata, restoring them
/// based on their change status. Files that haven't changed get their original
/// timestamps, while new or modified files get a new monotonic timestamp.
///
/// # Arguments
///
/// * `metadata_path` - Path to the metadata file
/// * `verbose` - Verbosity level (0 = quiet, 1 = normal, 2+ = detailed)
/// * `working_dir` - Working directory to operate from
pub fn salvage(metadata_path: &Path, verbose: u8, working_dir: &Path) -> Result<()> {
    if verbose > 0 {
        eprintln!("Salvaging timestamps from metadata metadata...");
    }

    // Metadata path should already be absolute from CLI layer
    // Load the metadata
    let metadata = load_metadata(metadata_path)?;

    if metadata.is_empty() {
        if verbose > 0 {
            eprintln!("Metadata is empty, nothing to restore");
        }
        return Ok(());
    }

    // Print metadata metadata info
    if verbose > 0 {
        eprintln!("Metadata:");
        eprintln!("  Format version: {}", metadata.version);
        eprintln!("  Tracked files: {}", metadata.len());
        eprintln!("  Metadata file: {}", metadata_path.display());
        if let Ok(metadata_info) = std::fs::metadata(metadata_path) {
            eprintln!("  Metadata size: {} bytes", metadata_info.len());
        }
    }

    // Generate monotonic timestamp
    let new_mtime = generate_monotonic_timestamp(&metadata);

    // Discover tracked files - these are already relative to repo root
    let (repo_root, tracked_files, symlink_count) = discover_tracked_files(working_dir)?;

    // Report symlinks if any were found
    if symlink_count > 0 {
        eprintln!(
            "Warning: Skipped {} symbolic link{} (timestamps not needed for symlinks)",
            symlink_count,
            if symlink_count == 1 { "" } else { "s" }
        );
    }

    // Analyze files to categorize them
    let (unchanged, modified, added) =
        analyze_files(&repo_root, &tracked_files, &metadata, verbose)?;

    if verbose > 0 {
        eprintln!(
            "Found {} unchanged, {} modified, {} added files",
            unchanged.len(),
            modified.len(),
            added.len()
        );
    }

    // Restore timestamps
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

    // Always print summary statistics (not just in verbose mode)
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

    Ok(())
}

/// Executes the stow command.
///
/// This command stows files in the cargo hold by scanning all Git-tracked
/// files, computing their hashes, and saving the complete state to the metadata
/// file.
///
/// # Arguments
///
/// * `metadata_path` - Path to the metadata file
/// * `verbose` - Verbosity level (0 = quiet, 1 = normal, 2+ = detailed)
/// * `working_dir` - Working directory to operate from
pub fn stow(metadata_path: &Path, verbose: u8, working_dir: &Path) -> Result<()> {
    if verbose > 0 {
        eprintln!("Stowing files in cargo hold...");
    }

    // Discover tracked files - these are already relative to repo root
    let (repo_root, tracked_files, symlink_count) = discover_tracked_files(working_dir)?;

    if verbose > 0 {
        eprintln!("Found {} tracked files", tracked_files.len());
    }

    // Report symlinks if any were found
    if symlink_count > 0 {
        eprintln!(
            "Note: Skipped {} symbolic link{} (not stored in metadata)",
            symlink_count,
            if symlink_count == 1 { "" } else { "s" }
        );
    }

    // Build new metadata state in parallel
    let file_states: Vec<Result<FileState>> = tracked_files
        .par_iter()
        .map(|path| {
            let full_path = repo_root.join(path);
            let size = get_file_size(&full_path)?;
            let hash = hash_file(&full_path)?;
            // Get file metadata (use symlink_metadata for defensive consistency)
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

    // Convert results and build metadata
    let mut new_metadata = StateMetadata::new();
    let mut errors = 0;
    for result in file_states {
        match result {
            Ok(state) => {
                if let Err(e) = new_metadata.upsert(state) {
                    errors += 1;
                    eprintln!("Warning: Failed to add file to metadata: {e:?}");
                }
            }
            Err(e) => {
                errors += 1;
                eprintln!("Warning: Failed to analyze file: {e:?}");
            }
        }
    }

    // Report errors if any
    if errors > 0 {
        eprintln!("Warning: Failed to analyze {errors} file(s)");
        if verbose == 0 {
            eprintln!("Run with -v for more details");
        }
    }

    // Load existing metadata to get the previous max_mtime_nanos
    let existing_metadata = load_metadata(metadata_path).ok();
    if let Some(existing) = existing_metadata {
        // Preserve the max mtime from the current build as the last_gc_mtime_nanos
        new_metadata.last_gc_mtime_nanos = existing.max_mtime_nanos();
        if verbose > 0
            && let Some(mtime) = new_metadata.last_gc_mtime_nanos
        {
            eprintln!("Preserving previous build timestamp for GC: {mtime} nanos");
        }
    }

    // Metadata path should already be absolute from CLI layer
    // Ensure the parent directory exists
    if let Some(parent) = metadata_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| crate::error::HoldError::IoError {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    save_metadata(&new_metadata, metadata_path)?;

    // Always print summary statistics
    eprintln!("File scan complete:");
    eprintln!("  Files tracked: {}", tracked_files.len());
    eprintln!("  Metadata entries: {}", new_metadata.len());
    if errors > 0 {
        eprintln!("  Files skipped: {errors} (errors)");
    }
    eprintln!("  Metadata saved to: {}", metadata_path.display());

    // Print metadata metadata file size
    if let Ok(metadata) = std::fs::metadata(metadata_path) {
        eprintln!("  Metadata size: {} KB", metadata.len() / 1024);
    }

    Ok(())
}

/// Executes the bilge command.
///
/// This command bilges out the metadata file (removes it), forcing a fresh
/// start on the next run.
///
/// # Arguments
///
/// * `metadata_path` - Path to the metadata file
/// * `verbose` - Verbosity level (0 = quiet, 1 = normal, 2+ = detailed)
pub fn bilge(metadata_path: &Path, verbose: u8) -> Result<()> {
    // No need to discover repository - metadata path is already absolute
    if verbose > 0 {
        eprintln!("Bilging out metadata metadata at {metadata_path:?}");
    }

    clean_metadata(metadata_path)?;

    if verbose > 0 {
        eprintln!("Metadata bilged successfully");
    }

    Ok(())
}

/// Heave (garbage collection)
pub struct Heave<'a> {
    target_dir: &'a Path,
    max_target_size: Option<&'a str>,
    dry_run: bool,
    debug: bool,
    preserve_cargo_binaries: &'a [String],
    age_threshold_days: u32,
    verbose: u8,
    metadata_path: Option<&'a Path>,
}

/// Builder for constructing [`Heave`] command instances.
///
/// Provides a fluent API for configuring garbage collection parameters
/// before executing the heave command.
#[derive(Default)]
pub struct HeaveBuilder<'a> {
    target_dir: Option<&'a Path>,
    max_target_size: Option<&'a str>,
    dry_run: bool,
    debug: bool,
    preserve_cargo_binaries: &'a [String],
    age_threshold_days: u32,
    verbose: u8,
    metadata_path: Option<&'a Path>,
}

impl<'a> HeaveBuilder<'a> {
    /// Create a new `HeaveBuilder` with default values.
    pub fn new() -> Self {
        Self {
            target_dir: None,
            max_target_size: None,
            dry_run: false,
            debug: false,
            preserve_cargo_binaries: &[],
            age_threshold_days: 7,
            verbose: 0,
            metadata_path: None,
        }
    }

    /// Set the target directory to clean.
    pub fn target_dir(mut self, path: &'a Path) -> Self {
        self.target_dir = Some(path);
        self
    }

    /// Set the maximum allowed size for the target directory.
    ///
    /// Size can be specified as "5G", "500M", or in bytes.
    pub fn max_target_size(mut self, size: Option<&'a str>) -> Self {
        self.max_target_size = size;
        self
    }

    /// Enable dry-run mode (show what would be deleted without actually
    /// deleting).
    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Enable debug output for garbage collection.
    pub fn debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    /// Set additional binaries to preserve in ~/.cargo/bin.
    pub fn preserve_cargo_binaries(mut self, binaries: &'a [String]) -> Self {
        self.preserve_cargo_binaries = binaries;
        self
    }

    /// Set the age threshold in days for removing artifacts (default: 7).
    pub fn age_threshold_days(mut self, days: u32) -> Self {
        self.age_threshold_days = days;
        self
    }

    /// Set the verbosity level.
    pub fn verbose(mut self, verbose: u8) -> Self {
        self.verbose = verbose;
        self
    }

    /// Set the metadata path for reading last GC timestamp.
    pub fn metadata_path(mut self, path: &'a Path) -> Self {
        self.metadata_path = Some(path);
        self
    }

    /// Build the [`Heave`] instance with the configured parameters.
    pub fn build(self) -> Heave<'a> {
        Heave {
            target_dir: self.target_dir.unwrap(),
            max_target_size: self.max_target_size,
            dry_run: self.dry_run,
            debug: self.debug,
            preserve_cargo_binaries: self.preserve_cargo_binaries,
            age_threshold_days: self.age_threshold_days,
            verbose: self.verbose,
            metadata_path: self.metadata_path,
        }
    }
}

impl<'a> Heave<'a> {
    pub fn builder<'b>() -> HeaveBuilder<'b> {
        HeaveBuilder::new()
    }

    /// Execute the heave command (garbage collection)
    pub fn heave(self) -> Result<()> {
        if self.verbose > 0 {
            eprintln!("Heave ho! Starting garbage collection...");
        }

        // Parse target size if provided
        let max_size = if let Some(size_str) = self.max_target_size {
            Some(gc::parse_size(size_str)?)
        } else {
            None
        };

        // Load metadata to get the last_gc_mtime_nanos if available
        let last_gc_mtime_nanos = if let Some(path) = self.metadata_path {
            load_metadata(path).ok().and_then(|m| m.last_gc_mtime_nanos)
        } else {
            None
        };

        if let Some(mtime) = last_gc_mtime_nanos {
            let mtime_secs = (mtime / 1_000_000_000) as u64;
            eprintln!(
                "Using previous build timestamp for artifact preservation ({}s ago)",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs().saturating_sub(mtime_secs))
                    .unwrap_or(0)
            );
        }

        // Configure GC
        let mut builder = Gc::builder()
            .target_dir(self.target_dir.to_path_buf())
            .dry_run(self.dry_run)
            .debug(self.debug || self.verbose >= 2)
            .age_threshold_days(self.age_threshold_days)
            .preserve_binaries(self.preserve_cargo_binaries.to_vec());

        if let Some(size) = max_size {
            builder = builder.max_target_size(size);
        }

        if let Some(nanos) = last_gc_mtime_nanos {
            builder = builder.previous_build_mtime_nanos(nanos);
        }

        let config = builder.build();

        // Perform garbage collection
        let stats = config.perform_gc(self.verbose)?;

        // Always report results
        eprintln!("Garbage collection complete:");
        eprintln!("  Initial size: {}", gc::format_size(stats.initial_size));
        eprintln!("  Final size: {}", gc::format_size(stats.final_size));
        eprintln!("  Space freed: {}", gc::format_size(stats.bytes_freed));
        eprintln!("  Artifacts removed: {}", stats.artifacts_removed);
        eprintln!("  Crates cleaned: {}", stats.crates_cleaned);
        eprintln!("  Binaries preserved: {}", stats.binaries_preserved);

        if self.dry_run {
            eprintln!("  (DRY RUN - no files were actually deleted)");
        }

        Ok(())
    }
}

/// Voyage command - combination of anchor and heave
pub struct Voyage<'a> {
    metadata_path: &'a Path,
    target_dir: &'a Path,
    max_target_size: Option<&'a str>,
    gc_dry_run: bool,
    gc_debug: bool,
    preserve_cargo_binaries: &'a [String],
    gc_age_threshold_days: u32,
    verbose: u8,
}

impl<'a> Voyage<'a> {
    /// Create a new builder for [`Voyage`]
    pub fn builder() -> VoyageBuilder<'a> {
        VoyageBuilder::new()
    }

    /// Execute the voyage (anchor + heave)
    pub fn run(self) -> Result<()> {
        eprintln!("🚢 Setting sail on voyage (anchor + heave)...");

        // Step 1: Run anchor
        // Need to get current directory for anchor
        let current_dir = std::env::current_dir().map_err(|source| HoldError::IoError {
            path: PathBuf::from("."),
            source,
        })?;
        anchor(self.metadata_path, self.verbose, &current_dir)?;

        eprintln!("🧹 Starting garbage collection...");

        // Step 2: Run heave
        Heave::builder()
            .target_dir(self.target_dir)
            .max_target_size(self.max_target_size)
            .dry_run(self.gc_dry_run)
            .debug(self.gc_debug)
            .preserve_cargo_binaries(self.preserve_cargo_binaries)
            .age_threshold_days(self.gc_age_threshold_days)
            .verbose(self.verbose)
            .metadata_path(self.metadata_path)
            .build()
            .heave()?;

        eprintln!("🚢 Voyage completed successfully!");

        Ok(())
    }
}

/// Builder for [`Voyage`]
pub struct VoyageBuilder<'a> {
    metadata_path: Option<&'a Path>,
    target_dir: Option<&'a Path>,
    max_target_size: Option<&'a str>,
    gc_dry_run: bool,
    gc_debug: bool,
    preserve_cargo_binaries: &'a [String],
    gc_age_threshold_days: u32,
    verbose: u8,
}

impl Default for VoyageBuilder<'_> {
    fn default() -> Self {
        Self {
            metadata_path: None,
            target_dir: None,
            max_target_size: None,
            gc_dry_run: false,
            gc_debug: false,
            preserve_cargo_binaries: &[],
            gc_age_threshold_days: 7, // Default to 7 days
            verbose: 0,
        }
    }
}

impl<'a> VoyageBuilder<'a> {
    /// Create a new `VoyageBuilder` with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the metadata path for the voyage operation.
    pub fn metadata_path(mut self, path: &'a Path) -> Self {
        self.metadata_path = Some(path);
        self
    }

    /// Set the target directory for garbage collection.
    pub fn target_dir(mut self, path: &'a Path) -> Self {
        self.target_dir = Some(path);
        self
    }

    /// Set the maximum target directory size for garbage collection.
    ///
    /// Size can be specified as "5G", "500M", or in bytes.
    pub fn max_target_size(mut self, size: Option<&'a str>) -> Self {
        self.max_target_size = size;
        self
    }

    /// Enable dry-run mode for garbage collection.
    pub fn gc_dry_run(mut self, dry_run: bool) -> Self {
        self.gc_dry_run = dry_run;
        self
    }

    /// Enable debug output for garbage collection.
    pub fn gc_debug(mut self, debug: bool) -> Self {
        self.gc_debug = debug;
        self
    }

    /// Set additional binaries to preserve during garbage collection.
    pub fn preserve_cargo_binaries(mut self, binaries: &'a [String]) -> Self {
        self.preserve_cargo_binaries = binaries;
        self
    }

    /// Set the age threshold in days for garbage collection (default: 7).
    pub fn gc_age_threshold_days(mut self, days: u32) -> Self {
        self.gc_age_threshold_days = days;
        self
    }

    /// Set the verbosity level.
    pub fn verbose(mut self, verbose: u8) -> Self {
        self.verbose = verbose;
        self
    }

    /// Build the [`Voyage`] instance with the configured parameters.
    pub fn build(self) -> Result<Voyage<'a>> {
        Ok(Voyage {
            metadata_path: self.metadata_path.ok_or_else(|| HoldError::ConfigError {
                message: "metadata_path is required".to_string(),
            })?,
            target_dir: self.target_dir.ok_or_else(|| HoldError::ConfigError {
                message: "target_dir is required".to_string(),
            })?,
            max_target_size: self.max_target_size,
            gc_dry_run: self.gc_dry_run,
            gc_debug: self.gc_debug,
            preserve_cargo_binaries: self.preserve_cargo_binaries,
            gc_age_threshold_days: self.gc_age_threshold_days,
            verbose: self.verbose,
        })
    }
}

/// Analyze files to categorize them as unchanged, modified, or added
fn analyze_files(
    repo_root: &Path,
    tracked_files: &[PathBuf],
    metadata: &StateMetadata,
    verbose: u8,
) -> Result<(Vec<FileState>, Vec<PathBuf>, Vec<PathBuf>)> {
    let mut unchanged = Vec::new();
    let mut modified = Vec::new();
    let mut added = Vec::new();

    // Process files in parallel for better performance
    let results: Vec<(PathBuf, FileCategory)> = tracked_files
        .par_iter()
        .map(|path| {
            let full_path = repo_root.join(path);
            let category = match metadata.get(path) {
                Ok(Some(metadatad_state)) => {
                    // Check size first (cheap operation)
                    match get_file_size(&full_path) {
                        Ok(size) if size != metadatad_state.size => FileCategory::Modified,
                        Ok(_) => {
                            // Size matches, check hash
                            match hash_file(&full_path) {
                                Ok(hash) if hash != metadatad_state.hash => FileCategory::Modified,
                                Ok(_) => FileCategory::Unchanged(metadatad_state.clone()),
                                Err(_) => FileCategory::Error,
                            }
                        }
                        Err(_) => FileCategory::Error,
                    }
                }
                Ok(None) => FileCategory::Added,
                Err(_) => FileCategory::Error,
            };
            (path.clone(), category)
        })
        .collect();

    // Collect results and track errors
    let mut errors = Vec::new();
    for (path, category) in results {
        match category {
            FileCategory::Unchanged(state) => unchanged.push(state),
            FileCategory::Modified => modified.push(path),
            FileCategory::Added => added.push(path),
            FileCategory::Error => {
                errors.push(path.clone());
                if verbose > 1 {
                    eprintln!("Warning: Could not analyze file {path:?}");
                }
            }
        }
    }

    // If there were any errors, report them
    if !errors.is_empty() {
        eprintln!("Warning: Failed to analyze {} file(s)", errors.len());
        if verbose == 0 {
            eprintln!("Run with -v for more details");
        }
    }

    Ok((unchanged, modified, added))
}

/// Category for file analysis
enum FileCategory {
    Unchanged(FileState),
    Modified,
    Added,
    Error,
}

/// Execute commands based on the parsed CLI arguments.
///
/// This is the main entry point for executing cargo-hold commands when using
/// it as a library. It takes a parsed `Cli` struct and executes the appropriate
/// command based on the parsed arguments.
///
/// # Arguments
///
/// * `cli` - A reference to the parsed CLI arguments
///
/// # Returns
///
/// Returns `Ok(())` on success, or an error if the command fails.
///
/// # Example
///
/// ```no_run
/// use std::path::PathBuf;
///
/// use cargo_hold::cli::{Cli, Commands, GlobalOpts};
/// use cargo_hold::commands::execute;
///
/// let cli = Cli::builder()
///     .command(Commands::Anchor)
///     .target_dir(PathBuf::from("target"))
///     .verbose(1)
///     .quiet(false)
///     .build()?;
///
/// let result = execute(&cli);
/// if let Err(e) = result {
///     eprintln!("Error: {e:?}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn execute(cli: &Cli) -> Result<()> {
    execute_with_dir(cli, None)
}

/// Execute commands with an explicit working directory.
///
/// This variant allows specifying a working directory for operations,
/// which is useful for testing and when the tool is invoked from
/// different locations.
///
/// # Arguments
///
/// * `cli` - A reference to the parsed CLI arguments
/// * `working_dir` - Optional working directory to use (defaults to current
///   directory)
///
/// # Returns
///
/// Returns `Ok(())` on success, or an error if the command fails.
pub fn execute_with_dir(cli: &Cli, working_dir: Option<&Path>) -> Result<()> {
    // Set up logging verbosity
    let verbose = if cli.global_opts().quiet() {
        0
    } else {
        cli.global_opts().verbose()
    };

    // Get the working directory
    let current_dir = if let Some(dir) = working_dir {
        dir.to_path_buf()
    } else {
        std::env::current_dir().map_err(|source| HoldError::IoError {
            path: PathBuf::from("."),
            source,
        })?
    };

    // Get the effective metadata path (already absolute)
    let metadata_path = cli.global_opts().get_metadata_path();

    // Get the absolute target directory
    let target_dir = cli.global_opts().get_target_dir();

    // Execute the appropriate command
    match cli.command() {
        Commands::Anchor => anchor(&metadata_path, verbose, &current_dir),
        Commands::Salvage => salvage(&metadata_path, verbose, &current_dir),
        Commands::Stow => stow(&metadata_path, verbose, &current_dir),
        Commands::Bilge => bilge(&metadata_path, verbose),
        Commands::Heave {
            max_target_size,
            dry_run,
            debug,
            preserve_cargo_binaries,
            age_threshold_days,
        } => Heave::builder()
            .target_dir(&target_dir)
            .max_target_size(max_target_size.as_deref())
            .dry_run(*dry_run)
            .debug(*debug)
            .preserve_cargo_binaries(preserve_cargo_binaries)
            .age_threshold_days(*age_threshold_days)
            .verbose(verbose)
            .metadata_path(&metadata_path)
            .build()
            .heave(),
        Commands::Voyage {
            max_target_size,
            gc_dry_run,
            gc_debug,
            preserve_cargo_binaries,
            gc_age_threshold_days,
        } => Voyage::builder()
            .metadata_path(&metadata_path)
            .target_dir(&target_dir)
            .max_target_size(max_target_size.as_deref())
            .gc_dry_run(*gc_dry_run)
            .gc_debug(*gc_debug)
            .preserve_cargo_binaries(preserve_cargo_binaries)
            .gc_age_threshold_days(*gc_age_threshold_days)
            .verbose(verbose)
            .build()?
            .run(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn setup_git_repo() -> TempDir {
        let temp_dir = TempDir::new().unwrap();

        // Initialize git repo
        let repo = git2::Repository::init(temp_dir.path()).unwrap();

        // Create and add a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(Path::new("test.txt")).unwrap();
        index.write().unwrap();

        temp_dir
    }

    #[test]
    fn test_stow_command() {
        let temp_dir = setup_git_repo();
        let metadata_path = temp_dir.path().join("test.metadata");

        stow(&metadata_path, 0, temp_dir.path()).unwrap();
        assert!(metadata_path.exists());
        let metadata = load_metadata(&metadata_path).unwrap();
        assert_eq!(metadata.len(), 1);
    }

    #[test]
    fn test_stow_from_subdirectory() {
        let temp_dir = setup_git_repo();

        // Create a subdirectory
        let subdir = temp_dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        // Create metadata path in parent directory
        let metadata_path = temp_dir.path().join("test.metadata");

        // Run stow from subdirectory - it should find the parent git repo
        stow(&metadata_path, 0, &subdir).unwrap();
        assert!(metadata_path.exists());
        let metadata = load_metadata(&metadata_path).unwrap();
        assert_eq!(metadata.len(), 1);
    }

    #[test]
    fn test_salvage_from_subdirectory() {
        let temp_dir = setup_git_repo();

        // Create a subdirectory
        let subdir = temp_dir.path().join("src");
        fs::create_dir(&subdir).unwrap();

        let metadata_path = temp_dir.path().join("test.metadata");

        // First stow from the root
        stow(&metadata_path, 0, temp_dir.path()).unwrap();

        // Now run salvage from subdirectory
        salvage(&metadata_path, 0, &subdir).unwrap();
    }

    #[test]
    fn test_bilge_command() {
        let temp_dir = setup_git_repo();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Create metadata first
        stow(&metadata_path, 0, temp_dir.path()).unwrap();
        assert!(metadata_path.exists());

        // Bilge it
        bilge(&metadata_path, 0).unwrap();
        assert!(!metadata_path.exists());
    }

    #[test]
    fn test_anchor_command() {
        let temp_dir = setup_git_repo();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Run anchor
        anchor(&metadata_path, 0, temp_dir.path()).unwrap();

        // Metadata should exist
        assert!(metadata_path.exists());
        let metadata = load_metadata(&metadata_path).unwrap();
        assert_eq!(metadata.len(), 1);
    }
}
