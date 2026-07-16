use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

use super::*;
use crate::gc::auto_cap::{
    GC_METRICS_WINDOW, MAX_GROWTH_FACTOR_PER_RUN_PCT, MAX_SHRINK_FACTOR_PER_RUN_PCT,
    MIN_HEADROOM_BYTES, MIN_STEADY_HEADROOM_BYTES, record_auto_cap_outcome,
    suggest_max_target_size,
};
use crate::metadata::{load_metadata, save_metadata};
use crate::state::{
    AutoCapRun, CAP_TRACE_SAMPLE_SOURCE_HEALTHY, CAP_TRACE_SAMPLE_SOURCE_HELD,
    CAP_TRACE_SAMPLE_SOURCE_POLICY_FLOOR, GcMetrics, METADATA_VERSION, StateMetadata,
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
        recent_auto_cap_runs: vec![AutoCapRun {
            cap: 456,
            initial_size: 120,
            final_size: 100,
            bytes_freed: 20,
            protected_artifact_bytes: 90,
            eligible_artifact_bytes: 30,
            retained_artifact_bytes: 100,
            preserved_binary_bytes: 5,
            unrecognized_bytes: 5,
        }],
        last_suggested_cap: Some(456),
        last_cap_trace: Some(crate::state::CapTrace {
            baseline: 100,
            growth_budget: 20,
            observed_growth_pct: 5,
            clamp_reason: "deadband/hold".to_string(),
            sample_source: CAP_TRACE_SAMPLE_SOURCE_HEALTHY.to_string(),
            sample_count: 2,
            ignored_over_cap_sample_count: 1,
            policy_floor: 0,
            policy_floor_sample_count: 0,
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
fn fresh_heave_records_current_auto_cap_model() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    make_profile(&target_dir);
    let metadata_path = target_dir.join("cargo-hold.metadata");

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

    let metrics = load_metadata(&metadata_path).unwrap().gc_metrics;
    assert_eq!(metrics.runs, 1);
    assert_eq!(metrics.last_suggested_cap, Some(MIN_HEADROOM_BYTES));
    assert_eq!(metrics.recent_auto_cap_runs.len(), 1);
    assert_eq!(metrics.recent_auto_cap_runs[0].final_size, 0);
}

#[test]
fn heave_auto_cap_can_be_disabled() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    make_profile(&target_dir);
    let metadata_path = temp_dir.path().join("cargo-hold.metadata");

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

    let metrics = load_metadata(&metadata_path).unwrap().gc_metrics;
    assert!(metrics.last_suggested_cap.is_none());
    assert!(metrics.recent_auto_cap_runs.is_empty());
}

#[test]
fn heave_records_why_previous_build_makes_cap_unattainable() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = temp_dir.path().join("target");
    make_profile(&target_dir);
    let metadata_path = temp_dir.path().join("cargo-hold.metadata");

    let previous_gc = SystemTime::now() - Duration::from_secs(1);
    let mut metadata = StateMetadata::new();
    metadata.last_gc_mtime_nanos = Some(previous_gc.duration_since(UNIX_EPOCH).unwrap().as_nanos());
    metadata.gc_metrics.last_suggested_cap = Some(1024);
    metadata.gc_metrics.recent_auto_cap_runs = vec![AutoCapRun {
        cap: 1024,
        initial_size: 1024,
        final_size: 1024,
        retained_artifact_bytes: 1024,
        ..Default::default()
    }];
    save_metadata(&metadata, &metadata_path).unwrap();

    write_crate_artifact(
        &target_dir,
        "preserved",
        "1234567890abcd12",
        32 * 1024,
        SystemTime::now(),
    );
    write_crate_artifact(
        &target_dir,
        "eligible",
        "abcdef1234567890",
        16 * 1024,
        SystemTime::now() - Duration::from_secs(24 * 60 * 60),
    );
    fs::write(
        target_dir.join("debug/unrecognized.bin"),
        vec![0u8; 8 * 1024],
    )
    .unwrap();
    write_preserved_binary(&target_dir.join("debug"), 4 * 1024);

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

    let metrics = load_metadata(&metadata_path).unwrap().gc_metrics;
    let run = metrics.recent_auto_cap_runs.last().unwrap();
    assert!(run.protected_artifact_bytes >= 32 * 1024);
    assert!(run.eligible_artifact_bytes >= 16 * 1024);
    assert!(run.bytes_freed >= 16 * 1024);
    assert!(run.retained_artifact_bytes >= run.protected_artifact_bytes);
    assert!(run.preserved_binary_bytes >= 4 * 1024);
    assert!(run.unrecognized_bytes >= 8 * 1024);
    assert!(run.final_size > run.cap);
    assert!(run.policy_floor() > run.cap);
}

