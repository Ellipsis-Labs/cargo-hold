//! # cargo-hold
//!
//! A CI tool that ensures Cargo's incremental compilation is reliable by
//! managing timestamps using content-based change detection.
//!
//! ## Overview
//!
//! cargo-hold solves the fundamental problem of using Cargo's incremental
//! compilation in CI environments where timestamps are unreliable. It uses
//! BLAKE3 content hashing to detect actual file changes and intelligently
//! manages timestamps to maximize build cache efficiency.
//!
//! ## Key Features
//!
//! - **Content-based change detection**: Uses BLAKE3 hashing instead of
//!   timestamps
//! - **Monotonic timestamp generation**: Ensures Cargo's assumptions about file
//!   ordering
//! - **Git-aware**: Only tracks version-controlled files, respecting .gitignore
//! - **Zero-copy deserialization**: Fast metadata loading with rkyv
//! - **Parallel processing**: Leverages rayon for efficient file scanning
//! - **Garbage collection**: Intelligent cleanup of old build artifacts
//!
//! ## Architecture
//!
//! The crate is organized into several modules:
//!
//! - [`cli`]: Command-line interface definitions using clap
//! - [`commands`]: Implementation of all cargo-hold subcommands
//! - [`error`]: Error types and handling with thiserror + miette
//! - [`gc`]: Garbage collection for build artifacts and cargo cache
//!
//! Internal modules (not part of the public API):
//! - `state`: Core build state management with content tracking
//! - `metadata`: Persistence layer for build state
//! - `discovery`: Git integration for file discovery
//! - `timestamp`: Monotonic timestamp generation
//! - `hashing`: BLAKE3-based file hashing utilities
//!
//! ## Usage in CI
//!
//! The primary CI integration point is the `anchor` command:
//!
//! ```bash
//! # In your CI pipeline, before building:
//! cargo hold anchor
//! cargo build --release
//! ```
//!
//! For complete CI workflow with garbage collection:
//!
//! ```bash
//! # Combines anchor + heave commands
//! cargo hold voyage --max-target-size 5G
//! cargo build --release
//! ```
//!
//! ## Library Usage
//!
//! While cargo-hold is primarily a CLI tool, it exposes its core functionality
//! as a library for integration into other tools:
//!
//! ```no_run
//! use cargo_hold::cli::{Cli, Commands};
//! use cargo_hold::commands;
//!
//! // Create CLI instance programmatically using the builder
//! let cli = Cli::builder()
//!     .target_dir("target")
//!     .verbose(1)
//!     .command(Commands::Anchor)
//!     .build()?;
//!
//! // Execute the command
//! commands::execute(&cli)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Performance
//!
//! cargo-hold is designed for speed:
//! - Memory-mapped I/O for file hashing
//! - Parallel file processing with rayon
//! - Zero-copy metadata deserialization
//! - BLAKE3 for fast cryptographic hashing
//!
//! ## Error Handling
//!
//! The crate uses a combination of:
//! - `thiserror` for strongly-typed errors
//! - `miette` for rich diagnostic output in CLI
//!
//! All public functions return `Result` types with descriptive error variants.

// Re-export public modules for library usage
pub mod cli;
pub mod commands;
pub mod error;
pub mod gc;

// Internal modules
mod discovery;
mod hashing;
mod logging;
mod metadata;
mod state;
mod timestamp;
