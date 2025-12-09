use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use filetime::FileTime;
use tempfile::TempDir;

use super::*;
use crate::commands::heave::{
    MAX_GROWTH_FACTOR_PER_RUN_PCT, MAX_SHRINK_FACTOR_PER_RUN_PCT, suggest_max_target_size,
};
use crate::metadata::{load_metadata, save_metadata};
use crate::state::{GcMetrics, METADATA_VERSION, StateMetadata};

fn setup_git_repo() -> TempDir {
    let temp_dir = TempDir::new().unwrap();

    // Initialize git repo
    let repo = git2::Repository::init(temp_dir.path()).unwrap();

    // Create and add a test file
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "test content").unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new("test.txt")).unwrap();
    index.write().unwrap();

    temp_dir
}

#[test]
fn test_stow_command() {
    let temp_dir = setup_git_repo();
    let metadata_path = temp_dir.path().join("test.metadata");

    stow(&metadata_path, 0, false, temp_dir.path()).unwrap();
    assert!(metadata_path.exists());
    let metadata = load_metadata(&metadata_path).unwrap();
    assert_eq!(metadata.len(), 1);
}

#[test]
fn test_stow_from_subdirectory() {
    let temp_dir = setup_git_repo();

    // Create a subdirectory
    let subdir = temp_dir.path().join("subdir");
    fs::create_dir(&subdir).unwrap();

    // Create metadata path in parent directory
    let metadata_path = temp_dir.path().join("test.metadata");

    // Run stow from subdirectory - it should find the parent git repo
    stow(&metadata_path, 0, false, &subdir).unwrap();
    assert!(metadata_path.exists());
    let metadata = load_metadata(&metadata_path).unwrap();
    assert_eq!(metadata.len(), 1);
}

#[test]
fn test_salvage_from_subdirectory() {
    let temp_dir = setup_git_repo();

    // Create a subdirectory
    let subdir = temp_dir.path().join("src");
    fs::create_dir(&subdir).unwrap();

    let metadata_path = temp_dir.path().join("test.metadata");

    // First stow from the root
    stow(&metadata_path, 0, false, temp_dir.path()).unwrap();

    // Now run salvage from subdirectory
    salvage(&metadata_path, 0, false, &subdir).unwrap();
}

#[test]
fn test_bilge_command() {
    let temp_dir = setup_git_repo();
    let metadata_path = temp_dir.path().join("test.metadata");

    // Create metadata first
    stow(&metadata_path, 0, false, temp_dir.path()).unwrap();
    assert!(metadata_path.exists());

    // Bilge it
    bilge(&metadata_path, 0, false).unwrap();
    assert!(!metadata_path.exists());
}

#[test]
fn test_anchor_command() {
    let temp_dir = setup_git_repo();
    let metadata_path = temp_dir.path().join("test.metadata");

    // Run anchor
    anchor(&metadata_path, 0, false, temp_dir.path()).unwrap();

    // Metadata should exist
    assert!(metadata_path.exists());
    let metadata = load_metadata(&metadata_path).unwrap();
    assert_eq!(metadata.len(), 1);
}

#[test]
fn test_stow_propagates_future_metadata_error() {
    let temp_dir = setup_git_repo();
    let metadata_path = temp_dir.path().join("test.metadata");

    // Persist metadata with a future format version
    let mut metadata = StateMetadata::new();
    metadata.version = METADATA_VERSION + 1;
    save_metadata(&metadata, &metadata_path).unwrap();

    let err = stow(&metadata_path, 0, false, temp_dir.path()).unwrap_err();
    assert!(matches!(err, HoldError::ConfigError { .. }));
}

#[test]
fn test_stow_preserves_last_gc_timestamp_when_time_advances() {
    let temp_dir = setup_git_repo();
    let metadata_path = temp_dir.path().join("test.metadata");
    let tracked_file = temp_dir.path().join("test.txt");

    // Simulate a build finishing an hour ago by backdating the tracked file.
    let one_hour_ago = SystemTime::now() - Duration::from_secs(3600);
    filetime::set_file_mtime(&tracked_file, FileTime::from_system_time(one_hour_ago)).unwrap();

    stow(&metadata_path, 0, false, temp_dir.path()).unwrap();
    let first_metadata = load_metadata(&metadata_path).unwrap();
    let first_preservation = first_metadata
        .last_gc_mtime_nanos
        .expect("stow should set last_gc_mtime_nanos");
    let expected_nanos = one_hour_ago.duration_since(UNIX_EPOCH).unwrap().as_nanos();
    assert_eq!(first_preservation, expected_nanos);

    // Allow the wall clock to move forward before running stow again.
    std::thread::sleep(Duration::from_millis(10));

    stow(&metadata_path, 0, false, temp_dir.path()).unwrap();
    let second_metadata = load_metadata(&metadata_path).unwrap();
    let second_preservation = second_metadata
        .last_gc_mtime_nanos
        .expect("stow should keep last_gc_mtime_nanos set");

    assert_eq!(second_preservation, expected_nanos);
}

fn make_profile(target: &Path) {
    let profile = target.join("debug");
    fs::create_dir_all(profile.join("build")).unwrap();
    fs::create_dir_all(profile.join("deps")).unwrap();
    fs::create_dir_all(profile.join(".fingerprint")).unwrap();
}

