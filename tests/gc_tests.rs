use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cargo_hold::gc::{self, Gc};
use tempfile::TempDir;

#[test]
fn test_gc_config_builder() {
    // Test default builder
    let config = Gc::builder().build();
    assert_eq!(config.target_dir(), Path::new("target"));
    assert_eq!(config.max_target_size(), None);
    assert!(!config.dry_run());
    assert!(!config.debug());
    assert_eq!(config.age_threshold_days(), 7);
    assert!(config.preserve_binaries().is_empty());
    assert_eq!(config.previous_build_mtime_nanos(), None);

    // Test builder with all options
    let config = Gc::builder()
        .target_dir("/custom/target")
        .max_target_size(1024 * 1024 * 1024) // 1GB
        .dry_run(true)
        .debug(true)
        .age_threshold_days(14)
        .preserve_binary("cargo-hold")
        .preserve_binary("cargo-test")
        .previous_build_mtime_nanos(123456789)
        .build();

    assert_eq!(config.target_dir(), Path::new("/custom/target"));
    assert_eq!(config.max_target_size(), Some(1024 * 1024 * 1024));
    assert!(config.dry_run());
    assert!(config.debug());
    assert_eq!(config.age_threshold_days(), 14);
    assert_eq!(config.preserve_binaries(), &["cargo-hold", "cargo-test"]);
    assert_eq!(config.previous_build_mtime_nanos(), Some(123456789));
}

/// Helper to create a file with specific size and modification time
fn create_file_with_mtime(path: &Path, size: usize, age_days: u32) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Create file with specific size
    let content = vec![b'x'; size];
    fs::write(path, content)?;

    // Set modification time
    if age_days > 0 {
        let mtime = SystemTime::now() - Duration::from_secs(age_days as u64 * 24 * 60 * 60);
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(mtime))?;
    }

    Ok(())
}

/// Helper to create a typical Rust target directory structure
fn setup_target_dir(temp_dir: &TempDir) -> PathBuf {
    let target_dir = temp_dir.path().join("target");

    // Create profile directories
    let debug_dir = target_dir.join("debug");
    fs::create_dir_all(&debug_dir).unwrap();

    // Create standard subdirectories
    fs::create_dir_all(debug_dir.join("deps")).unwrap();
    fs::create_dir_all(debug_dir.join("build")).unwrap();
    fs::create_dir_all(debug_dir.join(".fingerprint")).unwrap();
    fs::create_dir_all(debug_dir.join("incremental")).unwrap();

    target_dir
}

/// Helper to create crate artifacts in a profile directory
fn create_crate_artifacts(
    profile_dir: &Path,
    crate_name: &str,
    hash: &str,
    size_kb: usize,
    age_days: u32,
) {
    // Create fingerprint directory with lib prefix to match deps artifacts
    let fingerprint_dir = profile_dir
        .join(".fingerprint")
        .join(format!("lib{crate_name}-{hash}"));
    fs::create_dir_all(&fingerprint_dir).unwrap();

    // Set the directory mtime
    if age_days > 0 {
        let mtime = SystemTime::now() - Duration::from_secs(age_days as u64 * 24 * 60 * 60);
        filetime::set_file_mtime(
            &fingerprint_dir,
            filetime::FileTime::from_system_time(mtime),
        )
        .unwrap();
    }

    create_file_with_mtime(&fingerprint_dir.join("invoked.timestamp"), 128, age_days).unwrap();
    create_file_with_mtime(&fingerprint_dir.join("dep-lib"), 256, age_days).unwrap();

    // Create deps artifacts
    let deps_dir = profile_dir.join("deps");
    create_file_with_mtime(
        &deps_dir.join(format!("lib{crate_name}-{hash}.rlib")),
        size_kb * 1024,
        age_days,
    )
    .unwrap();
    create_file_with_mtime(
        &deps_dir.join(format!("lib{crate_name}-{hash}.d")),
        1024,
        age_days,
    )
    .unwrap();

    // Create build artifacts
    let build_dir = profile_dir
        .join("build")
        .join(format!("{crate_name}-{hash}"));
    fs::create_dir_all(&build_dir).unwrap();

    // Set the directory mtime
    if age_days > 0 {
        let mtime = SystemTime::now() - Duration::from_secs(age_days as u64 * 24 * 60 * 60);
        filetime::set_file_mtime(&build_dir, filetime::FileTime::from_system_time(mtime)).unwrap();
    }

    create_file_with_mtime(&build_dir.join("out"), 2048, age_days).unwrap();
}

