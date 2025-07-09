//! # cargo-hold CLI
//!
//! The command-line interface for cargo-hold, a CI tool that ensures Cargo's
//! incremental compilation is reliable by managing timestamps using
//! content-based change detection.
//!
//! ## Installation
//!
//! ```bash
//! cargo install cargo-hold
//! # or with cargo-binstall:
//! cargo binstall cargo-hold
//! ```
//!
//! ## Commands
//!
//! - **anchor**: Main CI command - restores timestamps and updates metadata
//! - **salvage**: Restores file timestamps from metadata
//! - **stow**: Saves current file state to metadata
//! - **bilge**: Clears metadata for a fresh start
//! - **heave**: Garbage collection for build artifacts
//! - **voyage**: Combined anchor + heave for complete CI workflow
//!
//! ## Quick Start
//!
//! In your CI pipeline:
//!
//! ```bash
//! # Basic usage - restore timestamps before building
//! cargo hold anchor
//! cargo build --release
//!
//! # With garbage collection
//! cargo hold voyage --max-target-size 5G
//! cargo build --release
//! ```
//!
//! ## Environment Variables
//!
//! - `CARGO_HOLD_TARGET_DIR`: Override target directory (default: ./target)
//! - `CARGO_HOLD_METADATA_PATH`: Custom metadata file location
//! - `CARGO_HOLD_VERBOSE`: Enable verbose output
//! - `CARGO_HOLD_QUIET`: Silence all output except errors
//!
//! See individual commands for more environment variables.

use std::io::IsTerminal;

use cargo_hold::cli::Cli;

fn main() -> miette::Result<()> {
    // Install miette's fancy panic and error report handler
    miette::set_panic_hook();

    // Configure miette handler based on terminal capabilities
    // This provides better error formatting for both TTY and non-TTY environments
    if std::io::stderr().is_terminal() {
        miette::set_hook(Box::new(|_| {
            Box::new(
                miette::GraphicalReportHandler::new()
                    .with_theme(miette::GraphicalTheme::unicode_nocolor())
                    .with_context_lines(3),
            )
        }))?;
    } else {
        // Use a simpler handler for non-TTY environments (CI, logs, etc.)
        miette::set_hook(Box::new(|_| {
            Box::new(
                miette::GraphicalReportHandler::new()
                    .with_theme(miette::GraphicalTheme::none())
                    .with_context_lines(0),
            )
        }))?;
    }

    // Parse command line arguments
    let cli = Cli::parse_args();

    // Execute the appropriate command
    let result = cargo_hold::commands::execute(&cli);

    // Convert our error type to miette's Result
    result.map_err(Into::into)
}
