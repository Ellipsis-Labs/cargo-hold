# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

cargo-hold is a Rust-based CI tool that solves Cargo's incremental compilation issues in CI environments by intelligently managing timestamps using content-based change detection.

## Key Architecture

The codebase follows a modular architecture:

- `cli.rs`: Command-line interface using clap with subcommands (anchor, salvage, stow, bilge, heave, voyage)
- `state.rs`: Core build state management with BLAKE3 hashing for content tracking
- `metadata.rs`: Persistence layer using rkyv for zero-copy deserialization
- `discovery.rs`: Git integration for tracking only version-controlled files
- `timestamp.rs`: Monotonic timestamp generation to ensure build consistency
- `gc.rs`: Garbage collection logic for managing build artifact lifecycle
- `error.rs`: Error types and handling with thiserror + miette
- `hashing.rs`: File hashing utilities using BLAKE3 with memory-mapped I/O
- `commands.rs`: Command execution logic for all subcommands

The tool stores metadata in `target/cargo-hold.metadata` and uses parallel processing via rayon for performance.

## Development Commands

```bash
# Build
cargo build
cargo build --release

# Run tests
cargo test
cargo nextest run              # Preferred, especially in CI
cargo nextest run --profile ci # CI profile (no fail-fast)

# Lint and check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo +nightly fmt --check
cargo deny check               # License compliance
cargo audit                    # Security audit

# Format code
cargo +nightly fmt

# Run a single test
cargo test test_name
cargo nextest run -E 'test(test_name)'

# Install locally
cargo install --path .
cargo binstall cargo-hold      # If cargo-binstall is available
```

## Testing Approach

- Unit tests are embedded in source files using `#[cfg(test)]` modules
- Integration tests in `tests/` directory test full command workflows
- Tests use temporary directories and mock Git repositories
- Key testing utilities: `assert_fs`, `predicates`, `proptest`, `filetime`, `tempfile`
- nextest configuration: CI profile with 90s slow timeout (3 periods), no fail-fast
- The `anchor` command is the primary CI integration point

## CI Integration

The project uses GitHub Actions with:

- Matrix testing on Ubuntu and macOS
- Custom actions in `.github/actions/`:
  - `rust-cache`: Separate registry and target caches
  - `cargo-hold-install`: Installs and runs `cargo hold voyage`
- Jobs: check, test, clippy, fmt, deny, audit
- Release profile optimization with single codegen unit and LTO
- Uses cargo-hold itself (`cargo hold voyage`) in CI

## Important Implementation Details

1. **Metadata Format**: Version 2 format with automatic migration from v1
2. **Error Handling**: Uses thiserror for type-safe errors + miette for rich error reporting
3. **Performance**: BLAKE3 with mmap for efficient file hashing, rayon for parallelism
4. **Git Safety**: Only processes Git-tracked files, respects .gitignore
5. **Timestamp Logic**: Always advances timestamps monotonically to maintain Cargo's assumptions
6. **Minimum Rust Version**: 1.88.0
7. **Pre-commit Hooks**: lefthook configuration for taplo, prettier, rustfmt, and alejandra

## Command Overview

- **anchor**: Main CI command - salvages timestamps and updates state
- **salvage**: Restores file timestamps based on content hashes
- **stow**: Saves current file state to metadata
- **bilge**: Clears metadata for fresh start
- **heave**: Garbage collection with size and age thresholds
- **voyage**: Combines anchor + heave for complete CI workflow

## Development Recommendations

- Use `cargo nextest run` rather than `cargo test` for running tests
- Always run `cargo clippy` before committing
- Use `cargo +nightly fmt` for formatting (project uses 2024 style edition)
- Check licenses with `cargo deny check` when adding dependencies
- Use conventional commits-style commit messages https://www.conventionalcommits.org/en/v1.0.0/

## Error Handling Guidelines

cargo-hold uses a carefully chosen combination of error handling crates that balance developer ergonomics with exceptional user experience:

- [`thiserror`](https://crates.io/crates/thiserror): For defining strongly-typed, zero-cost error enums with automatic trait implementations
- [`miette`](https://crates.io/crates/miette): For rich, compiler-quality error diagnostics in user-facing applications

**The recommendation is to use `thiserror` + `miette` for all new code.**

### Core Principles

- **Libraries**: Define concrete error types with `thiserror`, return `Result<T, YourError>`
- **Applications**: Use `miette::Result<T>` at the top level for beautiful error reporting
- **Never** use `unwrap()` - use `expect()` only when certain a condition is infallible
- **Always** add context when propagating errors with `.wrap_err()`
- **Testing**: Use `matches!` macro to test error types, never convert errors to strings

### Error Definition Pattern

```rust
use thiserror::Error;
use miette::{Diagnostic, SourceSpan};

#[derive(Error, Debug, Diagnostic)]
pub enum CargoHoldError {
    #[error("Failed to read metadata from {path}")]
    #[diagnostic(
        code(cargo_hold::metadata::read_failed),
        help("Check if the file exists and has correct permissions")
    )]
    MetadataReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Invalid timestamp sequence detected")]
    #[diagnostic(code(cargo_hold::timestamp::invalid_sequence))]
    InvalidTimestamp {
        #[source_code]
        context: String,
        #[label("timestamp conflict here")]
        span: SourceSpan,
    },
}
```

### Error Propagation Pattern

```rust
use miette::{IntoDiagnostic, Result, WrapErr};

// Application entry points use miette::Result
fn main() -> Result<()> {
    let args = Args::parse();

    run_command(args)
        .wrap_err("cargo-hold command failed")?;

    Ok(())
}

// Internal functions add context as errors propagate
fn process_metadata(path: &Path) -> miette::Result<Metadata> {
    let content = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to read metadata from '{}'", path.display()))?;

    let metadata = rkyv::from_bytes(&content)
        .map_err(|e| MetadataError::DeserializationFailed {
            path: path.to_owned(),
            source: e,
        })
        .into_diagnostic()?;

    Ok(metadata)
}
```

### Testing Error Handling

```rust
#[test]
fn test_metadata_version_mismatch() {
    let result = load_metadata("fixtures/v1_metadata.bin");

    // ✅ GOOD: Use matches! to verify error type
    assert!(matches!(
        result,
        Err(MetadataError::VersionMismatch { expected: 2, found: 1 })
    ));

    // ❌ BAD: Don't convert to strings for testing
    // assert!(result.unwrap_err().to_string().contains("version"));
}
```

### Common Patterns in cargo-hold

1. **File Operations**: Always add path context

```rust
fs::create_dir_all(&cache_dir)
    .into_diagnostic()
    .wrap_err_with(|| format!("Failed to create cache directory '{}'", cache_dir.display()))?;
```

2. **Git Operations**: Include repository state

```rust
git_command.output()
    .into_diagnostic()
    .wrap_err("Failed to execute git command")
    .wrap_err_with(|| format!("Working directory: {}", repo_path.display()))?;
```

3. **Timestamp Validation**: Provide actionable errors

```rust
if new_timestamp <= old_timestamp {
    return Err(TimestampError::NonMonotonic {
        old: old_timestamp,
        new: new_timestamp,
        file: path.to_owned(),
    })
    .into_diagnostic()
    .wrap_err("Timestamp consistency check failed")?;
}
```
