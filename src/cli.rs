//! Command-line interface definitions for cargo-hold.
//!
//! This module defines the CLI structure using clap, including all subcommands
//! and their arguments. The main entry point is the [`Cli`] struct.
//!
//! # Example
//!
//! ```no_run
//! use cargo_hold::cli::{Cli, Commands};
//!
//! // Parse command-line arguments
//! let cli = Cli::parse_args();
//!
//! // Access the parsed command
//! match &cli.command() {
//!     Commands::Anchor => println!("Running anchor command"),
//!     Commands::Voyage {
//!         max_target_size, ..
//!     } => {
//!         println!("Running voyage with size limit: {:?}", max_target_size);
//!     }
//!     _ => {}
//! }
//! ```

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::error::{HoldError, Result};

/// Main command-line interface for cargo-hold.
///
/// This struct represents the top-level CLI configuration, containing both
/// global options that apply to all commands and the specific subcommand
/// to execute.
#[derive(Parser)]
#[command(
    name = "cargo-hold",
    bin_name = "cargo-hold",
    author,
    version,
    about = "A CI tool to ensure Cargo's incremental compilation is reliable",
    long_about = None,
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    global_opts: GlobalOpts,

    #[command(subcommand)]
    command: Commands,
}

/// Global options that apply to all cargo-hold commands.
///
/// These options control the overall behavior of cargo-hold, including
/// where to find the target directory, where to store metadata, and
/// output verbosity levels.
#[derive(Parser)]
pub struct GlobalOpts {
    /// Path to the target directory (defaults to ./target)
    #[arg(
        long,
        global = true,
        default_value = "target",
        env = "CARGO_HOLD_TARGET_DIR"
    )]
    target_dir: PathBuf,

    /// Path to the metadata file (defaults to
    /// `<target-dir>/cargo-hold.metadata`)
    #[arg(long, global = true, env = "CARGO_HOLD_METADATA_PATH")]
    metadata_path: Option<PathBuf>,

    /// Enable verbose output (use multiple times for more verbosity)
    #[arg(short, long, global = true, action = clap::ArgAction::Count, env = "CARGO_HOLD_VERBOSE")]
    verbose: u8,

    /// Silence all output except for errors
    #[arg(
        short,
        long,
        global = true,
        conflicts_with = "verbose",
        env = "CARGO_HOLD_QUIET"
    )]
    quiet: bool,
}

impl GlobalOpts {
    /// Create a new builder for constructing `GlobalOpts` programmatically.
    pub fn builder() -> GlobalOptsBuilder {
        GlobalOptsBuilder::default()
    }

    /// Get the effective metadata path
    pub fn get_metadata_path(&self) -> PathBuf {
        let path = self
            .metadata_path()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.target_dir().join("cargo-hold.metadata"));

        normalize_path(path)
    }

    /// Get the absolute target directory path
    pub fn get_target_dir(&self) -> PathBuf {
        normalize_path(self.target_dir())
    }

    /// Get the target directory
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// Get the metadata path option
    pub fn metadata_path(&self) -> Option<&Path> {
        self.metadata_path.as_deref()
    }

    /// Get the verbose level
    pub fn verbose(&self) -> u8 {
        self.verbose
    }

    /// Check if quiet mode is enabled
    pub fn quiet(&self) -> bool {
        self.quiet
    }
}

/// Builder for constructing `GlobalOpts` programmatically.
///
/// This builder provides a fluent API for creating `GlobalOpts` instances
/// without going through command-line parsing. Useful for testing and
/// programmatic usage.
#[derive(Default)]
pub struct GlobalOptsBuilder {
    target_dir: Option<PathBuf>,
    metadata_path: Option<PathBuf>,
    verbose: u8,
    quiet: bool,
}

impl GlobalOptsBuilder {
    /// Set the target directory path.
    pub fn target_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.target_dir = Some(dir.into());
        self
    }

    /// Set the metadata file path.
    pub fn metadata_path(mut self, path: Option<impl Into<PathBuf>>) -> Self {
        self.metadata_path = path.map(|p| p.into());
        self
    }

    /// Set the verbosity level (0 = normal, 1+ = verbose).
    pub fn verbose(mut self, level: u8) -> Self {
        self.verbose = level;
        self
    }

    /// Enable or disable quiet mode.
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    /// Build the `GlobalOpts` instance with the configured values.
    pub fn build(self) -> GlobalOpts {
        GlobalOpts {
            target_dir: self.target_dir.unwrap_or_else(|| PathBuf::from("target")),
            metadata_path: self.metadata_path,
            verbose: self.verbose,
            quiet: self.quiet,
        }
    }
}