fn write_preserved_binary(profile_dir: &Path, size: usize) {
    #[cfg(unix)]
    let path = profile_dir.join("preserved-runner");
    #[cfg(not(unix))]
    let path = profile_dir.join("preserved-runner.exe");

    fs::write(&path, vec![0u8; size]).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

#[derive(Clone, Copy, Default)]
struct SyntheticTarget {
    protected_artifacts: u64,
    preserved_binaries: u64,
    eligible_artifacts: u64,
    unrecognized: u64,
}

impl SyntheticTarget {
    fn total(self) -> u64 {
        self.protected_artifacts
            .saturating_add(self.preserved_binaries)
            .saturating_add(self.eligible_artifacts)
            .saturating_add(self.unrecognized)
    }
}

fn synthetic_voyage(metrics: &mut GcMetrics, target: SyntheticTarget) -> AutoCapRun {
    let initial_size = target.total();

    let (cap, trace) = suggest_max_target_size(metrics, Some(initial_size)).unwrap();
    let bytes_freed = initial_size
        .saturating_sub(cap)
        .min(target.eligible_artifacts);
    let run = AutoCapRun {
        cap,
        initial_size,
        final_size: initial_size - bytes_freed,
        bytes_freed,
        protected_artifact_bytes: target.protected_artifacts,
        eligible_artifact_bytes: target.eligible_artifacts,
        retained_artifact_bytes: target
            .protected_artifacts
            .saturating_add(target.eligible_artifacts.saturating_sub(bytes_freed)),
        preserved_binary_bytes: target.preserved_binaries,
        unrecognized_bytes: target.unrecognized,
    };
    metrics.last_suggested_cap = Some(cap);
    metrics.last_cap_trace = Some(trace);
    record_auto_cap_outcome(metrics, run.clone());
    run
}

fn metrics_with_healthy_run(cap: u64, final_size: u64) -> GcMetrics {
    GcMetrics {
        last_suggested_cap: Some(cap),
        recent_auto_cap_runs: vec![AutoCapRun {
            cap,
            initial_size: final_size,
            final_size,
            retained_artifact_bytes: final_size,
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn metadata_only_then_large_fallback_build_recovers_to_protected_working_set() {
    let mib = 1024 * 1024;
    let gib = 1024 * mib;
    let mut metrics = GcMetrics::default();

    // Empty/metadata-only cold start: voyage runs before the first real build.
    let cold = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            unrecognized: mib,
            ..Default::default()
        },
    );
    assert_eq!(cold.cap, MIN_HEADROOM_BYTES + mib);
    assert_eq!(cold.bytes_freed, 0);

    // Build/test grows the saved target to 19.9 GiB. The fallback restore then
    // runs voyage before building, exactly like Phoenix.
    let first_restore = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            protected_artifacts: 10 * gib + 9 * gib / 10,
            eligible_artifacts: 6 * gib + gib / 2,
            unrecognized: 2 * gib + gib / 2,
            ..Default::default()
        },
    );
    assert!(first_restore.protected_artifact_bytes > first_restore.cap);
    assert_eq!(first_restore.bytes_freed, 6 * gib + gib / 2);

    // Artifacts created after the prior voyage are protected on the next
    // fallback restore. A second proof confirms that the old cap is impossible.
    let second_restore = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            protected_artifacts: 17 * gib + 2 * gib / 5,
            unrecognized: 2 * gib + gib / 2,
            ..Default::default()
        },
    );
    assert_eq!(second_restore.cap, first_restore.cap);
    assert!(second_restore.protected_artifact_bytes > second_restore.cap);

    let recovered = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            protected_artifacts: 17 * gib + 2 * gib / 5,
            unrecognized: 2 * gib + gib / 2,
            ..Default::default()
        },
    );
    assert_eq!(
        metrics.last_cap_trace.as_ref().unwrap().sample_source,
        CAP_TRACE_SAMPLE_SOURCE_POLICY_FLOOR
    );
    assert_eq!(
        recovered.cap,
        (10 * gib + 9 * gib / 10) + MIN_HEADROOM_BYTES
    );
    assert_eq!(recovered.bytes_freed, 0);

    // The growing protected floor is confirmed again and recovery advances to
    // the realistic working set, still based only on recognized protected bytes.
    let recovered_again = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            protected_artifacts: 17 * gib + 2 * gib / 5,
            unrecognized: 2 * gib + gib / 2,
            ..Default::default()
        },
    );
    assert_eq!(
        recovered_again.cap,
        (17 * gib + 2 * gib / 5) + MIN_HEADROOM_BYTES
    );
    assert_eq!(recovered_again.bytes_freed, 0);

    // More than a complete chronological window cannot resurrect the ancient
    // metadata-only healthy sample or restart destructive cleanup.
    for _ in 0..=GC_METRICS_WINDOW {
        let run = synthetic_voyage(
            &mut metrics,
            SyntheticTarget {
                protected_artifacts: 17 * gib + 2 * gib / 5,
                unrecognized: 2 * gib + gib / 2,
                ..Default::default()
            },
        );
        assert_eq!(run.bytes_freed, 0);
        assert_eq!(run.cap, recovered_again.cap);
    }
    assert_eq!(metrics.recent_auto_cap_runs.len(), GC_METRICS_WINDOW);
    assert!(
        metrics
            .recent_auto_cap_runs
            .iter()
            .all(|run| run.initial_size > gib)
    );
}

