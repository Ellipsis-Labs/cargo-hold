use std::path::{Path, PathBuf};

use clap::Parser;

use crate::cli::{Cli, Commands, normalize_path};

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