impl Cli {
    /// Get the global options
    pub fn global_opts(&self) -> &GlobalOpts {
        &self.global_opts
    }

    /// Get the command
    pub fn command(&self) -> &Commands {
        &self.command
    }

    /// Create a builder for programmatic construction
    pub fn builder() -> CliBuilder {
        CliBuilder::default()
    }
}

/// Builder for [`Cli`]
#[derive(Debug, Default)]
pub struct CliBuilder {
    target_dir: Option<PathBuf>,
    metadata_path: Option<PathBuf>,
    verbose: u8,
    quiet: bool,
    command: Option<Commands>,
}

impl CliBuilder {
    /// Set the target directory
    pub fn target_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.target_dir = Some(dir.into());
        self
    }

    /// Set the metadata path
    pub fn metadata_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.metadata_path = Some(path.into());
        self
    }

    /// Set the verbose level
    pub fn verbose(mut self, level: u8) -> Self {
        self.verbose = level;
        self
    }

    /// Enable quiet mode
    pub fn quiet(mut self, enabled: bool) -> Self {
        self.quiet = enabled;
        self
    }

    /// Set the command
    pub fn command(mut self, command: Commands) -> Self {
        self.command = Some(command);
        self
    }

    /// Build the Cli instance
    pub fn build(self) -> Result<Cli> {
        let command = self.command.ok_or(HoldError::ConfigError {
            message: "Command is required".to_string(),
        })?;

        Ok(Cli {
            global_opts: GlobalOpts::builder()
                .target_dir(self.target_dir.unwrap_or_else(|| PathBuf::from("target")))
                .metadata_path(self.metadata_path)
                .verbose(self.verbose)
                .quiet(self.quiet)
                .build(),
            command,
        })
    }
}

/// Normalize a path to be absolute and clean, without requiring it to exist.
///
/// This function:
/// - Converts relative paths to absolute using the current directory
/// - Removes `.` and `..` components where possible
/// - Does NOT resolve symlinks (preserves user intent)
/// - Does NOT require the path to exist
///
/// For paths that must exist, consider using canonicalize() instead.
fn normalize_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();

    // First, make it absolute if it's relative
    let absolute = if path.is_relative() {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    } else {
        path.to_path_buf()
    };

    // Clean up the path by resolving . and .. components
    let mut components = Vec::new();
    for component in absolute.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                // Pop the last component if it's not a ParentDir
                if let Some(last) = components.last()
                    && !matches!(last, Component::ParentDir)
                {
                    components.pop();
                    continue;
                }
                components.push(component);
            }
            Component::CurDir => {
                // Skip . components
                continue;
            }
            _ => components.push(component),
        }
    }

    // Reconstruct the path
    let mut result = PathBuf::new();
    for component in components {
        result.push(component);
    }

    result
}

