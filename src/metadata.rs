use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use memmap2::Mmap;

use crate::error::{HoldError, Result};
use crate::state::{METADATA_VERSION, StateMetadata};

/// Loads the state metadata from disk using zero-copy deserialization.
///
/// This function uses memory-mapped I/O and rkyv for extremely fast loading.
/// If the metadata file doesn't exist, returns empty metadata.
/// If the metadata file is from an incompatible format, automatically resets
/// it.
///
/// # Errors
///
/// Returns an error if:
/// - The metadata file exists but cannot be read due to I/O issues
/// - The metadata version is newer than the current supported version
pub fn load_metadata(metadata_path: &Path) -> Result<StateMetadata> {
    match load_metadata_inner(metadata_path) {
        Ok(metadata) => Ok(metadata),
        Err(HoldError::DeserializationError { .. }) => {
            // Any deserialization error is treated as format incompatibility
            eprintln!("⚠️  Detected incompatible metadata format from previous cargo-hold version");
            eprintln!("   Automatically resetting metadata to use new format...");

            // Try to remove the old metadata file
            if let Err(remove_err) = fs::remove_file(metadata_path) {
                eprintln!("   Warning: Could not remove old metadata file: {remove_err}");
            }

            // Return a fresh metadata instance
            Ok(StateMetadata::new())
        }
        Err(other) => Err(other),
    }
}

/// Internal function that loads metadata without automatic recovery.
fn load_metadata_inner(metadata_path: &Path) -> Result<StateMetadata> {
    // Check if file exists
    if !metadata_path.exists() {
        return Ok(StateMetadata::new());
    }

    // Open the file
    let file = File::open(metadata_path).map_err(|source| HoldError::IoError {
        path: metadata_path.to_path_buf(),
        source,
    })?;

    // Check if file is empty
    let file_metadata = file.metadata().map_err(|source| HoldError::IoError {
        path: metadata_path.to_path_buf(),
        source,
    })?;

    if file_metadata.len() == 0 {
        return Ok(StateMetadata::new());
    }

    // Memory map the file
    let mmap = unsafe { Mmap::map(&file) }.map_err(|source| HoldError::IoError {
        path: metadata_path.to_path_buf(),
        source,
    })?;

    // Deserialize using rkyv
    let metadata = rkyv::from_bytes::<StateMetadata, rkyv::rancor::BoxedError>(&mmap[..])
        .map_err(|source| HoldError::DeserializationError { source })?;

    // Check version compatibility
    if metadata.version > METADATA_VERSION {
        return Err(HoldError::ConfigError {
            message: format!(
                "Metadata version {} is newer than supported version {}. Please update cargo-hold.",
                metadata.version, METADATA_VERSION
            ),
        });
    }

    // Handle migration from older versions
    // Note: Migration happens in memory only. The file format is upgraded
    // to the current version when save_metadata() is next called.
    let metadata = if metadata.version < METADATA_VERSION {
        migrate_metadata(metadata)?
    } else {
        metadata
    };

    Ok(metadata)
}

/// Migrates metadata from older versions to the current version.
///
/// This function handles the migration path for each version upgrade.
/// Currently handles:
/// - v1 -> v2: Adds the last_gc_mtime_nanos field (defaults to None)
///
/// # Arguments
///
/// * `metadata` - The metadata to migrate
///
/// # Returns
///
/// The migrated metadata with the current version
fn migrate_metadata(mut metadata: StateMetadata) -> Result<StateMetadata> {
    // Migration from v1 to v2
    if metadata.version == 1 {
        // v1 -> v2: The last_gc_mtime_nanos field is already None by default
        // due to the Skip attribute, so we just need to update the version
        metadata.version = 2;
    }

    // Future migrations would go here
    // if metadata.version == 2 {
    //     // v2 -> v3 migration logic
    //     metadata.version = 3;
    // }

    Ok(metadata)
}

