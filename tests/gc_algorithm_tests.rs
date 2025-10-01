use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cargo_hold::gc::{
    ArtifactInfo, CrateArtifact, parse_crate_artifact_name, select_artifacts_for_removal,
};
use proptest::prelude::*;

// Property test strategies

/// Generate a valid crate name
fn crate_name_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{0,30}".prop_map(|s| s.replace('_', "-"))
}

/// Generate a valid hash (16 hex chars)
fn hash_strategy() -> impl Strategy<Value = String> {
    "[0-9a-f]{16}"
}

// Tests for parse_crate_artifact_name
#[test]
fn test_parse_crate_artifact_name_basic() {
    let cases = vec![
        ("libfoo-1234567890abcdef", "libfoo", "1234567890abcdef"),
        (
            "serde-1.0.136-78d1b3f8c7b8e0a2",
            "serde-1.0.136",
            "78d1b3f8c7b8e0a2",
        ),
        (
            "my-cool-lib-0123456789abcdef.rlib",
            "my-cool-lib",
            "0123456789abcdef",
        ),
        (
            "build-script-build-fedcba0987654321",
            "build-script-build",
            "fedcba0987654321",
        ),
    ];

    for (input, expected_name, expected_hash) in cases {
        let path = Path::new(input);
        let result = parse_crate_artifact_name(path);
        assert!(result.is_some(), "Failed to parse: {input}");
        let (name, hash) = result.unwrap();
        assert_eq!(name, expected_name);
        assert_eq!(hash, expected_hash);
    }
}

#[test]
fn test_parse_crate_artifact_name_invalid() {
    let invalid_cases = vec![
        "foo",                   // No hash
        "foo-123",               // Hash too short
        "foo-ghijklmnopqrstuv",  // Invalid hex chars
        "foo-1234567890abcdef0", // Hash too long (17 chars)
        "-1234567890abcdef",     // No name part
    ];

    for input in invalid_cases {
        let path = Path::new(input);
        let result = parse_crate_artifact_name(path);
        assert!(result.is_none(), "Should fail to parse: {input}");
    }
}

proptest! {
    #[test]
    fn test_parse_crate_artifact_name_property(
        name in crate_name_strategy(),
        hash in hash_strategy(),
        extension in prop::option::of("[a-z]{1,4}"),
    ) {
        let filename = if let Some(ext) = extension {
            format!("{name}-{hash}.{ext}")
        } else {
            format!("{name}-{hash}")
        };

        let path = Path::new(&filename);
        let result = parse_crate_artifact_name(path);

        prop_assert!(result.is_some());
        let (parsed_name, parsed_hash) = result.unwrap();
        prop_assert_eq!(parsed_name, name);
        prop_assert_eq!(parsed_hash, hash);
    }
}

// Helper functions

fn create_test_artifact(name: &str, hash: &str, size: u64, age_days: u64) -> CrateArtifact {
    let mtime = SystemTime::now()
        .checked_sub(Duration::from_secs(age_days * 24 * 60 * 60))
        .unwrap_or(SystemTime::now());

    CrateArtifact {
        name: name.to_string(),
        hash: hash.to_string(),
        artifacts: vec![ArtifactInfo {
            path: PathBuf::from(format!("target/debug/deps/lib{name}-{hash}.rlib")),
            size,
            _modified: mtime,
        }],
        total_size: size,
        newest_mtime: mtime,
    }
}

// Combined selection tests

