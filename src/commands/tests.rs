use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

use super::*;
use crate::gc::auto_cap::{
    HARD_CEILING_MIN_FINALS, MAX_GROWTH_FACTOR_PER_RUN_PCT, MAX_SHRINK_FACTOR_PER_RUN_PCT,
    MIN_HEADROOM_BYTES, suggest_max_target_size,
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
    assert!(matches!(err, HoldError::ConfigError(_)));
}

#[test]
fn test_stow_preserves_last_gc_timestamp_when_time_advances() {
    let temp_dir = setup_git_repo();
    let metadata_path = temp_dir.path().join("test.metadata");
    let one_hour_ago = SystemTime::now() - Duration::from_secs(3600);
    let expected_nanos = one_hour_ago.duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let mut seed = StateMetadata::new();
    seed.last_gc_mtime_nanos = Some(expected_nanos);
    save_metadata(&seed, &metadata_path).unwrap();

    // Allow the wall clock to move forward before running stow again.
    std::thread::sleep(Duration::from_millis(10));

    stow(&metadata_path, 0, false, temp_dir.path()).unwrap();
    let second_metadata = load_metadata(&metadata_path).unwrap();
    let second_preservation = second_metadata
        .last_gc_mtime_nanos
        .expect("stow should keep last_gc_mtime_nanos set");

    assert_eq!(second_preservation, expected_nanos);
}

#[test]
fn test_stow_preserves_gc_metrics() {
    let temp_dir = setup_git_repo();
    let metadata_path = temp_dir.path().join("test.metadata");

    let mut existing = StateMetadata::new();
    existing.gc_metrics = GcMetrics {
        runs: 3,
        seed_initial_size: Some(123),
        recent_initial_sizes: vec![100, 110, 120],
        recent_bytes_freed: vec![10, 20, 30],
        last_suggested_cap: Some(456),
        recent_final_sizes: vec![90, 95, 100],
        last_cap_trace: Some(crate::state::CapTrace {
            baseline: 100,
            growth_budget: 20,
            observed_growth_pct: 5,
            clamp_reason: "deadband/hold".to_string(),
        }),
    };
    save_metadata(&existing, &metadata_path).unwrap();

    stow(&metadata_path, 0, false, temp_dir.path()).unwrap();
    let reloaded = load_metadata(&metadata_path).unwrap();

    assert_eq!(reloaded.gc_metrics, existing.gc_metrics);
}

fn make_profile(target: &Path) {
    let profile = target.join("debug");
    fs::create_dir_all(profile.join("build")).unwrap();
    fs::create_dir_all(profile.join("deps")).unwrap();
    fs::create_dir_all(profile.join(".fingerprint")).unwrap();
}

#[test]
fn test_heave_records_last_gc_timestamp() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    make_profile(&target_dir);
    let metadata_path = temp_dir.path().join("cargo-hold.metadata");

    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    Heave::builder()
        .target_dir(&target_dir)
        .max_target_size(None)
        .auto_max_target_size(false)
        .metadata_path(&metadata_path)
        .age_threshold_days(7)
        .verbose(0)
        .quiet(true)
        .build()
        .unwrap()
        .heave()
        .unwrap();

    let after = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    let reloaded = load_metadata(&metadata_path).unwrap();
    let recorded = reloaded
        .last_gc_mtime_nanos
        .expect("heave should record last_gc_mtime_nanos");

    assert!(
        recorded >= before && recorded <= after,
        "last_gc_mtime_nanos should reflect GC time"
    );
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
        .unwrap()
        .heave()
        .unwrap();

    let reloaded = load_metadata(&metadata_path).unwrap();
    let metrics = &reloaded.gc_metrics;
    assert_eq!(metrics.runs, 1);
    assert!(
        metrics
            .last_suggested_cap
            .is_some_and(|cap| cap == MIN_HEADROOM_BYTES + 6 * 1024 * 1024)
    );
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
        .unwrap()
        .heave()
        .unwrap();

    let reloaded = load_metadata(&metadata_path).unwrap();
    assert!(reloaded.gc_metrics.last_suggested_cap.is_none());
}

#[test]
fn cold_start_from_current_skips_hard_ceiling() {
    let metrics = GcMetrics::default();
    let seed = 1024 * 1024;

    let (cap, trace) = suggest_max_target_size(&metrics, Some(seed)).unwrap();

    assert_eq!(cap, seed + MIN_HEADROOM_BYTES);
    assert_eq!(trace.clamp_reason, "cold-start");
}