#[test]
fn test_heave_auto_cap_records_metrics() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    make_profile(&target_dir);
    let metadata_path = temp_dir.path().join("cargo-hold.metadata");

    let mut metadata = StateMetadata::new();
    metadata.gc_metrics.seed_initial_size = Some(5 * 1024 * 1024);
    metadata.gc_metrics.recent_initial_sizes = vec![5 * 1024 * 1024, 6 * 1024 * 1024];
    metadata.gc_metrics.recent_bytes_freed = vec![0, 0];
    save_metadata(&metadata, &metadata_path).unwrap();

    Heave::builder()
        .target_dir(&target_dir)
        .max_target_size(None)
        .auto_max_target_size(true)
        .metadata_path(&metadata_path)
        .age_threshold_days(7)
        .verbose(0)
        .quiet(true)
        .build()
        .heave()
        .unwrap();

    let reloaded = load_metadata(&metadata_path).unwrap();
    let metrics = &reloaded.gc_metrics;
    assert_eq!(metrics.runs, 1);
    assert!(
        metrics
            .last_suggested_cap
            .is_some_and(|cap| cap == 12 * 1024 * 1024)
    ); // capped at 2x max final (6 MiB -> 12 MiB)
    assert!(!metrics.recent_initial_sizes.is_empty());
}

#[test]
fn test_heave_auto_cap_can_be_disabled() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    make_profile(&target_dir);
    let metadata_path = temp_dir.path().join("cargo-hold.metadata");

    let mut metadata = StateMetadata::new();
    metadata.gc_metrics.seed_initial_size = Some(5 * 1024 * 1024);
    save_metadata(&metadata, &metadata_path).unwrap();

    Heave::builder()
        .target_dir(&target_dir)
        .max_target_size(None)
        .auto_max_target_size(false)
        .metadata_path(&metadata_path)
        .age_threshold_days(7)
        .verbose(0)
        .quiet(true)
        .build()
        .heave()
        .unwrap();

    let reloaded = load_metadata(&metadata_path).unwrap();
    assert!(reloaded.gc_metrics.last_suggested_cap.is_none());
}

fn mk_metrics(initials: &[u64], freed: &[u64], last_cap: Option<u64>) -> GcMetrics {
    GcMetrics {
        runs: initials.len() as u32,
        seed_initial_size: initials.first().copied(),
        recent_initial_sizes: initials.to_vec(),
        recent_bytes_freed: freed.to_vec(),
        last_suggested_cap: last_cap,
    }
}

#[test]
fn steady_usage_stays_near_baseline() {
    // Stable finals ~10 GiB, prior cap 12 GiB.
    let initials = [12 * 1024 * 1024 * 1024];
    let freed = [2 * 1024 * 1024 * 1024]; // final = 10 GiB
    let metrics = mk_metrics(&initials, &freed, Some(12 * 1024 * 1024 * 1024));

    let cap = suggest_max_target_size(&metrics, Some(initials[0])).unwrap();
    // With 2 GiB headroom and 10% clamp, stays at 12 GiB.
    assert_eq!(cap, 12 * 1024 * 1024 * 1024);
}

#[test]
fn slow_growth_advances_gradually() {
    // Finals grow 0.5 GiB per run; last cap 12 GiB.
    let g = 1024 * 1024 * 1024 / 2; // 0.5 GiB
    let finals = [10 * 1024 * 1024 * 1024, 10 * 1024 * 1024 * 1024 + g];
    let initials = [
        finals[0] + 2 * 1024 * 1024 * 1024,
        finals[1] + 2 * 1024 * 1024 * 1024,
    ];
    let freed = [2 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024];
    let metrics = mk_metrics(&initials, &freed, Some(12 * 1024 * 1024 * 1024));

    let cap = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    // Cap grows to 13 GiB (baseline 10.5 + 2.5 growth, below +10% clamp).
    let expected = 13 * 1024 * 1024 * 1024;
    assert_eq!(cap, expected);
}

#[test]
fn spike_is_bounded_by_hard_ceiling() {
    // One large spike to 30 GiB, finals previously 10 GiB.
    let initials = [12 * 1024 * 1024 * 1024, 32 * 1024 * 1024 * 1024];
    let freed = [2 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024]; // finals 10 GiB, 30 GiB
    let metrics = mk_metrics(&initials, &freed, Some(12 * 1024 * 1024 * 1024));

    let cap = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    // Per-run clamp from 12 GiB limits growth to +10%.
    let expected =
        12 * 1024 * 1024 * 1024 + (12 * 1024 * 1024 * 1024 * MAX_GROWTH_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, expected);
}

#[test]
fn shrink_moves_down_slowly_not_below_baseline() {
    // Finals drop from 10 GiB to 6 GiB; prior cap 14 GiB.
    let initials = [12 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024];
    let freed = [2 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024]; // finals 10 GiB, 6 GiB
    let metrics = mk_metrics(&initials, &freed, Some(14 * 1024 * 1024 * 1024));

    let cap = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    // Cap should decline by at most 10% per run, but never below baseline (6 GiB).
    let min_cap =
        14 * 1024 * 1024 * 1024 - (14 * 1024 * 1024 * 1024 * MAX_SHRINK_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, min_cap);
}
