use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use assert_fs::TempDir;
use cargo_hold::cli::{Cli, Commands, GcArgs};
use cargo_hold::commands::execute_with_dir;
use cargo_hold::error::Result;
use miette::{Context, IntoDiagnostic};

use crate::common::TempHomeGuard;

pub struct TestWorkspace {
    dir: TempDir,
    _home: TempHomeGuard,
}

impl TestWorkspace {
    pub fn new() -> Self {
        let home = TempHomeGuard::new();
        let dir = TempDir::new().unwrap();
        Self { dir, _home: home }
    }
}

impl std::ops::Deref for TestWorkspace {
    type Target = TempDir;

    fn deref(&self) -> &Self::Target {
        &self.dir
    }
}

impl std::ops::DerefMut for TestWorkspace {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.dir
    }
}

/// Helper to create a test Git repository
pub fn setup_test_repo() -> TestWorkspace {
    let temp_dir = TestWorkspace::new();

    // Initialize git repo
    let repo = git2::Repository::init(temp_dir.path()).unwrap();

    // Create test files
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();

    let main_rs = src_dir.join("main.rs");
    fs::write(&main_rs, "fn main() { println!(\"Hello\"); }").unwrap();

    let lib_rs = src_dir.join("lib.rs");
    fs::write(&lib_rs, "pub fn hello() { }").unwrap();

    // Add files to git index
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("src/main.rs")).unwrap();
    index.add_path(Path::new("src/lib.rs")).unwrap();
    index.write().unwrap();

    temp_dir
}

/// Helper to execute a command using the library
pub fn execute_command(command: Commands, temp_dir: &TempDir, verbose: u8) -> Result<()> {
    execute_command_with_dir(command, temp_dir, temp_dir.path(), verbose)
}

/// Helper to execute a command from a specific directory
pub fn execute_command_with_dir(
    command: Commands,
    temp_dir: &TempDir,
    working_dir: &Path,
    verbose: u8,
) -> Result<()> {
    // Use absolute paths for everything
    let target_dir = temp_dir.path().join("target");

    let cli = Cli::builder()
        .target_dir(target_dir)
        .verbose(verbose)
        .quiet(false)
        .command(command)
        .build()?;

    // Use the new execute_with_dir function
    execute_with_dir(&cli, Some(working_dir))
}

/// Helper to create a complete Cargo project with Cargo.toml
pub fn setup_cargo_project() -> TestWorkspace {
    let temp_dir = TestWorkspace::new();

    // Initialize git repo
    let repo = git2::Repository::init(temp_dir.path()).unwrap();

    // Create Cargo.toml
    let cargo_toml = temp_dir.path().join("Cargo.toml");
    fs::write(
        &cargo_toml,
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "test-bin"
path = "src/main.rs"

[dependencies]
"#,
    )
    .unwrap();

    // Create src directory and files
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();

    let main_rs = src_dir.join("main.rs");
    fs::write(
        &main_rs,
        r#"fn main() {
    println!("Hello, world!");
    lib_function();
}

fn lib_function() {
    println!("Library function called");
}
"#,
    )
    .unwrap();

    let lib_rs = src_dir.join("lib.rs");
    fs::write(
        &lib_rs,
        r#"pub fn hello() {
    println!("Hello from lib");
}

pub fn add(left: usize, right: usize) -> usize {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
"#,
    )
    .unwrap();

    // Add files to git index
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("Cargo.toml")).unwrap();
    index.add_path(Path::new("src/main.rs")).unwrap();
    index.add_path(Path::new("src/lib.rs")).unwrap();
    index.write().unwrap();

    // Create initial commit
    let sig = git2::Signature::now("Test User", "test@example.com").unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
        .unwrap();

    temp_dir
}

/// Helper to run a cargo command in a directory
pub fn run_cargo_command(
    args: &[&str],
    working_dir: &Path,
) -> miette::Result<std::process::Output> {
    let output = Command::new("cargo")
        .args(args)
        .current_dir(working_dir)
        .output()
        .into_diagnostic()?;
    Ok(output)
}

/// Helper to run cargo-hold voyage command
pub fn run_voyage(temp_dir: &TempDir, verbose: u8) -> Result<()> {
    execute_command(
        Commands::Voyage {
            gc: GcArgs::new(None, vec![]),
            gc_dry_run: false,
            gc_debug: false,
            gc_age_threshold_days: 7,
            gc_auto_max_target_size: true,
        },
        temp_dir,
        verbose,
    )
}

/// Helper to reset all source file timestamps to current time
pub fn reset_source_timestamps(project_dir: &Path) -> miette::Result<()> {
    let current_time = SystemTime::now();

    // Reset Cargo.toml
    let cargo_toml = project_dir.join("Cargo.toml");
    if cargo_toml.exists() {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&cargo_toml)
            .into_diagnostic()
            .wrap_err("Failed to open Cargo.toml")?;
        file.set_modified(current_time)
            .into_diagnostic()
            .wrap_err("Failed to set modified time")?;
    }

    // Reset all .rs files in src/
    let src_dir = project_dir.join("src");
    if src_dir.exists() {
        for entry in fs::read_dir(&src_dir)
            .into_diagnostic()
            .wrap_err("Failed to read src dir")?
        {
            let entry = entry.into_diagnostic().wrap_err("Failed to read entry")?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let file = fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .into_diagnostic()
                    .wrap_err("Failed to open file")?;
                file.set_modified(current_time)
                    .into_diagnostic()
                    .wrap_err("Failed to set modified time")?;
            }
        }
    }

    Ok(())
}

/// Helper to check if artifacts were built by comparing timestamps
#[allow(dead_code)]
pub fn artifacts_were_built(target_dir: &Path, before_time: SystemTime) -> bool {
    let debug_dir = target_dir.join("debug");
    if !debug_dir.exists() {
        return false;
    }

    // Check for build artifacts newer than before_time
    for entry in fs::read_dir(&debug_dir)
        .unwrap_or_else(|_| panic!("Could not read debug dir"))
        .flatten()
    {
        let path = entry.path();
        if let Ok(metadata) = fs::metadata(&path)
            && let Ok(mtime) = metadata.modified()
            && mtime > before_time
        {
            return true;
        }
    }
    false
}

// Helper function to calculate directory size
pub fn get_directory_size(path: &Path) -> u64 {
    let mut size = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Ok(metadata) = fs::metadata(&path) {
                    size += metadata.len();
                }
            } else if path.is_dir() {
                size += get_directory_size(&path);
            }
        }
    }
    size
}