#[test]
fn full_window_of_protected_overages_cannot_keep_an_ancient_healthy_sample_alive() {
    let gib = 1024 * 1024 * 1024;
    let tiny_cap = 257 * 1024 * 1024;
    let mut metrics = GcMetrics {
        last_suggested_cap: Some(tiny_cap),
        recent_auto_cap_runs: vec![AutoCapRun {
            cap: tiny_cap,
            initial_size: 1024 * 1024,
            final_size: 1024 * 1024,
            unrecognized_bytes: 1024 * 1024,
            ..Default::default()
        }],
        ..Default::default()
    };

    for _ in 0..(GC_METRICS_WINDOW + 5) {
        record_auto_cap_outcome(
            &mut metrics,
            AutoCapRun {
                cap: tiny_cap,
                initial_size: 20 * gib,
                final_size: 13 * gib,
                bytes_freed: 7 * gib,
                protected_artifact_bytes: 11 * gib,
                eligible_artifact_bytes: 7 * gib,
                retained_artifact_bytes: 11 * gib,
                unrecognized_bytes: 2 * gib,
                ..Default::default()
            },
        );
    }

    assert_eq!(metrics.recent_auto_cap_runs.len(), GC_METRICS_WINDOW);
    assert!(
        metrics
            .recent_auto_cap_runs
            .iter()
            .all(|run| run.final_size > run.cap)
    );

    let (cap, trace) = suggest_max_target_size(&metrics, Some(20 * gib)).unwrap();
    assert_eq!(cap, 11 * gib + MIN_HEADROOM_BYTES);
    assert_eq!(trace.sample_source, CAP_TRACE_SAMPLE_SOURCE_POLICY_FLOOR);
    assert_eq!(trace.sample_count, GC_METRICS_WINDOW as u32);
}

#[test]
fn repeated_unrecognized_256_gib_bloat_never_ratchets_cap() {
    let gib = 1024 * 1024 * 1024;
    let mut metrics = metrics_with_healthy_run(12 * gib, 10 * gib);

    let mut held_cap = None;
    for _ in 0..(GC_METRICS_WINDOW * 2) {
        let run = synthetic_voyage(
            &mut metrics,
            SyntheticTarget {
                unrecognized: 256 * gib,
                ..Default::default()
            },
        );
        assert!(run.cap <= 12 * gib);
        if let Some(expected) = held_cap {
            assert_eq!(run.cap, expected);
            assert_eq!(
                metrics.last_cap_trace.as_ref().unwrap().sample_source,
                CAP_TRACE_SAMPLE_SOURCE_HELD
            );
        } else {
            held_cap = Some(run.cap);
        }
        assert_eq!(metrics.last_cap_trace.as_ref().unwrap().policy_floor, 0);
    }
    assert_eq!(metrics.recent_auto_cap_runs.len(), GC_METRICS_WINDOW);
}

#[test]
fn repeated_eligible_256_gib_bloat_only_moves_cap_through_normal_clamps() {
    let gib = 1024 * 1024 * 1024;
    let mut metrics = metrics_with_healthy_run(12 * gib, 10 * gib);

    let mut previous_cap = 12 * gib;
    let mut consecutive_holds = 0;
    for _ in 0..(GC_METRICS_WINDOW * 3) {
        let run = synthetic_voyage(
            &mut metrics,
            SyntheticTarget {
                eligible_artifacts: 256 * gib,
                ..Default::default()
            },
        );
        assert!(
            run.cap
                <= previous_cap.saturating_add(previous_cap * MAX_GROWTH_FACTOR_PER_RUN_PCT / 100)
        );
        assert_ne!(
            metrics.last_cap_trace.as_ref().unwrap().sample_source,
            CAP_TRACE_SAMPLE_SOURCE_POLICY_FLOOR
        );
        consecutive_holds = if run.cap == previous_cap {
            consecutive_holds + 1
        } else {
            0
        };
        previous_cap = run.cap;
    }

    // The directory size itself never becomes a baseline: only the post-GC
    // final does, so repeated eligible bloat settles for a complete history
    // window instead of compounding indefinitely.
    assert!(consecutive_holds >= GC_METRICS_WINDOW);
}

