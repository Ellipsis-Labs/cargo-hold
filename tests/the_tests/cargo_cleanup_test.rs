//! Tests for cargo home directory cleanup functionality

use std::fs;
use std::time::{Duration, SystemTime};

use cargo_hold::gc::Gc;

use crate::common::TempHomeGuard;

#[test]
fn test_clean_cargo_registry_with_mock_home() {
    // Create a temporary directory to act as cargo home
    let home = TempHomeGuard::new();
    let cargo_home = home.cargo_home();

    // Set up .cargo/registry/cache with old files
    let cache_dir = cargo_home
        .join("registry")
        .join("cache")
        .join("github.com-123");
    fs::create_dir_all(&cache_dir).unwrap();

    // Create old file (40 days old)
    let old_file = cache_dir.join("old-crate-1.0.0.crate");
    fs::write(&old_file, b"old content").unwrap();
    let old_time = SystemTime::now() - Duration::from_secs(40 * 24 * 60 * 60);
    filetime::set_file_mtime(&old_file, filetime::FileTime::from_system_time(old_time)).unwrap();

    // Create new file (1 day old)
    let new_file = cache_dir.join("new-crate-2.0.0.crate");
    fs::write(&new_file, b"new content").unwrap();

    // Run clean_cargo_registry with mock home
    let config = Gc::builder()
        .target_dir(home.home().join("target"))
        .dry_run(false)
        .debug(false)
        .age_threshold_days(7)
        .build();

    let bytes_freed = config
        .clean_cargo_registry_with_home(&cargo_home, 0)
        .unwrap();

    // Verify old file is removed and new file is kept
    assert!(!old_file.exists(), "Old file should be removed");
    assert!(new_file.exists(), "New file should be kept");
    assert!(bytes_freed > 0, "Should have freed some bytes");
}

#[test]
fn test_clean_cargo_bin_with_mock_home() {
    // Create a temporary directory to act as cargo home
    let home = TempHomeGuard::new();
    let cargo_home = home.cargo_home();
    let bin_dir = cargo_home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    // Create old binary (40 days old) that should be removed
    let old_binary = bin_dir.join("old-tool");
    fs::write(&old_binary, b"#!/bin/sh\necho old").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&old_binary).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&old_binary, perms).unwrap();
    }
    let old_time = SystemTime::now() - Duration::from_secs(40 * 24 * 60 * 60);
    filetime::set_file_mtime(&old_binary, filetime::FileTime::from_system_time(old_time)).unwrap();

    // Create cargo binary (old but should be kept)
    let cargo_binary = bin_dir.join("cargo-test");
    fs::write(&cargo_binary, b"#!/bin/sh\necho cargo").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&cargo_binary).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cargo_binary, perms).unwrap();
    }
    filetime::set_file_mtime(
        &cargo_binary,
        filetime::FileTime::from_system_time(old_time),
    )
    .unwrap();

    // Create custom preserved binary (old but should be kept)
    let custom_binary = bin_dir.join("my-tool");
    fs::write(&custom_binary, b"#!/bin/sh\necho custom").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&custom_binary).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&custom_binary, perms).unwrap();
    }
    filetime::set_file_mtime(
        &custom_binary,
        filetime::FileTime::from_system_time(old_time),
    )
    .unwrap();

    // Run clean_cargo_bin with mock home
    let config = Gc::builder()
        .target_dir(home.home().join("target"))
        .dry_run(false)
        .debug(false)
        .age_threshold_days(7)
        .preserve_binary("my-tool")
        .build();

    let bytes_freed = config.clean_cargo_bin_with_home(&cargo_home, 0).unwrap();

    // Verify results
    assert!(!old_binary.exists(), "Old binary should be removed");
    assert!(cargo_binary.exists(), "Cargo binary should be preserved");
    assert!(
        custom_binary.exists(),
        "Custom preserved binary should be kept"
    );
    assert!(bytes_freed > 0, "Should have freed some bytes");
}

#[test]
fn test_gc_cleans_cargo_home_even_with_missing_target() {
    // This test verifies the behavior we fixed - that GC cleans ~/.cargo
    // even when the target directory doesn't exist
    let home = TempHomeGuard::new();
    let nonexistent_target = home.home().join("does_not_exist");

    let config = Gc::builder()
        .target_dir(nonexistent_target)
        .dry_run(false)
        .debug(false)
        .age_threshold_days(7)
        .build();

    // This should succeed and potentially clean ~/.cargo
    let stats = config.perform_gc(0).unwrap();

    // We can't assert specific bytes_freed because it depends on the actual
    // ~/.cargo state but we can verify the operation completes successfully
    assert_eq!(stats.initial_size, 0); // target dir doesn't exist
    assert_eq!(stats.final_size, 0); // target dir still doesn't exist
    assert_eq!(stats.artifacts_removed, 0); // no artifacts in nonexistent dir
    assert_eq!(stats.crates_cleaned, 0); // no crates in nonexistent dir
    // bytes_freed may be > 0 from cleaning ~/.cargo
}
