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
//! use cargo_hold::gc::config::Gc;
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

mod artifacts;
pub(crate) mod auto_cap;
mod cargo;
mod cleanup;
pub mod config;
mod size;
#[cfg(test)]
mod tests;

pub(crate) use cleanup::calculate_directory_size;
pub(crate) use size::{format_size, parse_size};