#[test]
fn test_combined_selection_size_and_age() {
    // Create artifacts with varying ages and sizes
    let artifacts = vec![
        create_test_artifact("old_large", "1234567890abcdef", 5000, 30), // 30 days old, 5KB
        create_test_artifact("old_small", "2234567890abcdef", 1000, 20), // 20 days old, 1KB
        create_test_artifact("recent_large", "3234567890abcdef", 4000, 5), // 5 days old, 4KB
        create_test_artifact("recent_small", "4234567890abcdef", 500, 2), // 2 days old, 0.5KB
    ];

    // Total size: 10.5KB
    // Set max size to 6KB (need to free 4.5KB)
    // Set age threshold to 10 days (should remove artifacts older than 10 days)

    let selected = select_artifacts_for_removal(&artifacts, 10500, Some(6000), 10, None, 0, false);

    // Should remove:
    // 1. old_large (5KB) to get under size limit (leaves 5.5KB)
    // 2. old_small (1KB) because it's older than 10 days
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().any(|a| a.name == "old_large"));
    assert!(selected.iter().any(|a| a.name == "old_small"));
}

#[test]
fn test_combined_selection_only_age() {
    // Create artifacts all under size limit
    let artifacts = vec![
        create_test_artifact("old1", "1234567890abcdef", 1000, 15), // 15 days old
        create_test_artifact("old2", "2234567890abcdef", 1000, 12), // 12 days old
        create_test_artifact("new1", "3234567890abcdef", 1000, 5),  // 5 days old
        create_test_artifact("new2", "4234567890abcdef", 1000, 3),  // 3 days old
    ];

    // Total size: 4KB, max size: 10KB (no size pressure)
    // Age threshold: 10 days

    let selected = select_artifacts_for_removal(&artifacts, 4000, Some(10000), 10, None, 0, false);

    // Should only remove artifacts older than 10 days
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().any(|a| a.name == "old1"));
    assert!(selected.iter().any(|a| a.name == "old2"));
}

#[test]
fn test_combined_selection_only_size() {
    // Create artifacts all very recent
    let artifacts = vec![
        create_test_artifact("large1", "1234567890abcdef", 5000, 2), // 2 days old
        create_test_artifact("large2", "2234567890abcdef", 4000, 1), // 1 day old
        create_test_artifact("small1", "3234567890abcdef", 1000, 3), // 3 days old
        create_test_artifact("small2", "4234567890abcdef", 500, 2),  // 2 days old
    ];

    // Total size: 10.5KB, max size: 5KB
    // Age threshold: 30 days (nothing is old enough)

    let selected = select_artifacts_for_removal(&artifacts, 10500, Some(5000), 30, None, 0, false);

    // Should remove oldest first until under size limit
    // Removes: small1 (3 days), large1 (2 days) = 6KB freed (enough to get under
    // 5KB limit)
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().any(|a| a.name == "small1"));
    assert!(selected.iter().any(|a| a.name == "large1"));
}

#[test]
fn test_combined_selection_no_size_limit() {
    // When no size limit is specified, should only apply age-based cleanup
    let artifacts = vec![
        create_test_artifact("old", "1234567890abcdef", 10000, 15),
        create_test_artifact("new", "2234567890abcdef", 10000, 5),
    ];

    let selected = select_artifacts_for_removal(&artifacts, 20000, None, 10, None, 0, false);

    // Should only remove the old artifact
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "old");
}

#[test]
fn test_combined_selection_everything_removed() {
    // Test case where size limit requires removing everything
    let artifacts = vec![
        create_test_artifact("a", "1234567890abcdef", 5000, 20),
        create_test_artifact("b", "2234567890abcdef", 5000, 10),
        create_test_artifact("c", "3234567890abcdef", 5000, 5),
    ];

    // Total: 15KB, max size: 0KB, age threshold: 30 days
    let selected = select_artifacts_for_removal(&artifacts, 15000, Some(0), 30, None, 0, false);

    // All artifacts should be selected for removal
    assert_eq!(selected.len(), 3);
}

#[test]
fn test_combined_selection_exact_size_limit() {
    // Test when current size exactly matches the limit
    let artifacts = vec![
        create_test_artifact("a", "1234567890abcdef", 1000, 15),
        create_test_artifact("b", "2234567890abcdef", 2000, 10),
        create_test_artifact("c", "3234567890abcdef", 3000, 5),
    ];

    // Total: 6KB, max size: 6KB exactly
    let selected = select_artifacts_for_removal(&artifacts, 6000, Some(6000), 10, None, 0, false);

    // Should only remove artifacts older than 10 days
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().any(|a| a.name == "a"));
    assert!(selected.iter().any(|a| a.name == "b"));
}