#[test]
fn test_parse_size() {
    // Test raw numbers
    assert_eq!(gc::parse_size("100").unwrap(), 100);
    assert_eq!(gc::parse_size("1024").unwrap(), 1024);

    // Test with suffixes
    assert_eq!(gc::parse_size("1K").unwrap(), 1024);
    assert_eq!(gc::parse_size("1KB").unwrap(), 1024);
    assert_eq!(gc::parse_size("1KiB").unwrap(), 1024);
    assert_eq!(gc::parse_size("2M").unwrap(), 2 * 1024 * 1024);
    assert_eq!(gc::parse_size("2MB").unwrap(), 2 * 1024 * 1024);
    assert_eq!(gc::parse_size("2MiB").unwrap(), 2 * 1024 * 1024);
    assert_eq!(gc::parse_size("3G").unwrap(), 3 * 1024 * 1024 * 1024);
    assert_eq!(gc::parse_size("3GB").unwrap(), 3 * 1024 * 1024 * 1024);
    assert_eq!(gc::parse_size("3GiB").unwrap(), 3 * 1024 * 1024 * 1024);

    // Test decimal values
    assert_eq!(
        gc::parse_size("1.5G").unwrap(),
        (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
    );
    assert_eq!(gc::parse_size("0.5M").unwrap(), 512 * 1024);

    // Test edge cases
    assert_eq!(gc::parse_size("0").unwrap(), 0);
    assert_eq!(gc::parse_size("0B").unwrap(), 0);

    // Test error cases
    assert!(gc::parse_size("").is_err());
    assert!(gc::parse_size("abc").is_err());
    assert!(gc::parse_size("100X").is_err());
}

#[test]
fn test_format_size() {
    assert_eq!(gc::format_size(0), "0 B");
    assert_eq!(gc::format_size(100), "100 B");
    assert_eq!(gc::format_size(1023), "1023 B");
    assert_eq!(gc::format_size(1024), "1.0 KiB");
    assert_eq!(gc::format_size(1536), "1.5 KiB");
    assert_eq!(gc::format_size(1024 * 1024), "1.0 MiB");
    assert_eq!(gc::format_size(1024 * 1024 * 1024), "1.0 GiB");
    assert_eq!(gc::format_size(1024_u64.pow(4)), "1.0 TiB");

    // Test large values
    assert_eq!(
        gc::format_size(5 * 1024 * 1024 * 1024 + 512 * 1024 * 1024),
        "5.5 GiB"
    );
}

#[test]
fn test_gc_age_based_cleanup() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = setup_target_dir(&temp_dir);

    // Create old and new crate artifacts
    let debug_dir = target_dir.join("debug");
    create_crate_artifacts(&debug_dir, "old-crate", "1234567890abcdef", 1024, 10); // 10 days old
    create_crate_artifacts(&debug_dir, "new-crate", "fedcba0987654321", 2048, 2); // 2 days old

    // Verify the files were created with the correct age
    let test_file = debug_dir
        .join("deps")
        .join("libold-crate-1234567890abcdef.rlib");
    if let Ok(metadata) = fs::metadata(&test_file)
        && let Ok(mtime) = metadata.modified()
    {
        let age = SystemTime::now().duration_since(mtime).unwrap();
        eprintln!(
            "Test file age: {} seconds ({} days)",
            age.as_secs(),
            age.as_secs() / (24 * 60 * 60)
        );
    }

    // Run GC with 7-day threshold
    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .dry_run(false)
        .debug(true) // Enable debug for more output
        .age_threshold_days(7)
        .build();

    let stats = config.perform_gc(2).unwrap(); // Use verbose=2 for debugging

    // Debug output
    eprintln!(
        "Stats: bytes_freed={}, crates_cleaned={}, artifacts_removed={}",
        stats.bytes_freed, stats.crates_cleaned, stats.artifacts_removed
    );

    // List contents of deps directory
    if let Ok(entries) = fs::read_dir(debug_dir.join("deps")) {
        eprintln!("Contents of deps directory after GC:");
        for entry in entries.flatten() {
            eprintln!("  - {:?}", entry.file_name());
        }
    }

    // List contents of fingerprint directory
    if let Ok(entries) = fs::read_dir(debug_dir.join(".fingerprint")) {
        eprintln!("Contents of .fingerprint directory after GC:");
        for entry in entries.flatten() {
            eprintln!("  - {:?}", entry.file_name());
        }
    }

    // Old crate should be removed
    assert!(
        stats.bytes_freed > 0,
        "Expected bytes_freed > 0, got {}",
        stats.bytes_freed
    );
    assert!(
        stats.crates_cleaned >= 1,
        "Expected at least 1 crate cleaned, got {}",
        stats.crates_cleaned
    );

    // Debug: Check if files exist
    let rlib_exists = debug_dir
        .join("deps")
        .join("libold-crate-1234567890abcdef.rlib")
        .exists();
    let d_file_exists = debug_dir
        .join("deps")
        .join("libold-crate-1234567890abcdef.d")
        .exists();
    let fingerprint_exists = debug_dir
        .join(".fingerprint")
        .join("libold-crate-1234567890abcdef")
        .exists();
    eprintln!(
        "After GC: rlib exists={rlib_exists}, d_file exists={d_file_exists}, fingerprint \
         exists={fingerprint_exists}"
    );

    // Verify old crate artifacts are gone
    assert!(!rlib_exists, "Old rlib should be removed");
    assert!(!d_file_exists, "Old .d file should be removed");
    assert!(!fingerprint_exists, "Old fingerprint should be removed");

    // Verify new crate still exists
    assert!(
        debug_dir
            .join("deps")
            .join("libnew-crate-fedcba0987654321.rlib")
            .exists()
    );
    assert!(
        debug_dir
            .join(".fingerprint")
            .join("libnew-crate-fedcba0987654321")
            .exists()
    );
}