#[test]
fn isolated_policy_floor_spike_does_not_trigger_recovery() {
    let gib = 1024 * 1024 * 1024;
    let mut metrics = metrics_with_healthy_run(12 * gib, 10 * gib);

    let spike = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            protected_artifacts: 128 * gib,
            preserved_binaries: 128 * gib,
            ..Default::default()
        },
    );
    assert!(spike.cap <= 12 * gib);

    let first_normal = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            protected_artifacts: 10 * gib,
            ..Default::default()
        },
    );
    assert_eq!(first_normal.cap, spike.cap);
    assert_eq!(
        metrics.last_cap_trace.as_ref().unwrap().sample_source,
        CAP_TRACE_SAMPLE_SOURCE_HELD
    );

    let second_normal = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            protected_artifacts: 10 * gib,
            ..Default::default()
        },
    );
    assert!(second_normal.cap <= spike.cap);
    assert_ne!(
        metrics.last_cap_trace.as_ref().unwrap().sample_source,
        CAP_TRACE_SAMPLE_SOURCE_POLICY_FLOOR
    );
}

#[test]
fn preserved_binaries_contribute_to_confirmed_policy_floor() {
    let gib = 1024 * 1024 * 1024;
    let mut metrics = metrics_with_healthy_run(4 * gib, 4 * gib);
    let working_set = SyntheticTarget {
        protected_artifacts: 6 * gib,
        preserved_binaries: 5 * gib,
        ..Default::default()
    };

    let first = synthetic_voyage(&mut metrics, working_set);
    let second = synthetic_voyage(&mut metrics, working_set);
    assert_eq!(first.cap, 4 * gib);
    assert_eq!(second.cap, 4 * gib);
    assert_eq!(second.policy_floor(), 11 * gib);

    let recovered = synthetic_voyage(&mut metrics, working_set);
    assert_eq!(recovered.cap, 11 * gib + MIN_HEADROOM_BYTES);
    assert_eq!(
        metrics.last_cap_trace.as_ref().unwrap().sample_source,
        CAP_TRACE_SAMPLE_SOURCE_POLICY_FLOOR
    );
    assert_eq!(
        metrics.last_cap_trace.as_ref().unwrap().policy_floor,
        11 * gib
    );
}

#[test]
fn stable_healthy_runs_do_not_oscillate_and_an_isolated_spike_does_not_stick() {
    let gib = 1024 * 1024 * 1024;
    let mut metrics = metrics_with_healthy_run(12 * gib, 10 * gib);

    let mut previous_cap = 12 * gib;
    for _ in 0..GC_METRICS_WINDOW {
        let run = synthetic_voyage(
            &mut metrics,
            SyntheticTarget {
                protected_artifacts: 10 * gib,
                ..Default::default()
            },
        );
        assert!(run.cap <= previous_cap);
        assert!(previous_cap - run.cap <= previous_cap * MAX_SHRINK_FACTOR_PER_RUN_PCT / 100);
        previous_cap = run.cap;
    }
    let steady_cap = previous_cap;
    assert!(steady_cap >= 10 * gib + MIN_STEADY_HEADROOM_BYTES);

    let spike = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            unrecognized: 256 * gib,
            ..Default::default()
        },
    );
    assert_eq!(spike.cap, steady_cap);

    for _ in 0..GC_METRICS_WINDOW {
        let run = synthetic_voyage(
            &mut metrics,
            SyntheticTarget {
                protected_artifacts: 10 * gib,
                ..Default::default()
            },
        );
        assert!(run.cap <= steady_cap);
    }
}

#[test]
fn healthy_growth_remains_bounded_per_run() {
    let gib = 1024 * 1024 * 1024;
    let mut metrics = GcMetrics {
        last_suggested_cap: Some(12 * gib),
        recent_auto_cap_runs: vec![
            AutoCapRun {
                cap: 12 * gib,
                initial_size: 10 * gib,
                final_size: 10 * gib,
                retained_artifact_bytes: 10 * gib,
                ..Default::default()
            },
            AutoCapRun {
                cap: 12 * gib,
                initial_size: 12 * gib,
                final_size: 12 * gib,
                retained_artifact_bytes: 12 * gib,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let run = synthetic_voyage(
        &mut metrics,
        SyntheticTarget {
            protected_artifacts: 13 * gib,
            ..Default::default()
        },
    );
    assert_eq!(
        run.cap,
        12 * gib + 12 * gib * MAX_GROWTH_FACTOR_PER_RUN_PCT / 100
    );
    assert_eq!(
        metrics.last_cap_trace.as_ref().unwrap().sample_source,
        CAP_TRACE_SAMPLE_SOURCE_HEALTHY
    );
}