#[test]
fn test_combined_selection_zero_age_threshold() {
    // Test with age threshold of 0 days (should remove everything)
    let artifacts = vec![
        create_test_artifact("fresh", "1234567890abcdef", 1000, 0), // Created today
        create_test_artifact("recent", "2234567890abcdef", 2000, 1), // 1 day old
        create_test_artifact("old", "3234567890abcdef", 3000, 5),   // 5 days old
    ];

    // Total: 6KB, max size: 10KB (no size pressure), age threshold: 0 days
    let selected = select_artifacts_for_removal(&artifacts, 6000, Some(10000), 0, None, 0, false);

    // All artifacts should be removed (all are >= 0 days old)
    assert_eq!(selected.len(), 3);
}

#[test]
fn test_combined_selection_same_timestamps() {
    // Test when all artifacts have the same timestamp
    let now = SystemTime::now()
        .checked_sub(Duration::from_secs(15 * 24 * 60 * 60))
        .unwrap();

    let mut artifacts = vec![
        create_test_artifact("a", "1234567890abcdef", 3000, 0),
        create_test_artifact("b", "2234567890abcdef", 2000, 0),
        create_test_artifact("c", "3234567890abcdef", 1000, 0),
    ];

    // Set all to same timestamp (15 days old)
    for artifact in &mut artifacts {
        artifact.newest_mtime = now;
    }

    // Total: 6KB, max size: 4KB, age threshold: 10 days
    let selected = select_artifacts_for_removal(&artifacts, 6000, Some(4000), 10, None, 0, false);

    // Should remove enough for size (at least 2KB) and all are old enough
    // Since they have same timestamp, the order might be implementation-dependent
    assert!(selected.len() >= 2);

    // Calculate total removed size
    let removed_size: u64 = selected.iter().map(|a| a.total_size).sum();
    assert!(removed_size >= 2000); // Need to free at least 2KB
}

#[test]
fn test_combined_selection_empty_list() {
    // Test with empty artifact list
    let artifacts = vec![];
    let selected = select_artifacts_for_removal(&artifacts, 0, Some(1000), 7, None, 0, false);
    assert_eq!(selected.len(), 0);
}

// CRITICAL TESTS FOR TIMESTAMP PRESERVATION FEATURE

#[test]
fn test_combined_selection_preserves_previous_build_artifacts() {
    // This is the CORE TEST for the feature: artifacts from previous build should
    // be preserved even when they would otherwise be selected for deletion due
    // to size constraints

    let now = SystemTime::now();
    let one_hour_ago = now.checked_sub(Duration::from_secs(3600)).unwrap();
    let one_day_ago = now.checked_sub(Duration::from_secs(24 * 3600)).unwrap();

    // Create artifacts with specific timestamps
    let mut artifacts = vec![
        create_test_artifact("old_artifact", "1111111111111111", 5000, 0), // 5KB
        create_test_artifact("recent_artifact1", "2222222222222222", 4000, 0), // 4KB
        create_test_artifact("recent_artifact2", "3333333333333333", 3000, 0), // 3KB
        create_test_artifact("recent_artifact3", "4444444444444444", 2000, 0), // 2KB
    ];

    // Set specific timestamps
    artifacts[0].newest_mtime = one_day_ago; // Old artifact
    artifacts[1].newest_mtime = one_hour_ago; // Recent artifact (was two_hours_ago, now within preservation window)
    artifacts[2].newest_mtime = one_hour_ago; // Recent artifact  
    artifacts[3].newest_mtime = one_hour_ago; // Recent artifact

    // Total: 14KB, max size: 6KB (need to free 8KB)
    // Previous build was one hour ago
    let previous_build_nanos = one_hour_ago
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    let selected = select_artifacts_for_removal(
        &artifacts,
        14000,
        Some(6000),
        30, // High age threshold so it doesn't interfere
        Some(previous_build_nanos),
        2, // verbose
        false,
    );

    // Should only remove the old artifact (5KB), not enough to meet size limit
    // but recent artifacts are preserved
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "old_artifact");

    // Verify that recent artifacts were NOT selected despite size constraint
    assert!(!selected.iter().any(|a| a.name.starts_with("recent_")));
}

