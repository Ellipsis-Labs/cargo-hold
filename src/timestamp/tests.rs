use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use crate::state::{FileState, StateMetadata};
use crate::timestamp::{
    generate_monotonic_timestamp, restore_timestamps, set_file_mtime, system_time_to_nanos,
};

#[test]
fn test_generate_monotonic_timestamp() {
    let mut metadata = StateMetadata::new();

    // Empty metadata should use current time
    let ts1 = generate_monotonic_timestamp(&metadata);
    assert!(ts1 >= SystemTime::now() - Duration::from_secs(1));

    // Add a file with a future timestamp
    let future_time = SystemTime::now() + Duration::from_secs(3600);
    metadata
        .upsert(FileState {
            path: PathBuf::from("test.rs"),
            size: 100,
            hash: "hash".to_string(),
            mtime_nanos: system_time_to_nanos(future_time),
        })
        .unwrap();

    // Generated timestamp should be after the future time
    let ts2 = generate_monotonic_timestamp(&metadata);
    assert!(ts2 > future_time);
}

#[test]
fn test_set_file_mtime() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "content").unwrap();

    let new_time = SystemTime::now() - Duration::from_secs(3600);
    set_file_mtime(&test_file, new_time).unwrap();

    let metadata = fs::metadata(&test_file).unwrap();
    let mtime = metadata.modified().unwrap();

    // Allow small delta for filesystem precision
    let delta = mtime
        .duration_since(new_time)
        .unwrap_or_else(|e| e.duration());
    assert!(delta < Duration::from_secs(1));
}

#[test]
fn test_restore_timestamps() {
    let temp_dir = TempDir::new().unwrap();

    // Create test files
    let unchanged_file = temp_dir.path().join("unchanged.txt");
    let modified_file = temp_dir.path().join("modified.txt");
    let added_file = temp_dir.path().join("added.txt");

    fs::write(&unchanged_file, "unchanged").unwrap();
    fs::write(&modified_file, "modified").unwrap();
    fs::write(&added_file, "added").unwrap();

    // Create file states with relative paths
    let old_time = SystemTime::now() - Duration::from_secs(7200);
    let unchanged_state = FileState {
        path: PathBuf::from("unchanged.txt"),
        size: 9,
        hash: "hash1".to_string(),
        mtime_nanos: system_time_to_nanos(old_time),
    };

    let new_time = SystemTime::now();

    // Restore timestamps (using temp_dir as repo root)
    restore_timestamps(
        temp_dir.path(),
        &[&unchanged_state],
        &[&PathBuf::from("modified.txt")],
        &[&PathBuf::from("added.txt")],
        new_time,
    )
    .unwrap();

    // Verify unchanged file has old timestamp
    let unchanged_meta = fs::metadata(&unchanged_file).unwrap();
    let unchanged_mtime = unchanged_meta.modified().unwrap();
    let delta = unchanged_mtime
        .duration_since(old_time)
        .unwrap_or_else(|e| e.duration());
    assert!(delta < Duration::from_secs(1));

    // Verify modified and added files have new timestamp
    for path in &[&modified_file, &added_file] {
        let meta = fs::metadata(path).unwrap();
        let mtime = meta.modified().unwrap();
        let delta = mtime
            .duration_since(new_time)
            .unwrap_or_else(|e| e.duration());
        assert!(delta < Duration::from_secs(1));
    }
}

#[test]
#[cfg(unix)]
fn test_set_mtime_symlink() {
    use std::os::unix::fs::symlink;
    use std::time::SystemTime;

    use crate::error::HoldError;

    let temp_dir = TempDir::new().unwrap();
    let target = temp_dir.path().join("target.txt");
    let link = temp_dir.path().join("link.txt");

    fs::write(&target, "content").unwrap();
    symlink(&target, &link).unwrap();

    let result = set_file_mtime(&link, SystemTime::now());
    assert!(matches!(result, Err(HoldError::InvalidFileType { .. })));
}

#[test]
#[cfg(unix)]
fn test_set_mtime_read_only_file() {
    use std::os::unix::fs::PermissionsExt;

    use crate::error::HoldError;

    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("readonly.txt");

    fs::write(&test_file, "content").unwrap();

    // Make file read-only
    let mut perms = fs::metadata(&test_file).unwrap().permissions();
    perms.set_mode(0o444);
    fs::set_permissions(&test_file, perms).unwrap();

    let result = set_file_mtime(&test_file, SystemTime::now());
    assert!(matches!(result, Err(HoldError::SetTimestampError { .. })));
}
