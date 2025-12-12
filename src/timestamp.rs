use std::cmp::max;
use std::fs::OpenOptions;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// Compute a duration from nanoseconds with saturation at [`Duration::MAX`].
///
/// Returns the saturated duration along with a flag indicating whether the
/// input exceeded the representable range.
pub fn saturating_duration_from_nanos(nanos: u128) -> (Duration, bool) {
    let seconds = nanos / NANOS_PER_SECOND;
    if seconds > u64::MAX as u128 {
        return (Duration::MAX, true);
    }

    let nanos_remainder = (nanos % NANOS_PER_SECOND) as u32;
    (Duration::new(seconds as u64, nanos_remainder), false)
}

/// Compute a [`SystemTime`] from nanoseconds with saturation at
/// `UNIX_EPOCH + Duration::MAX`.
///
/// Returns the saturated timestamp along with a flag indicating whether the
/// input exceeded the representable range.
pub fn saturating_system_time_from_nanos(nanos: u128) -> (SystemTime, bool) {
    let (duration, saturated) = saturating_duration_from_nanos(nanos);
    (UNIX_EPOCH + duration, saturated)
}

use crate::error::{HoldError, Result};
use crate::state::{FileState, StateMetadata};

/// Convert nanoseconds since UNIX_EPOCH to SystemTime
fn nanos_to_system_time(nanos: u128) -> SystemTime {
    saturating_system_time_from_nanos(nanos).0
}

/// Convert SystemTime to nanoseconds since UNIX_EPOCH
fn system_time_to_nanos(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos()
}

/// Generates a monotonic timestamp that is guaranteed to be newer than any
/// timestamp in the metadata.
///
/// This function ensures that timestamps only move forward, even if the system
/// clock goes backwards (e.g., due to NTP adjustments or clock skew in CI
/// environments).
///
/// # Arguments
///
/// * `metadata` - The current state metadata to check for the maximum existing
///   timestamp
///
/// # Returns
///
/// A `SystemTime` that is guaranteed to be at least 1 nanosecond newer than any
/// timestamp in the metadata, or the current system time, whichever is later.
pub fn generate_monotonic_timestamp(metadata: &StateMetadata) -> SystemTime {
    // Get the maximum timestamp from metadata in nanos
    let max_metadata_nanos = metadata.max_mtime_nanos().unwrap_or(0);

    // Get the current system time in nanos
    let now_nanos = system_time_to_nanos(SystemTime::now());

    // Return the maximum of now and max_metadata_nanos + 1
    let monotonic_nanos = max(now_nanos, max_metadata_nanos + 1);

    nanos_to_system_time(monotonic_nanos)
}

/// Sets the modification time of a file.
///
/// This function checks for symbolic links before opening the file and rejects
/// them for security reasons.
///
/// # Arguments
///
/// * `path` - Path to the file
/// * `mtime` - The new modification time to set
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be opened for writing
/// - The path points to a symbolic link
/// - The timestamp cannot be set (e.g., permission denied)
pub fn set_file_mtime(path: &Path, mtime: SystemTime) -> Result<()> {
    // Check for symlinks before opening
    let metadata = std::fs::symlink_metadata(path).map_err(|source| HoldError::IoError {
        path: path.to_path_buf(),
        source,
    })?;

    // Reject symlinks
    if metadata.is_symlink() {
        return Err(HoldError::InvalidFileType(
            path.to_path_buf(),
            "Cannot set timestamp on symbolic links".to_string(),
        ));
    }

    // Reject directories
    if metadata.is_dir() {
        return Err(HoldError::InvalidFileType(
            path.to_path_buf(),
            "Cannot set timestamp on directories".to_string(),
        ));
    }

    // Open the file to get a handle
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| HoldError::SetTimestampError(path.to_path_buf(), source))?;

    // Set the modification time
    file.set_modified(mtime)
        .map_err(|source| HoldError::SetTimestampError(path.to_path_buf(), source))?;

    Ok(())
}

/// Restores timestamps for a set of files based on their change status.
///
/// This is the core logic that enables Cargo's incremental compilation to work
/// correctly. Unchanged files get their original timestamps restored, while
/// modified and added files get a new monotonic timestamp.
///
/// # Arguments
///
/// * `repo_root` - The repository root path
/// * `unchanged_files` - Files that haven't changed (restore original
///   timestamps)
/// * `modified_files` - Files that have been modified (set new timestamp)
/// * `added_files` - Files that are newly tracked (set new timestamp)
/// * `new_mtime` - The new monotonic timestamp for modified/added files
///
/// # Errors
///
/// Returns an error if any file's timestamp cannot be set.
pub fn restore_timestamps(
    repo_root: &Path,
    unchanged_files: &[&FileState],
    modified_files: &[&Path],
    added_files: &[&Path],
    new_mtime: SystemTime,
) -> Result<()> {
    // Restore original timestamps for unchanged files
    for file_state in unchanged_files {
        let mtime = nanos_to_system_time(file_state.mtime_nanos);
        let full_path = repo_root.join(&file_state.path);
        set_file_mtime(&full_path, mtime)?;
    }

    // Set new timestamp for modified files
    for path in modified_files {
        let full_path = repo_root.join(path);
        set_file_mtime(&full_path, new_mtime)?;
    }

    // Set new timestamp for added files
    for path in added_files {
        let full_path = repo_root.join(path);
        set_file_mtime(&full_path, new_mtime)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::state::FileState;

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
}