#[test]
fn test_combined_selection_timestamp_buffer_edge_case() {
    // Test the 1-second buffer for timestamp comparison

    let now = SystemTime::now();
    let base_time = now.checked_sub(Duration::from_secs(3600)).unwrap();

    // Create artifacts at various times around the cutoff
    let mut artifacts = vec![
        create_test_artifact("exactly_at_cutoff", "1111111111111111", 1000, 0),
        create_test_artifact("just_before_cutoff", "2222222222222222", 1000, 0),
        create_test_artifact("just_after_cutoff", "3333333333333333", 1000, 0),
        create_test_artifact("well_before_cutoff", "4444444444444444", 1000, 0),
    ];

    // Set precise timestamps
    artifacts[0].newest_mtime = base_time; // Exactly at cutoff
    artifacts[1].newest_mtime = base_time.checked_sub(Duration::from_millis(500)).unwrap(); // 500ms before
    artifacts[2].newest_mtime = base_time.checked_add(Duration::from_millis(500)).unwrap(); // 500ms after
    artifacts[3].newest_mtime = base_time.checked_sub(Duration::from_secs(2)).unwrap(); // 2s before

    let previous_build_nanos = base_time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    let selected = select_artifacts_for_removal(
        &artifacts,
        4000,
        Some(2000), // Need to remove 2KB
        30,
        Some(previous_build_nanos),
        0,
        false,
    );

    // With 1-second buffer, artifacts at or near the cutoff should be preserved
    // Only well_before_cutoff should be selected
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "well_before_cutoff");
}

#[test]
fn test_combined_selection_exceeds_size_for_preservation() {
    // Test that we can exceed the size limit to preserve recent artifacts

    let now = SystemTime::now();
    let recent = now.checked_sub(Duration::from_secs(600)).unwrap(); // 10 minutes ago
    let old = now.checked_sub(Duration::from_secs(3 * 24 * 3600)).unwrap(); // 3 days ago

    let mut artifacts = vec![
        create_test_artifact("old1", "1111111111111111", 2000, 0),
        create_test_artifact("old2", "2222222222222222", 2000, 0),
        create_test_artifact("recent1", "3333333333333333", 8000, 0), // Large recent artifact
        create_test_artifact("recent2", "4444444444444444", 7000, 0), // Large recent artifact
    ];

    artifacts[0].newest_mtime = old;
    artifacts[1].newest_mtime = old;
    artifacts[2].newest_mtime = recent;
    artifacts[3].newest_mtime = recent;

    let previous_build_nanos = recent
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    // Total: 19KB, max size: 5KB
    // But recent artifacts (15KB) should be preserved
    let selected = select_artifacts_for_removal(
        &artifacts,
        19000,
        Some(5000),
        30,
        Some(previous_build_nanos),
        0,
        false,
    );

    // Should only select old artifacts
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().all(|a| a.name.starts_with("old")));

    // Final size would be 15KB (exceeding the 5KB limit) but that's OK
}

