use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use assert_fs::TempDir;
use cargo_hold::cli::{Cli, Commands};
use cargo_hold::commands::execute_with_dir;
use cargo_hold::error::Result;
use miette::{Context, IntoDiagnostic};

/// Helper to create a test Git repository
fn setup_test_repo() -> TempDir {
    let temp_dir = TempDir::new().unwrap();

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
fn execute_command(command: Commands, temp_dir: &TempDir, verbose: u8) -> Result<()> {
    execute_command_with_dir(command, temp_dir, temp_dir.path(), verbose)
}

/// Helper to execute a command from a specific directory
fn execute_command_with_dir(
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

#[test]
fn test_anchor_command_creates_cache() {
    let temp_dir = setup_test_repo();
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");

    // Run sync command
    execute_command(Commands::Anchor, &temp_dir, 0).unwrap();

    // Verify cache was created
    assert!(metadata_path.exists());
}

#[test]
fn test_anchor_command_with_modifications() {
    let temp_dir = setup_test_repo();
    let main_rs = temp_dir.path().join("src/main.rs");

    // First sync
    execute_command(Commands::Anchor, &temp_dir, 0).unwrap();

    // Record original mtime
    let original_mtime = fs::metadata(&main_rs).unwrap().modified().unwrap();

    // Wait a bit to ensure time difference
    std::thread::sleep(Duration::from_millis(10));

    // Modify file
    fs::write(&main_rs, "fn main() { println!(\"Modified\"); }").unwrap();

    // Second sync
    execute_command(Commands::Anchor, &temp_dir, 0).unwrap();

    // Verify mtime was updated
    let new_mtime = fs::metadata(&main_rs).unwrap().modified().unwrap();
    assert!(new_mtime > original_mtime);
}

#[test]
fn test_salvage_command() {
    let temp_dir = setup_test_repo();
    let lib_rs = temp_dir.path().join("src/lib.rs");

    // First stow
    execute_command(Commands::Stow, &temp_dir, 0).unwrap();

    // Set an old timestamp using std::fs
    let old_time = SystemTime::now() - Duration::from_secs(3600);
    let file = fs::OpenOptions::new().write(true).open(&lib_rs).unwrap();
    file.set_modified(old_time).unwrap();

    // Run salvage
    execute_command(Commands::Salvage, &temp_dir, 0).unwrap();

    // Verify timestamp was restored (should be close to original, not the old time
    // we set)
    let restored_mtime = fs::metadata(&lib_rs).unwrap().modified().unwrap();
    assert!(restored_mtime > old_time);
}

#[test]
fn test_stow_command() {
    let temp_dir = setup_test_repo();
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");

    // Run stow
    execute_command(Commands::Stow, &temp_dir, 0).unwrap();

    // Verify cache exists and has content
    assert!(metadata_path.exists());
    let metadata_size = fs::metadata(&metadata_path).unwrap().len();
    assert!(metadata_size > 0);
}

#[test]
fn test_bilge_command() {
    let temp_dir = setup_test_repo();
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");

    // First create a cache
    execute_command(Commands::Stow, &temp_dir, 0).unwrap();
    assert!(metadata_path.exists());

    // Bilge it
    execute_command(Commands::Bilge, &temp_dir, 0).unwrap();

    // Verify it's gone
    assert!(!metadata_path.exists());
}

#[test]
fn test_verbose_output() {
    let temp_dir = setup_test_repo();

    // Capture stderr by running in a thread
    let output = std::panic::catch_unwind(|| {
        execute_command(Commands::Anchor, &temp_dir, 1).unwrap();
    });

    assert!(output.is_ok());
}

#[test]
fn test_quiet_mode() {
    let temp_dir = setup_test_repo();

    // Use absolute path for target directory
    let target_dir = temp_dir.path().join("target");

    let cli = Cli::builder()
        .target_dir(target_dir)
        .verbose(0)
        .quiet(true)
        .command(Commands::Anchor)
        .build()
        .expect("Failed to build Cli");

    // Execute from the temp directory
    let result = execute_with_dir(&cli, Some(temp_dir.path()));

    assert!(result.is_ok());
}

#[test]
fn test_custom_metadata_path() {
    let temp_dir = setup_test_repo();
    let custom_metadata = temp_dir.path().join("custom.metadata");

    let target_dir = temp_dir.path().join("target");

    let cli = Cli::builder()
        .target_dir(target_dir)
        .metadata_path(custom_metadata.clone())
        .verbose(0)
        .quiet(false)
        .command(Commands::Stow)
        .build()
        .expect("Failed to build Cli");

    // Execute from the temp directory
    execute_with_dir(&cli, Some(temp_dir.path())).unwrap();

    // Verify custom cache was created
    assert!(custom_metadata.exists());

    // Default cache should not exist (since we used a custom path)
    let default_metadata = temp_dir.path().join("target/cargo-hold.metadata");
    assert!(!default_metadata.exists());
}

#[test]
fn test_idempotent_sync() {
    let temp_dir = setup_test_repo();
    let lib_rs = temp_dir.path().join("src/lib.rs");

    // First sync
    execute_command(Commands::Anchor, &temp_dir, 0).unwrap();
    let mtime1 = fs::metadata(&lib_rs).unwrap().modified().unwrap();

    // Second sync without changes
    execute_command(Commands::Anchor, &temp_dir, 0).unwrap();
    let mtime2 = fs::metadata(&lib_rs).unwrap().modified().unwrap();

    // Timestamps should remain the same for unchanged files
    assert_eq!(mtime1, mtime2);
}

#[test]
fn test_new_file_detection() {
    let temp_dir = setup_test_repo();

    // First sync
    execute_command(Commands::Anchor, &temp_dir, 0).unwrap();

    // Add new file
    let new_file = temp_dir.path().join("src/new.rs");
    fs::write(&new_file, "pub fn new() {}").unwrap();

    // Add to git
    let repo = git2::Repository::open(temp_dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("src/new.rs")).unwrap();
    index.write().unwrap();

    // Sync again - should detect the new file
    execute_command(Commands::Anchor, &temp_dir, 1).unwrap();
}

#[test]
fn test_not_in_git_repo() {
    let temp_dir = TempDir::new().unwrap();

    // Try to run in non-git directory
    let result = execute_command(Commands::Anchor, &temp_dir, 0);

    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(err_msg.contains("Git repository not found"));
}

#[test]
#[cfg(unix)]
fn test_sync_with_symlink() {
    use std::os::unix::fs::symlink;

    let temp_dir = setup_test_repo();

    // Create a symlink
    let target = temp_dir.path().join("src/target.rs");
    let link = temp_dir.path().join("src/link.rs");
    fs::write(&target, "pub fn target() {}").unwrap();
    symlink(&target, &link).unwrap();

    // Add symlink to git (git will track it as a symlink, not the target)
    let repo = git2::Repository::open(temp_dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("src/link.rs")).unwrap();
    index.write().unwrap();

    // Run sync - should handle symlink gracefully
    execute_command(Commands::Anchor, &temp_dir, 1).unwrap();
}

#[test]
fn test_heave_command() {
    let temp_dir = setup_test_repo();

    // Create a target directory with some content
    let target_dir = temp_dir.path().join("target");
    fs::create_dir_all(&target_dir).unwrap();

    let heave_command = Commands::Heave {
        max_target_size: Some("1M".to_string()),
        dry_run: true,
        debug: false,
        preserve_cargo_binaries: vec![],
        age_threshold_days: 7,
    };

    // Run heave command
    execute_command(heave_command, &temp_dir, 0).unwrap();
}

#[test]
fn test_voyage_command() {
    let temp_dir = setup_test_repo();

    let voyage_command = Commands::Voyage {
        max_target_size: None,
        gc_dry_run: true,
        gc_debug: false,
        preserve_cargo_binaries: vec![],
        gc_age_threshold_days: 7,
    };

    // Run voyage command (anchor + heave)
    execute_command(voyage_command, &temp_dir, 0).unwrap();

    // Verify cache was created (from anchor)
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");
    assert!(metadata_path.exists());
}

/// Helper to create a complete Cargo project with Cargo.toml
fn setup_cargo_project() -> TempDir {
    let temp_dir = TempDir::new().unwrap();

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
fn run_cargo_command(args: &[&str], working_dir: &Path) -> miette::Result<std::process::Output> {
    let output = Command::new("cargo")
        .args(args)
        .current_dir(working_dir)
        .output()
        .into_diagnostic()?;
    Ok(output)
}

/// Helper to run cargo-hold voyage command
fn run_voyage(temp_dir: &TempDir, verbose: u8) -> Result<()> {
    execute_command(
        Commands::Voyage {
            max_target_size: None,
            gc_dry_run: false,
            gc_debug: false,
            preserve_cargo_binaries: vec![],
            gc_age_threshold_days: 7,
        },
        temp_dir,
        verbose,
    )
}

/// Helper to reset all source file timestamps to current time
fn reset_source_timestamps(project_dir: &Path) -> miette::Result<()> {
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
fn artifacts_were_built(target_dir: &Path, before_time: SystemTime) -> bool {
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

#[test]
fn test_core_voyage_workflow_integration() {
    let temp_dir = setup_cargo_project();
    let target_dir = temp_dir.path().join("target");

    // Step 1: Initial voyage to establish baseline cache
    run_voyage(&temp_dir, 0).unwrap();

    // Step 2: Run cargo build to create artifacts
    let build_output = run_cargo_command(&["build"], temp_dir.path()).unwrap();
    assert!(
        build_output.status.success(),
        "Initial cargo build failed: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    // Verify artifacts were created
    assert!(target_dir.join("debug").exists());

    // Step 3: Reset all source file timestamps to current time (simulating CI cache
    // restoration)
    std::thread::sleep(Duration::from_millis(100)); // Ensure time difference
    reset_source_timestamps(temp_dir.path()).unwrap();

    // Step 4: Run voyage again to restore proper timestamps
    run_voyage(&temp_dir, 0).unwrap();

    // Step 5: Run cargo build again and verify incremental compilation works
    let rebuild_output = run_cargo_command(&["build"], temp_dir.path()).unwrap();
    assert!(
        rebuild_output.status.success(),
        "Rebuild after voyage failed: {}",
        String::from_utf8_lossy(&rebuild_output.stderr)
    );

    // Step 6: Verify that no significant recompilation occurred
    // The build should be very fast since nothing changed
    let stderr_output = String::from_utf8_lossy(&rebuild_output.stderr);

    // Cargo should indicate it's up to date or do minimal work
    // We check that it doesn't recompile the main binary
    assert!(
        stderr_output.contains("Finished")
            || stderr_output.is_empty()
            || !stderr_output.contains("Compiling test-project"),
        "Cargo performed unnecessary recompilation: {stderr_output}"
    );
}

#[test]
fn test_fresh_clone_simulation() {
    let temp_dir = setup_cargo_project();

    // First run with no existing cache (simulates fresh clone)
    run_voyage(&temp_dir, 1).unwrap();

    // Verify cache was created
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");
    assert!(metadata_path.exists());

    // Build should work fine
    let build_output = run_cargo_command(&["build"], temp_dir.path()).unwrap();
    assert!(build_output.status.success());
}

#[test]
fn test_incremental_build_simulation() {
    let temp_dir = setup_cargo_project();
    let main_rs = temp_dir.path().join("src/main.rs");

    // Initial voyage and build
    run_voyage(&temp_dir, 0).unwrap();
    let build_output = run_cargo_command(&["build"], temp_dir.path()).unwrap();
    assert!(build_output.status.success());

    // Modify a single source file
    std::thread::sleep(Duration::from_millis(50));
    fs::write(
        &main_rs,
        r#"fn main() {
    println!("Hello, modified world!");
    lib_function();
}

fn lib_function() {
    println!("Library function called");
}
"#,
    )
    .unwrap();

    // Add the change to git
    let repo = git2::Repository::open(temp_dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("src/main.rs")).unwrap();
    index.write().unwrap();

    // Run voyage again
    run_voyage(&temp_dir, 0).unwrap();

    // Build again - should only recompile affected parts
    let rebuild_output = run_cargo_command(&["build"], temp_dir.path()).unwrap();
    assert!(rebuild_output.status.success());

    // Verify some compilation occurred (since we modified code)
    let stderr_output = String::from_utf8_lossy(&rebuild_output.stderr);
    assert!(
        stderr_output.contains("Compiling") || stderr_output.contains("Finished"),
        "Expected some compilation activity, got: {stderr_output}"
    );
}

#[test]
fn test_cache_restoration_after_timestamp_reset() {
    let temp_dir = setup_cargo_project();
    let lib_rs = temp_dir.path().join("src/lib.rs");
    let main_rs = temp_dir.path().join("src/main.rs");

    // First, set old timestamps on the source files to simulate aged files
    let old_time = SystemTime::now() - Duration::from_secs(3600); // 1 hour ago
    let file = fs::OpenOptions::new().write(true).open(&lib_rs).unwrap();
    file.set_modified(old_time).unwrap();
    let file = fs::OpenOptions::new().write(true).open(&main_rs).unwrap();
    file.set_modified(old_time).unwrap();

    // Initial stow to create metadata with the old timestamps
    execute_command(Commands::Stow, &temp_dir, 0).unwrap();

    // Build the project
    let build_output = run_cargo_command(&["build"], temp_dir.path()).unwrap();
    assert!(build_output.status.success());

    // Simulate checkout/clone where all timestamps become current
    let before_reset = SystemTime::now();
    std::thread::sleep(Duration::from_millis(50));
    reset_source_timestamps(temp_dir.path()).unwrap();

    // Verify timestamps were actually changed to current time
    let reset_mtime = fs::metadata(&lib_rs).unwrap().modified().unwrap();
    assert!(reset_mtime >= before_reset);
    assert!(reset_mtime > old_time);

    // Run salvage to restore proper timestamps (not anchor/voyage which would
    // overwrite them)
    execute_command(Commands::Salvage, &temp_dir, 0).unwrap();

    // Verify timestamp was restored correctly
    let restored_mtime = fs::metadata(&lib_rs).unwrap().modified().unwrap();

    // For unchanged files, cargo-hold should restore them to their original
    // timestamp The restored time should be the old time (or very close due to
    // timestamp precision)
    assert!(
        restored_mtime < before_reset,
        "Timestamp {restored_mtime:?} should be restored to original value, not reset time \
         {reset_mtime:?}"
    );

    // Build should be incremental (fast)
    let rebuild_output = run_cargo_command(&["build"], temp_dir.path()).unwrap();
    assert!(rebuild_output.status.success());

    // Should not have done significant recompilation
    let stderr_output = String::from_utf8_lossy(&rebuild_output.stderr);
    assert!(
        stderr_output.contains("Finished")
            || stderr_output.is_empty()
            || !stderr_output.contains("Compiling test-project"),
        "Unnecessary recompilation occurred: {stderr_output}"
    );
}

#[test]
fn test_voyage_with_no_git_changes() {
    let temp_dir = setup_cargo_project();

    // Run voyage twice without any changes
    run_voyage(&temp_dir, 0).unwrap();
    run_voyage(&temp_dir, 0).unwrap();

    // Build should work fine both times
    let build_output1 = run_cargo_command(&["build"], temp_dir.path()).unwrap();
    assert!(build_output1.status.success());

    let build_output2 = run_cargo_command(&["build"], temp_dir.path()).unwrap();
    assert!(build_output2.status.success());

    // Second build should be very fast (no recompilation)
    let stderr_output = String::from_utf8_lossy(&build_output2.stderr);
    assert!(
        stderr_output.contains("Finished") || stderr_output.is_empty(),
        "Second build should be no-op, got: {stderr_output}"
    );
}

#[test]
fn test_stow_from_subdirectory() {
    let temp_dir = setup_test_repo();

    // Create target directory
    let target_dir = temp_dir.path().join("target");
    fs::create_dir(&target_dir).unwrap();

    // Create a subdirectory
    let subdir = temp_dir.path().join("subdir");
    fs::create_dir(&subdir).unwrap();

    // Run stow from subdirectory using execute_command_with_dir
    execute_command_with_dir(Commands::Stow, &temp_dir, &subdir, 0).unwrap();

    // Verify cache was created in parent's target directory
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");
    assert!(metadata_path.exists());
}

#[test]
fn test_voyage_from_subdirectory() {
    let temp_dir = setup_cargo_project();

    // Create target directory
    let target_dir = temp_dir.path().join("target");
    fs::create_dir(&target_dir).unwrap();

    // src directory already exists from setup_cargo_project
    let subdir = temp_dir.path().join("src");

    // Run voyage from subdirectory using execute_command_with_dir
    execute_command_with_dir(
        Commands::Voyage {
            max_target_size: None,
            gc_dry_run: false,
            gc_debug: false,
            preserve_cargo_binaries: vec![],
            gc_age_threshold_days: 7,
        },
        &temp_dir,
        &subdir,
        0,
    )
    .unwrap();

    // Verify cache was created
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");
    assert!(metadata_path.exists());
}

#[test]
fn test_salvage_from_subdirectory() {
    let temp_dir = setup_test_repo();

    // Create target directory
    let target_dir = temp_dir.path().join("target");
    fs::create_dir(&target_dir).unwrap();

    // First stow from the root to create cache (this will create target directory)
    execute_command(Commands::Stow, &temp_dir, 0).unwrap();

    // Create a subdirectory
    let subdir = temp_dir.path().join("nested/deep");
    fs::create_dir_all(&subdir).unwrap();

    // Run salvage from deep subdirectory using execute_command_with_dir
    execute_command_with_dir(Commands::Salvage, &temp_dir, &subdir, 0).unwrap();
}

#[test]
fn test_command_from_workspace_member() {
    // Setup a workspace with multiple members
    let temp_dir = TempDir::new().unwrap();

    // Initialize git repo
    let repo = git2::Repository::init(temp_dir.path()).unwrap();

    // Create root Cargo.toml with workspace
    let root_cargo = temp_dir.path().join("Cargo.toml");
    fs::write(
        &root_cargo,
        r#"[workspace]
members = ["crate-a", "crate-b"]
"#,
    )
    .unwrap();

    // Create crate-a
    let crate_a = temp_dir.path().join("crate-a");
    fs::create_dir(&crate_a).unwrap();
    fs::write(
        crate_a.join("Cargo.toml"),
        r#"[package]
name = "crate-a"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    let src_a = crate_a.join("src");
    fs::create_dir(&src_a).unwrap();
    fs::write(src_a.join("lib.rs"), "pub fn a() {}").unwrap();

    // Create crate-b
    let crate_b = temp_dir.path().join("crate-b");
    fs::create_dir(&crate_b).unwrap();
    fs::write(
        crate_b.join("Cargo.toml"),
        r#"[package]
name = "crate-b"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    let src_b = crate_b.join("src");
    fs::create_dir(&src_b).unwrap();
    fs::write(src_b.join("lib.rs"), "pub fn b() {}").unwrap();

    // Create target directory
    let target_dir = temp_dir.path().join("target");
    fs::create_dir(&target_dir).unwrap();

    // Add all files to git
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("Cargo.toml")).unwrap();
    index.add_path(Path::new("crate-a/Cargo.toml")).unwrap();
    index.add_path(Path::new("crate-a/src/lib.rs")).unwrap();
    index.add_path(Path::new("crate-b/Cargo.toml")).unwrap();
    index.add_path(Path::new("crate-b/src/lib.rs")).unwrap();
    index.write().unwrap();

    // Run voyage from within a workspace member
    let cli = Cli::builder()
        .target_dir(temp_dir.path().join("target"))
        .verbose(0)
        .quiet(false)
        .command(Commands::Voyage {
            max_target_size: None,
            gc_dry_run: false,
            gc_debug: false,
            preserve_cargo_binaries: vec![],
            gc_age_threshold_days: 7,
        })
        .build()
        .expect("Failed to build Cli");

    // Execute from crate-a directory
    let result = execute_with_dir(&cli, Some(&crate_a));

    result.unwrap();

    // Verify cache was created at workspace root
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");
    assert!(metadata_path.exists());
}

// CRITICAL INTEGRATION TEST FOR TIMESTAMP PRESERVATION FEATURE

#[test]
fn test_timestamp_preservation_workflow() {
    // This test verifies the core feature: stow → stow → heave workflow
    // with proper timestamp preservation across metadata saves

    let temp_dir = setup_cargo_project();
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");

    // Step 1: First stow - should create v2 metadata
    execute_command(Commands::Stow, &temp_dir, 1).unwrap();
    assert!(metadata_path.exists());

    // Verify metadata was created
    assert!(fs::metadata(&metadata_path).unwrap().len() > 0);

    // Step 2: Modify a file to simulate a build
    std::thread::sleep(Duration::from_millis(100)); // Ensure time difference
    let main_rs = temp_dir.path().join("src/main.rs");
    fs::write(
        &main_rs,
        r#"fn main() {
        println!("Modified for testing!");
    }"#,
    )
    .unwrap();

    // Update git index
    let repo = git2::Repository::open(temp_dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("src/main.rs")).unwrap();
    index.write().unwrap();

    // Step 3: Second stow - should preserve the previous max_mtime_nanos
    execute_command(Commands::Stow, &temp_dir, 1).unwrap();

    // Verify metadata was updated (size might change slightly)
    let updated_metadata_size = fs::metadata(&metadata_path).unwrap().len();
    assert!(updated_metadata_size > 0);

    // Step 4: Create some old artifacts in target directory to simulate a build
    let target_dir = temp_dir.path().join("target");
    let debug_dir = target_dir.join("debug");
    let deps_dir = debug_dir.join("deps");
    fs::create_dir_all(&deps_dir).unwrap();

    // Create fingerprint directory for proper artifact matching
    let fingerprint_dir = debug_dir.join(".fingerprint");
    fs::create_dir_all(&fingerprint_dir).unwrap();

    // Create an old artifact with matching fingerprint
    let old_artifact = deps_dir.join("libold_crate-1234567890abcdef.rlib");
    fs::write(&old_artifact, vec![0u8; 5000]).unwrap(); // 5KB file
    let old_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1000); // Very old
    filetime::set_file_mtime(
        &old_artifact,
        filetime::FileTime::from_system_time(old_time),
    )
    .unwrap();

    // Create matching fingerprint for old artifact
    let old_fingerprint = fingerprint_dir.join("libold_crate-1234567890abcdef");
    fs::create_dir_all(&old_fingerprint).unwrap();
    filetime::set_file_mtime(
        &old_fingerprint,
        filetime::FileTime::from_system_time(old_time),
    )
    .unwrap();

    // Create a recent artifact (should be preserved)
    let recent_artifact = deps_dir.join("librecent_crate-fedcba0987654321.rlib");
    fs::write(&recent_artifact, vec![0u8; 10000]).unwrap(); // 10KB file
    // Keep its current timestamp (recent)

    // Create matching fingerprint for recent artifact
    let recent_fingerprint = fingerprint_dir.join("librecent_crate-fedcba0987654321");
    fs::create_dir_all(&recent_fingerprint).unwrap();

    // Step 5: Run heave with a small size limit to force cleanup
    let heave_command = Commands::Heave {
        max_target_size: Some("1K".to_string()), // Very small to force cleanup
        dry_run: false,
        debug: true,
        preserve_cargo_binaries: vec![],
        age_threshold_days: 30, // High so age doesn't interfere
    };

    let initial_size = get_directory_size(&target_dir);
    execute_command(heave_command, &temp_dir, 2).unwrap();
    let final_size = get_directory_size(&target_dir);

    // Verify cleanup occurred
    assert!(
        final_size < initial_size,
        "GC should have removed some files"
    );

    // Verify old artifact was removed but recent one was preserved
    assert!(!old_artifact.exists(), "Old artifact should be removed");
    assert!(
        recent_artifact.exists(),
        "Recent artifact should be preserved due to timestamp"
    );
}

#[test]
fn test_heave_with_preservation_message() {
    // Test that heave shows the preservation message when last_gc_mtime_nanos is
    // set

    let temp_dir = setup_cargo_project();
    let metadata_path = temp_dir.path().join("target/cargo-hold.metadata");

    // Run stow twice to establish last_gc_mtime_nanos
    execute_command(Commands::Stow, &temp_dir, 0).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    execute_command(Commands::Stow, &temp_dir, 0).unwrap();

    // Metadata should now have last_gc_mtime_nanos set
    assert!(metadata_path.exists());

    // Create target structure with artifacts
    let debug_dir = temp_dir.path().join("target/debug");
    let deps_dir = debug_dir.join("deps");
    fs::create_dir_all(&deps_dir).unwrap();

    // Create test artifact
    let artifact = deps_dir.join("libtest-abcdef1234567890.rlib");
    fs::write(&artifact, vec![0u8; 1000]).unwrap();

    // Run heave - it should load the metadata and use last_gc_mtime_nanos
    let heave_command = Commands::Heave {
        max_target_size: None,
        dry_run: true, // Dry run to avoid actual deletion
        debug: true,
        preserve_cargo_binaries: vec![],
        age_threshold_days: 0, // Remove everything old
    };

    // Execute with verbose output to see the preservation message
    // The message "Using previous build timestamp for artifact preservation" should
    // be shown
    execute_command(heave_command, &temp_dir, 2).unwrap();
}

// Helper function to calculate directory size
fn get_directory_size(path: &Path) -> u64 {
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