/// Saves the state metadata to disk atomically.
///
/// This function writes to a temporary file first, then atomically renames it
/// to the final location. This ensures the metadata file is never left in a
/// partially written state.
///
/// Creates the parent directory if it doesn't exist - this is needed for
/// save/sync operations.
///
/// # Errors
///
/// Returns an error if:
/// - The parent directory cannot be created
/// - The metadata cannot be serialized
/// - The file cannot be written to disk
pub fn save_metadata(metadata: &StateMetadata, metadata_path: &Path) -> Result<()> {
    // Ensure the parent directory exists - create it for save operations
    if let Some(parent) = metadata_path.parent() {
        fs::create_dir_all(parent).map_err(|source| HoldError::CreateMetadataDirError {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // Serialize to bytes using rkyv
    let bytes = rkyv::to_bytes::<rkyv::rancor::BoxedError>(metadata)
        .map_err(|e| HoldError::SerializationError(Box::new(e)))?;

    // Create a temporary file path
    let temp_path = metadata_path.with_extension("tmp");

    // Write to temporary file
    let mut temp_file = File::create(&temp_path).map_err(|source| HoldError::IoError {
        path: temp_path.clone(),
        source,
    })?;

    temp_file
        .write_all(&bytes)
        .map_err(|source| HoldError::IoError {
            path: temp_path.clone(),
            source,
        })?;

    temp_file.sync_all().map_err(|source| HoldError::IoError {
        path: temp_path.clone(),
        source,
    })?;

    // Atomically rename to final location
    fs::rename(&temp_path, metadata_path).map_err(|source| HoldError::IoError {
        path: metadata_path.to_path_buf(),
        source,
    })?;

    Ok(())
}

/// Removes the metadata file from disk.
///
/// This function is idempotent - it succeeds even if the metadata file
/// doesn't exist.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be removed (e.g., due to
/// permission issues).
pub fn clean_metadata(metadata_path: &Path) -> Result<()> {
    if metadata_path.exists() {
        fs::remove_file(metadata_path).map_err(|source| HoldError::IoError {
            path: metadata_path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use tempfile::TempDir;

    use super::*;
    use crate::state::FileState;

    #[test]
    fn test_save_and_load_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Create metadata with some data
        let mut metadata = StateMetadata::new();
        metadata
            .upsert(FileState {
                path: PathBuf::from("test.rs"),
                size: 1234,
                hash: "abcdef".to_string(),
                mtime_nanos: SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            })
            .unwrap();

        // Save it
        save_metadata(&metadata, &metadata_path).unwrap();
        assert!(metadata_path.exists());

        // Load it back
        let loaded_metadata = load_metadata(&metadata_path).unwrap();
        assert_eq!(loaded_metadata.len(), 1);
        assert!(loaded_metadata.contains(&PathBuf::from("test.rs")).unwrap());
    }

    #[test]
    fn test_load_nonexistent_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("nonexistent.metadata");

        let metadata = load_metadata(&metadata_path).unwrap();
        assert!(metadata.is_empty());
    }

    #[test]
    fn test_clean_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Create a metadata file
        let metadata = StateMetadata::new();
        save_metadata(&metadata, &metadata_path).unwrap();
        assert!(metadata_path.exists());

        // Clean it
        clean_metadata(&metadata_path).unwrap();
        assert!(!metadata_path.exists());

        // Cleaning non-existent file should not error
        clean_metadata(&metadata_path).unwrap();
    }

    #[test]
    fn test_atomic_save() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        let metadata = StateMetadata::new();
        save_metadata(&metadata, &metadata_path).unwrap();

        // Temporary file should not exist
        let temp_path = metadata_path.with_extension("tmp");
        assert!(!temp_path.exists());
        assert!(metadata_path.exists());
    }

    #[test]
    fn test_metadata_version() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Create and save metadata
        let mut metadata = StateMetadata::new();
        metadata
            .upsert(FileState {
                path: PathBuf::from("test.rs"),
                size: 100,
                hash: "hash".to_string(),
                mtime_nanos: 123456789,
            })
            .unwrap();
        save_metadata(&metadata, &metadata_path).unwrap();

        // Load and check version
        let loaded_metadata = load_metadata(&metadata_path).unwrap();
        assert_eq!(loaded_metadata.version, METADATA_VERSION);
        assert_eq!(loaded_metadata.len(), 1);
    }

    #[test]
    fn test_metadata_migration_v1_to_v2() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Create v1 metadata manually (simulate old version)
        let mut metadata = StateMetadata::new();
        metadata.version = 1; // Force to v1
        metadata
            .upsert(FileState {
                path: PathBuf::from("test.rs"),
                size: 100,
                hash: "hash".to_string(),
                mtime_nanos: 123456789,
            })
            .unwrap();

        // Save with v1
        save_metadata(&metadata, &metadata_path).unwrap();

        // Load should migrate to v2
        let loaded_metadata = load_metadata(&metadata_path).unwrap();
        assert_eq!(loaded_metadata.version, 2);
        assert_eq!(loaded_metadata.len(), 1);
        assert!(loaded_metadata.last_gc_mtime_nanos.is_none()); // Should be None after migration
    }

    #[test]
    fn test_last_gc_mtime_nanos_preservation() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Create metadata with some files
        let mut metadata = StateMetadata::new();
        metadata
            .upsert(FileState {
                path: PathBuf::from("file1.rs"),
                size: 100,
                hash: "hash1".to_string(),
                mtime_nanos: 1000000000,
            })
            .unwrap();
        metadata
            .upsert(FileState {
                path: PathBuf::from("file2.rs"),
                size: 200,
                hash: "hash2".to_string(),
                mtime_nanos: 2000000000,
            })
            .unwrap();

        assert_eq!(metadata.max_mtime_nanos(), Some(2000000000));

        // Save and load again
        save_metadata(&metadata, &metadata_path).unwrap();
        let loaded = load_metadata(&metadata_path).unwrap();

        // Create new metadata and set last_gc_mtime_nanos
        let mut new_metadata = StateMetadata::new();
        new_metadata.last_gc_mtime_nanos = loaded.max_mtime_nanos();
        new_metadata
            .upsert(FileState {
                path: PathBuf::from("file3.rs"),
                size: 300,
                hash: "hash3".to_string(),
                mtime_nanos: 3000000000,
            })
            .unwrap();

        save_metadata(&new_metadata, &metadata_path).unwrap();

        // Load and verify
        let final_metadata = load_metadata(&metadata_path).unwrap();
        assert_eq!(final_metadata.last_gc_mtime_nanos, Some(2000000000));
        assert_eq!(final_metadata.max_mtime_nanos(), Some(3000000000));
    }

    #[test]
    fn test_format_incompatibility_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Create a file with invalid/corrupted data that will cause deserialization to
        // fail This simulates the scenario where old metadata format can't be
        // read
        let invalid_data = b"this is not valid rkyv data and should cause deserialization to fail";
        fs::write(&metadata_path, invalid_data).unwrap();

        // Verify the file exists and contains our invalid data
        assert!(metadata_path.exists());
        assert_eq!(fs::read(&metadata_path).unwrap(), invalid_data);

        // Attempt to load metadata - should recover gracefully and return fresh
        // metadata
        let result = load_metadata(&metadata_path);
        assert!(result.is_ok());

        let metadata = result.unwrap();

        // Should be fresh metadata
        assert_eq!(metadata.version, METADATA_VERSION);
        assert_eq!(metadata.len(), 0);
        assert!(metadata.last_gc_mtime_nanos.is_none());

        // The invalid file should have been removed during recovery
        assert!(!metadata_path.exists());
    }

    #[test]
    fn test_format_incompatibility_with_subsequent_save() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Create invalid data to simulate old format
        let invalid_data = b"invalid rkyv data representing old format";
        fs::write(&metadata_path, invalid_data).unwrap();

        // Load should recover gracefully
        let mut metadata = load_metadata(&metadata_path).unwrap();
        assert_eq!(metadata.version, METADATA_VERSION);
        assert_eq!(metadata.len(), 0);

        // Add some data and save
        metadata
            .upsert(FileState {
                path: PathBuf::from("test.rs"),
                size: 100,
                hash: "testhash".to_string(),
                mtime_nanos: 1234567890,
            })
            .unwrap();

        save_metadata(&metadata, &metadata_path).unwrap();

        // Should be able to load the new format without issues
        let reloaded = load_metadata(&metadata_path).unwrap();
        assert_eq!(reloaded.version, METADATA_VERSION);
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded.get(Path::new("test.rs")).unwrap().is_some());
    }

    #[test]
    fn test_version_migration_logic() {
        // Test the migration function directly since we can't easily create true v1
        // files with the current structure (rkyv serialization includes the
        // struct definition)

        // Create metadata that simulates v1 structure
        let mut v1_metadata = StateMetadata::new();
        v1_metadata.version = 1; // Manually set to v1
        v1_metadata
            .upsert(FileState {
                path: PathBuf::from("legacy.rs"),
                size: 200,
                hash: "legacyhash".to_string(),
                mtime_nanos: 9876543210,
            })
            .unwrap();

        // The v1 structure didn't have last_gc_mtime_nanos, so it should be None
        assert!(v1_metadata.last_gc_mtime_nanos.is_none());
        assert_eq!(v1_metadata.version, 1);

        // Test migration function directly
        let migrated = migrate_metadata(v1_metadata).unwrap();

        // Verify migration occurred
        assert_eq!(migrated.version, METADATA_VERSION); // Should be v2 now
        assert_eq!(migrated.len(), 1);
        assert!(migrated.get(Path::new("legacy.rs")).unwrap().is_some());
        assert!(migrated.last_gc_mtime_nanos.is_none()); // v1->v2 migration preserves None
    }

    #[test]
    fn test_future_version_handling() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Create metadata with a future version
        let mut future_metadata = StateMetadata::new();
        future_metadata.version = METADATA_VERSION + 1; // Future version

        save_metadata(&future_metadata, &metadata_path).unwrap();

        // Should return a ConfigError for future versions
        let result = load_metadata(&metadata_path);
        assert!(result.is_err());

        match result.unwrap_err() {
            HoldError::ConfigError { message } => {
                assert!(message.contains("newer than supported"));
                assert!(message.contains(&(METADATA_VERSION + 1).to_string()));
            }
            other => panic!("Expected ConfigError, got: {other:?}"),
        }
    }

    #[test]
    fn test_real_world_incompatible_format_scenario() {
        let temp_dir = TempDir::new().unwrap();
        let metadata_path = temp_dir.path().join("test.metadata");

        // Simulate the exact error the user encountered by creating data that
        // would cause "hash table length must be strictly less than its capacity"
        // This mimics old format data that can't be deserialized
        let problematic_data = [
            0x72, 0x6b, 0x79, 0x76, // rkyv magic
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // corrupted length
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // corrupted capacity
        ];

        fs::write(&metadata_path, problematic_data).unwrap();

        // Load should detect incompatibility and recover gracefully
        let metadata = load_metadata(&metadata_path).unwrap();

        // Verify recovery worked
        assert_eq!(metadata.version, METADATA_VERSION);
        assert_eq!(metadata.len(), 0);
        assert!(metadata.last_gc_mtime_nanos.is_none());

        // Old file should be gone
        assert!(!metadata_path.exists());

        // Should be able to use the recovered metadata normally
        let mut recovered = metadata;
        recovered
            .upsert(FileState {
                path: PathBuf::from("recovered.rs"),
                size: 42,
                hash: "recovered".to_string(),
                mtime_nanos: 12345,
            })
            .unwrap();

        // Save should work normally after recovery
        save_metadata(&recovered, &metadata_path).unwrap();

        // And subsequent loads should work fine
        let final_metadata = load_metadata(&metadata_path).unwrap();
        assert_eq!(final_metadata.len(), 1);
        assert!(
            final_metadata
                .get(Path::new("recovered.rs"))
                .unwrap()
                .is_some()
        );
    }
}