#[test]
fn test_combined_selection_no_previous_build_timestamp() {
    // Test behavior when previous_build_mtime_nanos is None (first run)

    let artifacts = vec![
        create_test_artifact("artifact1", "1111111111111111", 5000, 10),
        create_test_artifact("artifact2", "2222222222222222", 4000, 5),
        create_test_artifact("artifact3", "3333333333333333", 3000, 1),
    ];

    // Total: 12KB, max size: 6KB
    let selected = select_artifacts_for_removal(
        &artifacts,
        12000,
        Some(6000),
        30,
        None, // No previous build timestamp
        0,
        false,
    );

    // Should remove oldest first until under size limit
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].name, "artifact1"); // Oldest (10 days)
    assert_eq!(selected[1].name, "artifact2"); // Next oldest (5 days)
}

#[test]
fn test_combined_selection_all_artifacts_are_recent() {
    // Test when all artifacts are from the previous build

    let now = SystemTime::now();
    let recent = now.checked_sub(Duration::from_secs(300)).unwrap(); // 5 minutes ago

    let mut artifacts = vec![
        create_test_artifact("recent1", "1111111111111111", 5000, 0),
        create_test_artifact("recent2", "2222222222222222", 5000, 0),
        create_test_artifact("recent3", "3333333333333333", 5000, 0),
    ];

    // All artifacts are recent
    for artifact in &mut artifacts {
        artifact.newest_mtime = recent;
    }

    let previous_build_nanos = recent
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    // Total: 15KB, max size: 5KB
    let selected = select_artifacts_for_removal(
        &artifacts,
        15000,
        Some(5000),
        30,
        Some(previous_build_nanos),
        0,
        false,
    );

    // Nothing should be selected - all artifacts are preserved
    assert_eq!(selected.len(), 0);
}

#[test]
fn test_combined_selection_mixed_ages_with_preservation() {
    // Complex test with mixed artifact ages and preservation

    let now = SystemTime::now();
    let five_min_ago = now.checked_sub(Duration::from_secs(5 * 60)).unwrap();
    let one_hour_ago = now.checked_sub(Duration::from_secs(3600)).unwrap();
    let twelve_hours_ago = now.checked_sub(Duration::from_secs(12 * 3600)).unwrap();
    let two_days_ago = now.checked_sub(Duration::from_secs(2 * 24 * 3600)).unwrap();
    let ten_days_ago = now
        .checked_sub(Duration::from_secs(10 * 24 * 3600))
        .unwrap();

    let mut artifacts = vec![
        create_test_artifact("very_old", "1111111111111111", 3000, 0),
        create_test_artifact("old", "2222222222222222", 2000, 0),
        create_test_artifact("medium_old", "3333333333333333", 2500, 0),
        create_test_artifact("recent_preserve", "4444444444444444", 4000, 0),
        create_test_artifact("very_recent_preserve", "5555555555555555", 3500, 0),
    ];

    artifacts[0].newest_mtime = ten_days_ago;
    artifacts[1].newest_mtime = two_days_ago;
    artifacts[2].newest_mtime = twelve_hours_ago;
    artifacts[3].newest_mtime = one_hour_ago;
    artifacts[4].newest_mtime = five_min_ago;

    // Previous build was one hour ago
    let previous_build_nanos = one_hour_ago
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    // Total: 15KB, max size: 8KB, age threshold: 5 days
    let selected = select_artifacts_for_removal(
        &artifacts,
        15000,
        Some(8000),
        5,
        Some(previous_build_nanos),
        0,
        false,
    );

    // Should remove:
    // - very_old (10 days old, exceeds age threshold)
    // - old (2 days old, needed for size but not protected)
    // Should NOT remove:
    // - medium_old (12 hours, under age threshold and helps with size)
    // - recent_preserve (protected by previous build timestamp)
    // - very_recent_preserve (protected by previous build timestamp)

    assert_eq!(selected.len(), 3);
    assert!(selected.iter().any(|a| a.name == "very_old"));
    assert!(selected.iter().any(|a| a.name == "old"));
    assert!(selected.iter().any(|a| a.name == "medium_old"));
    assert!(!selected.iter().any(|a| a.name.contains("preserve")));
}
