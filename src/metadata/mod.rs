use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use memmap2::Mmap;

use crate::error::{HoldError, Result};
use crate::state::{METADATA_VERSION, StateMetadata};

#[cfg(test)]
mod tests;

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

    // The sizing model is intentionally a clean current-state format. Older
    // layouts fail deserialization and are reset by load_metadata().
    let metadata = deserialize_metadata(&mmap[..])?;

    // Check version compatibility
    if metadata.version > METADATA_VERSION {
        return Err(HoldError::ConfigError(format!(
            "Metadata version {} is newer than supported version {}. Please update cargo-hold.",
            metadata.version, METADATA_VERSION
        )));
    }
    if metadata.version < METADATA_VERSION {
        eprintln!(
            "⚠️  Metadata version {} is incompatible with current version {}; resetting metadata",
            metadata.version, METADATA_VERSION
        );
        fs::remove_file(metadata_path).map_err(|source| HoldError::IoError {
            path: metadata_path.to_path_buf(),
            source,
        })?;
        return Ok(StateMetadata::new());
    }

    Ok(metadata)
}

fn deserialize_metadata(bytes: &[u8]) -> Result<StateMetadata> {
    rkyv::from_bytes::<StateMetadata, rkyv::rancor::BoxedError>(bytes)
        .map_err(HoldError::DeserializationError)
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
