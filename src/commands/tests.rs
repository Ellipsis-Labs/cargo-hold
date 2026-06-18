use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

use super::*;
use crate::gc::auto_cap::{
    HARD_CEILING_MIN_FINALS, MAX_GROWTH_FACTOR_PER_RUN_PCT, MAX_SHRINK_FACTOR_PER_RUN_PCT,
    MIN_HEADROOM_BYTES, push_bounded, record_auto_cap_outcome, suggest_max_target_size,
};
use crate::metadata::{load_metadata, save_metadata};
use crate::state::{
    CAP_TRACE_SAMPLE_SOURCE_HEALTHY, CAP_TRACE_SAMPLE_SOURCE_LEGACY, GcMetrics, METADATA_VERSION,
    StateMetadata,
};

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
fn test_stow_fails_when_tracked_file_is_missing() {
    let temp_dir = setup_git_repo();
    let metadata_path = temp_dir.path().join("test.metadata");

    fs::remove_file(temp_dir.path().join("test.txt")).unwrap();

    let err = stow(&metadata_path, 0, true, temp_dir.path()).unwrap_err();
    assert!(matches!(
        err,
        HoldError::PartialFileProcessing {
            failed: 1,
            total: 1,
        }
    ));
    assert!(!metadata_path.exists());
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
fn test_salvage_fails_when_tracked_file_is_missing() {
    let temp_dir = setup_git_repo();
    let metadata_path = temp_dir.path().join("test.metadata");

    stow(&metadata_path, 0, true, temp_dir.path()).unwrap();
    fs::remove_file(temp_dir.path().join("test.txt")).unwrap();

    let err = salvage(&metadata_path, 0, true, temp_dir.path()).unwrap_err();
    assert!(matches!(
        err,
        HoldError::PartialFileProcessing {
            failed: 1,
            total: 1,
        }
    ));
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
        recent_sizing_final_sizes: vec![90, 95],
        recent_cap_overage_bytes: vec![4],
        last_cap_trace: Some(crate::state::CapTrace {
            baseline: 100,
            growth_budget: 20,
            observed_growth_pct: 5,
            clamp_reason: "deadband/hold".to_string(),
            sample_source: CAP_TRACE_SAMPLE_SOURCE_HEALTHY.to_string(),
            sample_count: 2,
            ignored_over_cap_sample_count: 1,
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

fn write_crate_artifact(target: &Path, name: &str, hash: &str, size: usize, mtime: SystemTime) {
    let profile = target.join("debug");
    let deps = profile.join("deps");
    fs::create_dir_all(&deps).unwrap();
    let artifact = deps.join(format!("lib{name}-{hash}.rlib"));
    fs::write(&artifact, vec![0u8; size]).unwrap();
    filetime::set_file_mtime(&artifact, filetime::FileTime::from_system_time(mtime)).unwrap();

    let fingerprint = profile.join(format!(".fingerprint/lib{name}-{hash}"));
    fs::create_dir_all(&fingerprint).unwrap();
    let invoked = fingerprint.join("invoked.timestamp");
    fs::write(&invoked, b"dummy").unwrap();
    let filetime = filetime::FileTime::from_system_time(mtime);
    filetime::set_file_mtime(&fingerprint, filetime).unwrap();
    filetime::set_file_mtime(&invoked, filetime).unwrap();
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
    assert_eq!(metrics.recent_sizing_final_sizes, vec![0]);
    assert!(metrics.recent_cap_overage_bytes.is_empty());
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
fn test_heave_auto_cap_records_overage_without_sizing_sample() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    make_profile(&target_dir);
    let metadata_path = temp_dir.path().join("cargo-hold.metadata");

    let previous_gc = SystemTime::now() - Duration::from_secs(1);
    let mut metadata = StateMetadata::new();
    metadata.last_gc_mtime_nanos = Some(previous_gc.duration_since(UNIX_EPOCH).unwrap().as_nanos());
    metadata.gc_metrics.seed_initial_size = Some(1024);
    metadata.gc_metrics.recent_initial_sizes = vec![1024];
    metadata.gc_metrics.recent_final_sizes = vec![1024];
    metadata.gc_metrics.recent_sizing_final_sizes = vec![1024];
    metadata.gc_metrics.last_suggested_cap = Some(1024);
    save_metadata(&metadata, &metadata_path).unwrap();

    write_crate_artifact(
        &target_dir,
        "preserved",
        "1234567890abcd12",
        32 * 1024,
        SystemTime::now(),
    );

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
    assert_eq!(metrics.last_suggested_cap, Some(1024));
    assert_eq!(metrics.recent_sizing_final_sizes, vec![1024]);
    assert_eq!(
        metrics.recent_cap_overage_bytes,
        vec![metrics.recent_final_sizes.last().copied().unwrap() - 1024]
    );
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
        ..Default::default()
    }
}

fn mk_metrics_with_finals(
    initials: &[u64],
    freed: &[u64],
    finals: &[u64],
    last_cap: Option<u64>,
) -> GcMetrics {
    let mut metrics = mk_metrics(initials, freed, last_cap);
    metrics.recent_final_sizes = finals.to_vec();
    metrics
}

fn with_sizing_finals(mut metrics: GcMetrics, finals: &[u64]) -> GcMetrics {
    metrics.recent_sizing_final_sizes = finals.to_vec();
    metrics
}

fn with_cap_overages(mut metrics: GcMetrics, overages: &[u64]) -> GcMetrics {
    metrics.recent_cap_overage_bytes = overages.to_vec();
    metrics
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
    let metrics = with_sizing_finals(mk_metrics(&initials, &freed, Some(last_cap)), &finals);

    let (cap, _) = suggest_max_target_size(&metrics, Some(initials[1])).unwrap();

    let expected = last_cap + (last_cap * MAX_GROWTH_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, expected);
}

#[test]
fn over_cap_legacy_finals_are_ignored_for_baseline() {
    let gib = 1024 * 1024 * 1024;
    let last_cap = 10 * gib;
    let metrics = with_cap_overages(
        mk_metrics_with_finals(
            &[12 * gib, 32 * gib],
            &[2 * gib, 2 * gib],
            &[10 * gib, 30 * gib],
            Some(last_cap),
        ),
        &[20 * gib],
    );

    let (cap, trace) = suggest_max_target_size(&metrics, Some(32 * gib)).unwrap();

    assert_eq!(cap, last_cap);
    assert_eq!(trace.sample_source, CAP_TRACE_SAMPLE_SOURCE_LEGACY);
    assert_eq!(trace.sample_count, 1);
    assert_eq!(trace.ignored_over_cap_sample_count, 1);
}

#[test]
fn repeated_over_cap_runs_without_healthy_samples_do_not_grow_cap() {
    let gib = 1024 * 1024 * 1024;
    let last_cap = 10 * gib;
    let metrics = with_cap_overages(
        mk_metrics_with_finals(
            &[30 * gib, 31 * gib],
            &[0, 0],
            &[30 * gib, 31 * gib],
            Some(last_cap),
        ),
        &[20 * gib, 21 * gib],
    );

    let (cap, trace) = suggest_max_target_size(&metrics, Some(31 * gib)).unwrap();

    assert_eq!(cap, last_cap);
    assert_eq!(trace.sample_source, CAP_TRACE_SAMPLE_SOURCE_LEGACY);
    assert_eq!(trace.ignored_over_cap_sample_count, 2);
}

#[test]
fn healthy_sizing_samples_drive_bounded_growth() {
    let gib = 1024 * 1024 * 1024;
    let last_cap = 12 * gib;
    let metrics = with_cap_overages(
        with_sizing_finals(
            mk_metrics_with_finals(
                &[12 * gib, 20 * gib],
                &[2 * gib, 2 * gib],
                &[10 * gib, 30 * gib],
                Some(last_cap),
            ),
            &[12 * gib, 14 * gib],
        ),
        &[18 * gib],
    );

    let (cap, trace) = suggest_max_target_size(&metrics, Some(30 * gib)).unwrap();

    let expected = last_cap + (last_cap * MAX_GROWTH_FACTOR_PER_RUN_PCT) / 100;
    assert_eq!(cap, expected);
    assert_eq!(trace.sample_source, CAP_TRACE_SAMPLE_SOURCE_HEALTHY);
    assert_eq!(trace.sample_count, 2);
    assert_eq!(trace.ignored_over_cap_sample_count, 1);
}

#[test]
fn repeated_large_overages_do_not_ratchet_auto_cap() {
    let gib = 1024 * 1024 * 1024;
    let healthy_final = 10 * gib;
    let bloated_final = 256 * gib;
    let mut metrics = with_sizing_finals(
        mk_metrics_with_finals(&[12 * gib], &[2 * gib], &[healthy_final], Some(12 * gib)),
        &[healthy_final],
    );

    let mut previous_cap = metrics.last_suggested_cap.unwrap();
    for _ in 0..8 {
        let (cap, trace) = suggest_max_target_size(&metrics, Some(bloated_final)).unwrap();
        assert!(
            cap <= previous_cap,
            "over-cap finals must not increase cap: previous={previous_cap}, next={cap}"
        );
        assert_eq!(trace.sample_source, CAP_TRACE_SAMPLE_SOURCE_HEALTHY);
        assert_eq!(trace.sample_count, 1);
        assert!(
            cap < bloated_final / 10,
            "cap should stay tied to healthy baseline, not observed bloat"
        );

        push_bounded(&mut metrics.recent_initial_sizes, bloated_final);
        push_bounded(&mut metrics.recent_bytes_freed, 0);
        push_bounded(&mut metrics.recent_final_sizes, bloated_final);
        metrics.last_suggested_cap = Some(cap);
        record_auto_cap_outcome(&mut metrics, cap, bloated_final);
        previous_cap = cap;
    }

    assert_eq!(metrics.recent_sizing_final_sizes, vec![healthy_final]);
    assert_eq!(metrics.recent_cap_overage_bytes.len(), 8);
    assert!(metrics.recent_final_sizes.contains(&bloated_final));
}

#[test]
fn spike_is_bounded_by_hard_ceiling() {
    // One large spike to 30 GiB, finals previously 10 GiB.
    let initials = [12 * 1024 * 1024 * 1024, 32 * 1024 * 1024 * 1024];
    let freed = [2 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024]; // finals 10 GiB, 30 GiB
    let metrics = with_sizing_finals(
        mk_metrics(&initials, &freed, Some(12 * 1024 * 1024 * 1024)),
        &[10 * 1024 * 1024 * 1024, 30 * 1024 * 1024 * 1024],
    );

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