/// Available cargo-hold subcommands.
///
/// Each variant represents a different operation that cargo-hold can perform,
/// from managing timestamps and metadata to cleaning up build artifacts.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Anchor your build state (recommended CI command)
    ///
    /// This is the main command that performs the complete workflow:
    /// 1. Restores timestamps from the metadata file based on content changes
    /// 2. Scans all Git-tracked files for modifications
    /// 3. Updates and saves the metadata with the current state
    ///
    /// Use this command in CI before running `cargo build` to ensure
    /// incremental compilation works correctly with cached artifacts.
    Anchor,

    /// Salvage file timestamps from the metadata
    ///
    /// Restores timestamps based on the previous build state:
    /// - Unchanged files: Restored to their original timestamps
    /// - Modified files: Given a new monotonic timestamp
    /// - New files: Given a new monotonic timestamp
    ///
    /// This prevents unnecessary rebuilds while ensuring changed files
    /// are properly recompiled.
    Salvage,

    /// Stow files in the cargo hold
    ///
    /// Scans all Git-tracked files and saves their current state:
    /// - Computes BLAKE3 hashes for content-based change detection
    /// - Records file sizes and modification times
    /// - Saves metadata to enable future timestamp restoration
    ///
    /// Run this after a successful build to update the metadata.
    Stow,

    /// Bilge out the metadata file
    ///
    /// Removes the metadata file, forcing a fresh start on the next run.
    /// Use this when:
    /// - You want to reset the timestamp tracking state
    /// - The metadata file has become corrupted
    /// - You're troubleshooting incremental compilation issues
    Bilge,

    /// Heave ho! Clean up old build artifacts
    ///
    /// Performs garbage collection on build artifacts to reclaim disk space:
    /// - First ensures target directory is under size limit (if specified)
    /// - Then removes artifacts older than the age threshold (default: 7 days)
    /// - Both conditions are always applied together for consistent cleanup
    /// - Always preserves: Binaries, important Cargo files, and recent
    ///   artifacts
    /// - Also cleans: ~/.cargo/registry/cache and ~/.cargo/git/checkouts
    ///
    /// Artifacts are removed by crate (all related files together) to maintain
    /// build consistency.
    Heave {
        /// Maximum target directory size (e.g., "5G", "500M", or bytes)
        #[arg(long, env = "CARGO_HOLD_MAX_TARGET_SIZE")]
        max_target_size: Option<String>,

        /// Show what would be deleted without actually deleting
        #[arg(long, env = "CARGO_HOLD_DRY_RUN")]
        dry_run: bool,

        /// Enable debug output for garbage collection
        #[arg(long, env = "CARGO_HOLD_DEBUG")]
        debug: bool,

        /// Additional binaries to preserve in ~/.cargo/bin (comma-separated)
        #[arg(
            long,
            value_delimiter = ',',
            env = "CARGO_HOLD_PRESERVE_CARGO_BINARIES"
        )]
        preserve_cargo_binaries: Vec<String>,

        /// Age threshold in days for removing artifacts (default: 7)
        #[arg(long, default_value = "7", env = "CARGO_HOLD_AGE_THRESHOLD_DAYS")]
        age_threshold_days: u32,
    },

    /// Full voyage - anchor and heave in one command
    ///
    /// Combines the anchor and heave commands for a complete CI workflow:
    /// 1. First runs anchor to restore timestamps and update metadata
    /// 2. Then runs heave to clean up old artifacts and manage disk usage
    ///
    /// This is ideal for CI pipelines that need both timestamp management
    /// and disk space control in a single command.
    Voyage {
        /// Maximum target directory size (e.g., "5G", "500M", or bytes)
        #[arg(long, env = "CARGO_HOLD_MAX_TARGET_SIZE")]
        max_target_size: Option<String>,

        /// Show what would be deleted without actually deleting
        #[arg(long, env = "CARGO_HOLD_GC_DRY_RUN")]
        gc_dry_run: bool,

        /// Enable debug output for garbage collection
        #[arg(long, env = "CARGO_HOLD_GC_DEBUG")]
        gc_debug: bool,

        /// Additional binaries to preserve in ~/.cargo/bin (comma-separated)
        #[arg(
            long,
            value_delimiter = ',',
            env = "CARGO_HOLD_PRESERVE_CARGO_BINARIES"
        )]
        preserve_cargo_binaries: Vec<String>,

        /// Age threshold in days for garbage collection (default: 7)
        #[arg(long, default_value = "7", env = "CARGO_HOLD_GC_AGE_THRESHOLD_DAYS")]
        gc_age_threshold_days: u32,
    },
}