#[test]
fn test_gc_removes_artifacts_with_stale_previous_timestamp() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = setup_target_dir(&temp_dir);
    let debug_dir = target_dir.join("debug");

    create_crate_artifacts(&debug_dir, "stale-crate", "1234567890abcdef", 512, 10);
    create_crate_artifacts(&debug_dir, "fresh-crate", "fedcba0987654321", 512, 2);

    let stale_previous = SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
    let stale_nanos = stale_previous
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .dry_run(false)
        .age_threshold_days(7)
        .previous_build_mtime_nanos(stale_nanos)
        .build();

    let stats = config.perform_gc(1).unwrap();

    let deps_dir = debug_dir.join("deps");
    let stale_artifact = deps_dir.join("libstale-crate-1234567890abcdef.rlib");
    let fresh_artifact = deps_dir.join("libfresh-crate-fedcba0987654321.rlib");

    assert!(stats.bytes_freed > 0, "Expected GC to free bytes");
    assert!(!stale_artifact.exists(), "Stale artifact should be removed");
    assert!(fresh_artifact.exists(), "Recent artifact should remain");
}

#[test]
fn test_gc_size_based_cleanup() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = setup_target_dir(&temp_dir);

    // Create multiple crate artifacts with different ages
    let debug_dir = target_dir.join("debug");
    create_crate_artifacts(&debug_dir, "oldest", "1111111111111111", 500, 10);
    create_crate_artifacts(&debug_dir, "middle", "2222222222222222", 500, 5);
    create_crate_artifacts(&debug_dir, "newest", "3333333333333333", 500, 1);

    // Run GC with size limit that should remove oldest crate
    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .max_target_size(1024 * 1024) // 1 MB limit
        .dry_run(false)
        .debug(true) // Enable debug to see what's happening
        .age_threshold_days(30) // High threshold so age doesn't matter
        .build();

    eprintln!("Running size-based GC with 1MB limit");
    let stats = config.perform_gc(2).unwrap(); // Verbose output

    // Should remove at least one crate
    assert!(
        stats.bytes_freed > 0,
        "Expected bytes_freed > 0, got {}",
        stats.bytes_freed
    );
    assert!(
        stats.crates_cleaned >= 1,
        "Expected at least 1 crate cleaned, got {}",
        stats.crates_cleaned
    );

    // Oldest should be removed first
    assert!(
        !debug_dir
            .join("deps")
            .join("liboldest-1111111111111111.rlib")
            .exists()
    );
}