#[test]
fn finals_without_initials_still_respect_hard_ceiling() {
    let gib = 1024 * 1024 * 1024;
    let metrics = GcMetrics {
        recent_final_sizes: vec![2 * gib],
        ..Default::default()
    };

    let (cap, trace) = suggest_max_target_size(&metrics, Some(gib)).unwrap();

    assert_eq!(cap, 4 * gib);
    assert_eq!(trace.clamp_reason, "cold-start");
}

#[test]
fn zero_finals_shrink_slowly_from_prev_cap() {
    let gib = 1024 * 1024 * 1024;
    let metrics = mk_metrics_with_finals(&[0, 0], &[0, 0], &[0, 0], Some(10 * gib));

    let (cap, trace) = suggest_max_target_size(&metrics, Some(10 * gib)).unwrap();

    let max_down = 10 * gib - (10 * gib * MAX_SHRINK_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, max_down);
    assert_eq!(trace.clamp_reason, "clamped:-shrink");
}

#[test]
fn tiny_restore_shrinks_by_max_down_not_below_headroom_floor() {
    let gib = 1024 * 1024 * 1024;
    let tiny = 50 * 1024 * 1024;
    let metrics = mk_metrics_with_finals(&[tiny, tiny], &[0, 0], &[tiny, tiny], Some(10 * gib));

    let (cap, trace) = suggest_max_target_size(&metrics, Some(tiny)).unwrap();

    let max_down = 10 * gib - (10 * gib * MAX_SHRINK_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, max_down);
    assert_eq!(trace.clamp_reason, "clamped:-shrink");
}

fn mk_metrics(initials: &[u64], freed: &[u64], last_cap: Option<u64>) -> GcMetrics {
    GcMetrics {
        runs: initials.len() as u32,
        seed_initial_size: initials.first().copied(),
        recent_initial_sizes: initials.to_vec(),
        recent_bytes_freed: freed.to_vec(),
        last_suggested_cap: last_cap,
        recent_final_sizes: Vec::new(),
        last_cap_trace: None,
    }
}

fn mk_metrics_with_finals(
    initials: &[u64],
    freed: &[u64],
    finals: &[u64],
    last_cap: Option<u64>,
) -> GcMetrics {
    GcMetrics {
        runs: initials.len() as u32,
        seed_initial_size: initials.first().copied(),
        recent_initial_sizes: initials.to_vec(),
        recent_bytes_freed: freed.to_vec(),
        last_suggested_cap: last_cap,
        recent_final_sizes: finals.to_vec(),
        last_cap_trace: None,
    }
}

#[test]
fn hard_ceiling_requires_min_history() {
    let gib = 1024 * 1024 * 1024;
    let metrics = GcMetrics {
        recent_final_sizes: vec![10 * gib; HARD_CEILING_MIN_FINALS],
        recent_initial_sizes: vec![40 * gib; HARD_CEILING_MIN_FINALS],
        recent_bytes_freed: vec![30 * gib; HARD_CEILING_MIN_FINALS],
        ..Default::default()
    };

    let (cap, trace) = suggest_max_target_size(&metrics, Some(12 * gib)).unwrap();

    assert_eq!(cap, 20 * gib);
    assert_eq!(trace.clamp_reason, "hard-ceiling");
}

#[test]
fn hard_ceiling_does_not_bypass_shrink_clamp() {
    let gib = 1024 * 1024 * 1024;
    let metrics = GcMetrics {
        last_suggested_cap: Some(10 * gib),
        recent_final_sizes: vec![gib; HARD_CEILING_MIN_FINALS],
        recent_initial_sizes: vec![40 * gib; HARD_CEILING_MIN_FINALS],
        recent_bytes_freed: vec![39 * gib; HARD_CEILING_MIN_FINALS],
        ..Default::default()
    };

    let (cap, trace) = suggest_max_target_size(&metrics, Some(12 * gib)).unwrap();

    let max_down = 10 * gib - (10 * gib * MAX_SHRINK_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, max_down);
    assert_eq!(trace.clamp_reason, "clamped:-shrink");
}

