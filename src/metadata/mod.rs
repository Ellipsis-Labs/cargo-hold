use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use memmap2::Mmap;
use rkyv::{Archive, Deserialize, Serialize};

use crate::error::{HoldError, Result};
use crate::state::{FileState, GcMetrics, METADATA_VERSION, StateMetadata};

#[cfg(test)]
mod tests;

/// Legacy layout for v2 metadata files (without GC metrics).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
struct StateMetadataV2 {
    pub version: u32,
    pub files: HashMap<String, FileState>,
    pub last_gc_mtime_nanos: Option<u128>,
}

impl From<StateMetadataV2> for StateMetadata {
    fn from(v2: StateMetadataV2) -> Self {
        StateMetadata {
            version: v2.version,
            files: v2.files,
            last_gc_mtime_nanos: v2.last_gc_mtime_nanos,
            gc_metrics: GcMetrics::default(),
        }
    }
}

/// Legacy layout for v3 metadata files (first to include GC metrics).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
struct StateMetadataV3 {
    pub version: u32,
    pub files: HashMap<String, FileState>,
    pub last_gc_mtime_nanos: Option<u128>,
    pub gc_metrics: GcMetricsV3,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Default)]
struct GcMetricsV3 {
    pub runs: u32,
    pub seed_initial_size: Option<u64>,
    pub recent_initial_sizes: Vec<u64>,
    pub recent_bytes_freed: Vec<u64>,
    pub last_suggested_cap: Option<u64>,
}

impl From<StateMetadataV3> for StateMetadata {
    fn from(v3: StateMetadataV3) -> Self {
        StateMetadata {
            version: v3.version,
            files: v3.files,
            last_gc_mtime_nanos: v3.last_gc_mtime_nanos,
            gc_metrics: GcMetrics {
                runs: v3.gc_metrics.runs,
                seed_initial_size: v3.gc_metrics.seed_initial_size,
                recent_initial_sizes: v3.gc_metrics.recent_initial_sizes,
                recent_bytes_freed: v3.gc_metrics.recent_bytes_freed,
                last_suggested_cap: v3.gc_metrics.last_suggested_cap,
                recent_final_sizes: Vec::new(),
                last_cap_trace: None,
            },
        }
    }
}

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

    // Deserialize using rkyv, with fallback to the v2 layout that didn't
    // include GC metrics. This ensures older v2 metadata can still be loaded
    // and migrated forward without being treated as incompatible.
    let metadata = deserialize_metadata(&mmap[..])?;

    // Check version compatibility
    if metadata.version > METADATA_VERSION {
        return Err(HoldError::ConfigError(format!(
            "Metadata version {} is newer than supported version {}. Please update cargo-hold.",
            metadata.version, METADATA_VERSION
        )));
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
/// - v2 -> v3: Adds gc_metrics with defaults
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

    // Migration from v2 to v3
    if metadata.version == 2 {
        // Initialize GC metrics with defaults to preserve forward compatibility.
        metadata.gc_metrics = Default::default();
        metadata.version = 3;
    }

    // Migration from v3 to v4: add recent_final_sizes + last_cap_trace
    if metadata.version == 3 {
        metadata.gc_metrics.recent_final_sizes = Vec::new();
        metadata.gc_metrics.last_cap_trace = None;
        metadata.version = 4;
    }

    Ok(metadata)
}

fn deserialize_metadata(bytes: &[u8]) -> Result<StateMetadata> {
    match rkyv::from_bytes::<StateMetadata, rkyv::rancor::BoxedError>(bytes) {
        Ok(metadata) => Ok(metadata),
        Err(primary_err) => {
            if let Ok(v3) = rkyv::from_bytes::<StateMetadataV3, rkyv::rancor::BoxedError>(bytes) {
                return Ok(StateMetadata::from(v3));
            }
            if let Ok(v2) = rkyv::from_bytes::<StateMetadataV2, rkyv::rancor::BoxedError>(bytes) {
                return Ok(StateMetadata::from(v2));
            }
            Err(HoldError::DeserializationError(primary_err))
        }
    }
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
        fs::create_dir_all(parent)
            .map_err(|source| HoldError::CreateMetadataDirError(parent.to_path_buf(), source))?;
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
