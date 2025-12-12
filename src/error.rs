//! Error types for cargo-hold.
//!
//! This module defines all error types used throughout cargo-hold, using
//! a combination of `thiserror` for ergonomic error definitions and `miette`
//! for rich diagnostic output.
//!
//! # Error Handling Strategy
//!
//! - All errors derive from [`HoldError`]
//! - Each variant includes helpful error messages and diagnostic codes
//! - Context is preserved through the error chain
//! - Errors are automatically converted to `miette::Result` for CLI output
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//!
//! use cargo_hold::error::{HoldError, Result};
//!
//! fn check_repo(path: &Path) -> Result<()> {
//!     // Example of returning a specific error
//!     if !path.join(".git").exists() {
//!         return Err(HoldError::RepoNotFound(path.to_path_buf()));
//!     }
//!     Ok(())
//! }
//! ```

use std::path::PathBuf;

use miette::Diagnostic;
use thiserror::Error;

/// Error types that can occur in cargo-hold operations
#[derive(Error, Debug, Diagnostic)]
pub enum HoldError {
    /// Git repository not found in the current directory or any parent.
    ///
    /// Raised when `git2::Repository::discover()` fails or when the repository
    /// is bare (no working directory). cargo-hold requires a Git repository to
    /// determine which files to track for timestamp management.
    #[error("Git repository not found in '{0}' or any parent directories")]
    #[diagnostic(
        code(cargo_hold::git::repo_not_found),
        help("Ensure 'cargo hold' is run from within a Git repository.")
    )]
    RepoNotFound(
        /// The path where the Git repository was searched for
        PathBuf,
    ),

    /// Failed to read the Git index to enumerate tracked files.
    ///
    /// Wraps errors from `repo.index()` when cargo-hold tries to read
    /// the list of files tracked by Git. The Git index contains the staged
    /// and tracked files that cargo-hold needs to manage.
    #[error("Failed to access Git index")]
    #[diagnostic(code(cargo_hold::git::index_error))]
    IndexError(#[from] git2::Error),

    /// File system I/O error during cargo-hold operations.
    ///
    /// Common causes: permission denied, file not found, disk full,
    /// or memory mapping failures. Used throughout for file operations,
    /// directory creation/removal, and metadata access.
    #[error("I/O error accessing '{path}'")]
    #[diagnostic(code(cargo_hold::io_error))]
    IoError {
        /// The path that caused the I/O error
        path: PathBuf,
        /// The underlying I/O error
        #[source]
        source: std::io::Error,
    },

    /// Failed to serialize StateMetadata to rkyv format.
    ///
    /// Occurs in `save_metadata()` when rkyv serialization fails.
    /// This is typically an internal error. The metadata file can be
    /// reset using `cargo hold bilge`.
    #[error("Failed to serialize metadata")]
    #[diagnostic(
        code(cargo_hold::metadata::serialization_error),
        help(
            "An internal error occurred while trying to save the metadata. Try running 'cargo \
             hold bilge' to reset."
        )
    )]
    SerializationError(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Failed to deserialize metadata from rkyv format.
    ///
    /// Occurs when loading metadata if the file is corrupted or from
    /// an incompatible format. cargo-hold automatically attempts recovery
    /// by resetting the metadata when this error is encountered.
    #[error("Failed to deserialize metadata: {0}")]
    #[diagnostic(
        code(cargo_hold::metadata::deserialization_error),
        help("The metadata file may be corrupted. Run 'cargo hold bilge' to reset it.")
    )]
    DeserializationError(
        /// The underlying deserialization error
        #[source]
        rkyv::rancor::BoxedError,
    ),

    /// Git index path contains invalid UTF-8.
    ///
    /// Raised when converting Git index entry paths from bytes to UTF-8
    /// strings fails. All paths tracked by Git must be valid UTF-8 for
    /// cargo-hold to process them.
    #[error("Invalid path: {message}")]
    #[diagnostic(code(cargo_hold::path::invalid))]
    InvalidPath {
        /// Description of why the path is invalid
        message: String,
    },

    /// Attempted to process a non-regular file (symlink or directory).
    ///
    /// cargo-hold only supports regular files. This error occurs when
    /// trying to hash, get size of, or set timestamps on symlinks or
    /// directories, which are explicitly unsupported.
    #[error("Invalid file type for '{0}': {1}")]
    #[diagnostic(
        code(cargo_hold::file::invalid_type),
        help("cargo-hold only processes regular files tracked by Git.")
    )]
    InvalidFileType(
        /// The path of the invalid file
        PathBuf,
        /// Description of the file type issue
        String,
    ),

    /// Failed to restore a file's modification time.
    ///
    /// Occurs during the salvage operation when cargo-hold cannot
    /// open a file for writing or call `set_modified()`. Common causes
    /// are insufficient permissions or file system restrictions.
    #[error("Failed to set file modification time for '{0}'")]
    #[diagnostic(
        code(cargo_hold::timestamp::set_error),
        help("Ensure you have write permissions for the file.")
    )]
    SetTimestampError(
        /// The file whose timestamp couldn't be set
        PathBuf,
        /// The underlying I/O error
        #[source]
        std::io::Error,
    ),

    /// Failed to create parent directory for metadata file.
    ///
    /// Raised when `fs::create_dir_all()` fails while preparing to
    /// save metadata. The metadata file is typically stored at
    /// `target/cargo-hold.metadata`.
    #[error("Failed to create metadata directory '{0}'")]
    #[diagnostic(
        code(cargo_hold::metadata::create_dir_error),
        help("Ensure you have write permissions for the parent directory.")
    )]
    CreateMetadataDirError(
        /// The directory path that couldn't be created
        PathBuf,
        /// The underlying I/O error
        #[source]
        std::io::Error,
    ),

    /// Invalid size specification for --max-target-size.
    ///
    /// Raised when parsing size strings like "5G" or "500M" fails.
    /// Valid suffixes are B (bytes), K (kilobytes), M (megabytes),
    /// G (gigabytes), or T (terabytes). Numbers without suffix are bytes.
    #[error("Invalid metadata size: '{0}' - {1}")]
    #[diagnostic(
        code(cargo_hold::gc::invalid_metadata_size),
        help(
            "Specify metadata size as a number with optional suffix (e.g., '5G', '500M', '1024K', \
             or raw bytes)"
        )
    )]
    InvalidMetadataSize(
        /// The invalid size value provided
        String,
        /// Description of the parsing error
        String,
    ),

    /// Cannot determine home directory for cargo cache cleanup.
    ///
    /// Raised when `home::cargo_home()` returns None during garbage
    /// collection of ~/.cargo/registry or ~/.cargo/bin. The home
    /// directory is needed to locate cargo's cache directories.
    #[error("Garbage collection error: {0}")]
    #[diagnostic(
        code(cargo_hold::gc::error),
        help("Check permissions and disk space, then try again.")
    )]
    GcError(
        /// Description of the garbage collection error
        String,
    ),

    /// Metadata version is newer than supported or configuration invalid.
    ///
    /// Raised when: 1) loaded metadata has version > METADATA_VERSION,
    /// indicating it was created by a newer cargo-hold version, or
    /// 2) required parameters are missing for the voyage command.
    #[error("Configuration error: {0}")]
    #[diagnostic(
        code(cargo_hold::config::error),
        help("Check the required configuration parameters.")
    )]
    ConfigError(
        /// Description of the configuration error
        String,
    ),

    /// PathBuf cannot be converted to UTF-8 string for storage.
    ///
    /// Raised in StateMetadata operations when a PathBuf contains
    /// non-UTF-8 sequences. All paths must be valid UTF-8 for storage
    /// in the metadata format and compatibility with Git.
    #[error("Invalid UTF-8 in path: {0}")]
    #[diagnostic(
        code(cargo_hold::path::invalid_utf8),
        help("File paths must be valid UTF-8. This is a requirement for Git-tracked files.")
    )]
    InvalidUtf8Path(
        /// The path containing invalid UTF-8
        PathBuf,
    ),
}

/// Type alias for Results in this crate
pub type Result<T> = std::result::Result<T, HoldError>;