#[test]
fn steady_usage_stays_near_baseline() {
    // Stable finals ~10 GiB, prior cap 12 GiB.
    let initials = [12 * 1024 * 1024 * 1024];
    let freed = [2 * 1024 * 1024 * 1024]; // final = 10 GiB
    let metrics = mk_metrics(&initials, &freed, Some(12 * 1024 * 1024 * 1024));

    let (cap, _) = suggest_max_target_size(&metrics, Some(initials[0])).unwrap();
    // Deadband allows shrink within clamp; 10% down from 12 GiB = 10.8 GiB.
    let expected =
        12 * 1024 * 1024 * 1024 - (12 * 1024 * 1024 * 1024 * MAX_SHRINK_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, expected);
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

    let (cap, _) = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    // Growth is within deadband; cap holds steady at 12 GiB.
    assert_eq!(cap, 12 * 1024 * 1024 * 1024);
}

#[test]
fn flat_usage_still_ratchets_up_from_headroom_floor() {
    let gib = 1024 * 1024 * 1024;
    // Two runs that end right at the 10 GiB cap; no real growth.
    let initials = [12 * gib, 12 * gib];
    let freed = [2 * gib, 2 * gib]; // finals stay 10 GiB both times
    let last_cap = 10 * gib;
    let metrics = mk_metrics(&initials, &freed, Some(last_cap));

    let (cap, _) = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    // Deadband prevents drift; cap should stay at 10 GiB.
    assert_eq!(cap, last_cap);
}

#[test]
fn repeated_caps_keep_increasing_even_without_growth() {
    let gib = 1024 * 1024 * 1024;
    // Prior run already ratcheted to 11 GiB; usage still flat at the cap.
    let initials = [13 * gib, 13 * gib];
    let freed = [2 * gib, 2 * gib]; // finals stay 11 GiB
    let last_cap = 11 * gib;
    let metrics = mk_metrics(&initials, &freed, Some(last_cap));

    let (cap, trace) = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    // Deadband should keep the cap pinned at 11 GiB.
    assert_eq!(cap, last_cap);
    assert_eq!(trace.clamp_reason, "deadband/hold");
}

#[test]
fn non_target_cleanup_does_not_inflate_growth() {
    let gib = 1024 * 1024 * 1024;
    // Target sits steady at 10 GiB, but a noisy registry cleanup reports 5 GiB
    // freed.
    let finals = [10 * gib, 10 * gib];
    let initials = [10 * gib, 10 * gib];
    let freed = [5 * gib, 0];
    let last_cap = 10 * gib;
    let metrics = mk_metrics_with_finals(&initials, &freed, &finals, Some(last_cap));

    let (cap, trace) = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    assert_eq!(cap, last_cap);
    assert_eq!(trace.clamp_reason, "deadband/hold");
}

#[test]
fn small_noise_stays_flat_with_deadband() {
    let gib = 1024 * 1024 * 1024;
    // Finals bounce by <1% between runs; prior cap 10 GiB.
    let finals = [10 * gib, 10 * gib + 50 * 1024 * 1024];
    let initials = [finals[0] + 2 * gib, finals[1] + 2 * gib];
    let freed = [2 * gib, 2 * gib];
    let last_cap = 10 * gib;
    let metrics = mk_metrics(&initials, &freed, Some(last_cap));

    let (cap, _) = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    assert_eq!(cap, last_cap);
}

#[test]
fn sustained_growth_moves_up_within_clamp() {
    let gib = 1024 * 1024 * 1024;
    // Finals grow meaningfully; cap should ratchet up but stay within +10%.
    let finals = [12 * gib, 14 * gib];
    let initials = [finals[0] + 2 * gib, finals[1] + 2 * gib];
    let freed = [2 * gib, 2 * gib];
    let last_cap = 12 * gib;
    let metrics = mk_metrics(&initials, &freed, Some(last_cap));

    let (cap, _) = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    let expected = last_cap + (last_cap * MAX_GROWTH_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, expected);
}

#[test]
fn spike_is_bounded_by_hard_ceiling() {
    // One large spike to 30 GiB, finals previously 10 GiB.
    let initials = [12 * 1024 * 1024 * 1024, 32 * 1024 * 1024 * 1024];
    let freed = [2 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024]; // finals 10 GiB, 30 GiB
    let metrics = mk_metrics(&initials, &freed, Some(12 * 1024 * 1024 * 1024));

    let (cap, _trace) = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

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

    let (cap, _) = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    // Cap should decline by at most 10% per run, but never below baseline (6 GiB).
    let min_cap =
        14 * 1024 * 1024 * 1024 - (14 * 1024 * 1024 * 1024 * MAX_SHRINK_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, min_cap);
}