#[test]
fn test_gc_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = setup_target_dir(&temp_dir);

    // Create artifacts
    let debug_dir = target_dir.join("debug");
    create_crate_artifacts(&debug_dir, "test-crate", "abcdef1234567890", 1024, 10);

    // Run GC in dry-run mode
    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .dry_run(true)
        .debug(false)
        .age_threshold_days(7)
        .build();

    let stats = config.perform_gc(0).unwrap();

    // Stats should show what would be cleaned
    assert!(stats.bytes_freed > 0);
    assert!(stats.crates_cleaned >= 1); // May be 2 if both lib and non-lib artifacts are found

    // But files should still exist
    assert!(
        debug_dir
            .join("deps")
            .join("libtest-crate-abcdef1234567890.rlib")
            .exists()
    );
}

#[test]
fn test_gc_incremental_cleanup() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = setup_target_dir(&temp_dir);

    // Create incremental compilation data
    let incremental_dir = target_dir.join("debug").join("incremental");
    let session_dir = incremental_dir.join("myproject-1234");
    fs::create_dir_all(&session_dir).unwrap();
    create_file_with_mtime(&session_dir.join("s-1234-working.bin"), 1024 * 1024, 0).unwrap();

    // Run GC
    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .dry_run(false)
        .debug(false)
        .age_threshold_days(30)
        .build();

    let stats = config.perform_gc(0).unwrap();

    // Incremental data should be removed
    assert!(stats.bytes_freed >= 1024 * 1024);
    assert!(!incremental_dir.exists());
}

#[test]
fn test_gc_misc_directories() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");

    // Create misc directories
    let doc_dir = target_dir.join("doc");
    fs::create_dir_all(&doc_dir).unwrap();
    create_file_with_mtime(&doc_dir.join("index.html"), 10240, 0).unwrap();

    let package_dir = target_dir.join("package");
    fs::create_dir_all(&package_dir).unwrap();
    create_file_with_mtime(&package_dir.join("myapp-0.1.0.crate"), 50000, 0).unwrap();

    let tmp_dir = target_dir.join("tmp");
    fs::create_dir_all(&tmp_dir).unwrap();
    create_file_with_mtime(&tmp_dir.join("tempfile"), 1000, 0).unwrap();

    // Run GC
    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .dry_run(false)
        .debug(false)
        .age_threshold_days(30)
        .build();

    let stats = config.perform_gc(0).unwrap();

    // All misc directories should be removed
    assert!(stats.bytes_freed > 60000);
    assert!(!doc_dir.exists());
    assert!(!package_dir.exists());
    assert!(!tmp_dir.exists());
}

#[test]
fn test_gc_preserve_binaries() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = setup_target_dir(&temp_dir);

    // Create binary files in debug directory
    let debug_dir = target_dir.join("debug");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Create executable binaries
        let bin1 = debug_dir.join("myapp");
        fs::write(&bin1, b"binary content").unwrap();
        let mut perms = fs::metadata(&bin1).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin1, perms).unwrap();

        let bin2 = debug_dir.join("test-runner");
        fs::write(&bin2, b"test binary").unwrap();
        let mut perms = fs::metadata(&bin2).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin2, perms).unwrap();
    }

    #[cfg(windows)]
    {
        // Create .exe files on Windows
        fs::write(debug_dir.join("myapp.exe"), b"binary content").unwrap();
        fs::write(debug_dir.join("test-runner.exe"), b"test binary").unwrap();
    }

    // Create some old artifacts
    create_crate_artifacts(&debug_dir, "old-crate", "1234567890abcdef", 1024, 10);

    // Run GC
    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .dry_run(false)
        .debug(false)
        .age_threshold_days(7)
        .build();

    let stats = config.perform_gc(0).unwrap();

    // Binaries should be preserved
    #[cfg(unix)]
    {
        assert!(debug_dir.join("myapp").exists());
        assert!(debug_dir.join("test-runner").exists());
    }

    #[cfg(windows)]
    {
        assert!(debug_dir.join("myapp.exe").exists());
        assert!(debug_dir.join("test-runner.exe").exists());
    }

    // Old crate should be removed
    assert!(
        !debug_dir
            .join("deps")
            .join("libold-crate-1234567890abcdef.rlib")
            .exists()
    );
    assert_eq!(stats.binaries_preserved, 2);
}