impl Cli {
    /// Parse command line arguments, handling the cargo subcommand case
    pub fn parse_args() -> Self {
        let args: Vec<String> = std::env::args().collect();

        // When invoked as `cargo hold`, cargo passes "hold" as the first argument
        // We need to skip it to parse the actual subcommand
        if args.len() >= 2 && args[1] == "hold" {
            // Skip the "hold" argument by reconstructing args without it
            let mut new_args = vec![args[0].clone()]; // program name
            new_args.extend_from_slice(&args[2..]); // rest of arguments after "hold"
            return Self::parse_from(new_args);
        }

        // Normal parsing if not invoked through cargo
        Self::parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parsing() {
        let cli = Cli::parse_from(["cargo-hold", "anchor"]);
        assert!(matches!(cli.command(), Commands::Anchor));
        assert_eq!(cli.global_opts().target_dir(), Path::new("target"));
        assert!(cli.global_opts().metadata_path().is_none());
        // get_metadata_path now returns absolute paths
        assert!(
            cli.global_opts()
                .get_metadata_path()
                .ends_with("target/cargo-hold.metadata")
        );
        assert_eq!(cli.global_opts().verbose(), 0);
        assert!(!cli.global_opts().quiet());
    }

    #[test]
    fn test_verbose_flag() {
        let cli = Cli::parse_from(["cargo-hold", "-vv", "stow"]);
        assert_eq!(cli.global_opts().verbose(), 2);
        assert!(matches!(cli.command(), Commands::Stow));
    }

    #[test]
    fn test_custom_metadata_path() {
        let cli = Cli::parse_from([
            "cargo-hold",
            "--metadata-path",
            "custom.metadata",
            "salvage",
        ]);
        assert_eq!(
            cli.global_opts().metadata_path(),
            Some(Path::new("custom.metadata"))
        );
        // get_metadata_path now returns absolute paths
        assert!(
            cli.global_opts()
                .get_metadata_path()
                .ends_with("custom.metadata")
        );
        assert!(matches!(cli.command(), Commands::Salvage));
    }

    #[test]
    fn test_custom_target_dir() {
        let cli = Cli::parse_from(["cargo-hold", "--target-dir", "build", "stow"]);
        assert_eq!(cli.global_opts().target_dir(), Path::new("build"));
        // get_metadata_path now returns absolute paths
        assert!(
            cli.global_opts()
                .get_metadata_path()
                .ends_with("build/cargo-hold.metadata")
        );
        assert!(matches!(cli.command(), Commands::Stow));
    }

    #[test]
    fn test_global_flag_positioning() {
        // Global flags can be placed anywhere
        let cli = Cli::parse_from(["cargo-hold", "bilge", "--verbose"]);
        assert_eq!(cli.global_opts().verbose(), 1);
        assert!(matches!(cli.command(), Commands::Bilge));
    }

    #[test]
    fn test_cli_builder() {
        // Test the builder pattern for programmatic construction
        let cli = Cli::builder()
            .target_dir("custom/target")
            .verbose(2)
            .quiet(false)
            .command(Commands::Anchor)
            .build()
            .expect("Failed to build CLI");

        assert_eq!(cli.global_opts().target_dir(), Path::new("custom/target"));
        assert_eq!(cli.global_opts().verbose(), 2);
        assert!(!cli.global_opts().quiet());
        assert!(matches!(cli.command(), Commands::Anchor));

        // Test builder with metadata path
        let cli = Cli::builder()
            .metadata_path("custom.metadata")
            .command(Commands::Stow)
            .build()
            .expect("Failed to build CLI");

        assert_eq!(
            cli.global_opts().metadata_path(),
            Some(Path::new("custom.metadata"))
        );
        assert!(matches!(cli.command(), Commands::Stow));
    }

    #[test]
    fn test_normalize_path() {
        // Test with current directory components
        let normalized = normalize_path("./target/./debug");
        assert!(normalized.is_absolute());
        // The path might start with ./ if current_dir fails, so check for /./
        assert!(!normalized.to_string_lossy().contains("/./"));

        // Test with parent directory components
        let normalized = normalize_path("target/../other/target");
        assert!(normalized.is_absolute());
        assert!(normalized.ends_with("other/target"));
        assert!(!normalized.to_string_lossy().contains(".."));

        // Test absolute path is preserved
        let abs_path = if cfg!(windows) {
            PathBuf::from("C:\\Users\\test")
        } else {
            PathBuf::from("/home/test")
        };
        let normalized = normalize_path(&abs_path);
        assert_eq!(normalized, abs_path);

        // Test complex path with multiple .. and .
        let normalized = normalize_path("./a/b/../c/./d/../e");
        assert!(normalized.is_absolute());
        assert!(normalized.ends_with("a/c/e"));
    }
}