#[test]
fn test_gc_empty_target_dir() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("nonexistent");

    // Run GC on non-existent directory
    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .dry_run(false)
        .debug(false)
        .age_threshold_days(7)
        .build();

    let stats = config.perform_gc(0).unwrap();

    // Should succeed even with non-existent target directory
    // The target-specific stats should be zero
    assert_eq!(stats.artifacts_removed, 0);
    assert_eq!(stats.crates_cleaned, 0);
    assert_eq!(stats.initial_size, 0);
    assert_eq!(stats.final_size, 0);

    // bytes_freed can be non-zero from cleaning ~/.cargo directories
    // This is expected behavior - GC cleans global cargo cache regardless
    // of whether the project target directory exists
    // Just verify the operation completed successfully (stats were returned)
}

#[test]
fn test_gc_already_under_size_limit() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = setup_target_dir(&temp_dir);

    // Create small artifacts
    let debug_dir = target_dir.join("debug");
    create_crate_artifacts(&debug_dir, "small-crate", "1234567890abcdef", 10, 0);

    // Run GC with high size limit
    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .max_target_size(10 * 1024 * 1024 * 1024) // 10 GB limit
        .dry_run(false)
        .debug(false)
        .age_threshold_days(30)
        .build();

    let stats = config.perform_gc(0).unwrap();

    // Target artifacts should remain untouched (global cargo cleanup may still run)
    assert_eq!(stats.artifacts_removed, 0);
    assert_eq!(stats.crates_cleaned, 0);

    // Files should still exist
    assert!(
        debug_dir
            .join("deps")
            .join("libsmall-crate-1234567890abcdef.rlib")
            .exists()
    );
}

#[test]
fn test_cargo_registry_cleanup() {
    // Skip this test if we can't determine home directory
    let Some(_home_dir) = home::home_dir() else {
        return;
    };

    let temp_cargo = TempDir::new().unwrap();
    let cargo_dir = temp_cargo.path();

    // Create mock cargo directories
    let registry_cache = cargo_dir
        .join("registry")
        .join("cache")
        .join("github.com-1234");
    fs::create_dir_all(&registry_cache).unwrap();
    create_file_with_mtime(&registry_cache.join("old-crate-1.0.0.crate"), 100000, 10).unwrap();
    create_file_with_mtime(&registry_cache.join("new-crate-2.0.0.crate"), 200000, 2).unwrap();

    // Create git checkouts
    let git_checkouts = cargo_dir.join("git").join("checkouts");
    fs::create_dir_all(&git_checkouts).unwrap();
    let old_checkout = git_checkouts.join("old-repo-1234567890");
    fs::create_dir_all(&old_checkout).unwrap();
    create_file_with_mtime(&old_checkout.join("Cargo.toml"), 1000, 10).unwrap();

    // Note: We can't easily test the actual cargo cleanup without mocking the
    // home directory This would require more complex test setup
}

#[test]
fn test_multiple_profile_directories() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");

    // Create multiple profile directories
    for profile in &["debug", "release", "test", "bench"] {
        let profile_dir = target_dir.join(profile);
        fs::create_dir_all(&profile_dir).unwrap();

        // Add standard subdirectories
        fs::create_dir_all(profile_dir.join("deps")).unwrap();
        fs::create_dir_all(profile_dir.join("build")).unwrap();
        fs::create_dir_all(profile_dir.join(".fingerprint")).unwrap();

        // Add old crate artifacts
        create_crate_artifacts(
            &profile_dir,
            &format!("{profile}-crate"),
            "1234567890abcdef",
            500,
            10,
        );
    }

    // Run GC
    let config = Gc::builder()
        .target_dir(target_dir.clone())
        .dry_run(false)
        .debug(false)
        .age_threshold_days(7)
        .build();

    let stats = config.perform_gc(0).unwrap();

    // Should clean all profile directories
    assert!(stats.crates_cleaned >= 4); // May be 8 if both lib and non-lib artifacts are found
    assert!(stats.bytes_freed > 0);

    // Verify artifacts are removed from all profiles
    for profile in &["debug", "release", "test", "bench"] {
        let profile_dir = target_dir.join(profile);
        assert!(
            !profile_dir
                .join("deps")
                .join(format!("lib{profile}-crate-1234567890abcdef.rlib"))
                .exists()
        );
    }
}

#[test]
fn test_gc_with_custom_preserve_binaries() {
    // This test would require mocking the home directory to test cargo bin
    // cleanup with custom preserve_binaries list. Skipping for now as it
    // requires complex setup.
}
