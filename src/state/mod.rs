use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rkyv::{Archive, Deserialize, Serialize};

use crate::error::{HoldError, Result};

#[cfg(test)]
mod tests;

/// Current version of the metadata format.
///
/// This version is incremented when incompatible changes are made to the
/// metadata format. The tool will refuse to load metadata with a version higher
/// than this constant.
pub const METADATA_VERSION: u32 = 4;

/// Represents the state of a single file at a point in time.
///
/// This struct captures all the information needed to detect changes
/// in a file and restore its timestamp correctly.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct FileState {
    /// Repository-relative path to the file.
    ///
    /// This path is relative to the Git repository root and must be valid UTF-8
    /// (as required by Git itself).
    #[rkyv(with = rkyv::with::AsString)]
    pub path: PathBuf,

    /// Size of the file in bytes.
    ///
    /// Used as a quick check before computing the hash - if the size differs,
    /// we know the file has changed without needing to read its contents.
    pub size: u64,

    /// Hex-encoded BLAKE3 hash of the file's contents.
    ///
    /// This provides a cryptographically strong guarantee that the file's
    /// contents haven't changed.
    pub hash: String,

    /// The monotonically-increasing timestamp last set on this file by
    /// cargo-hold.
    ///
    /// Stored as nanoseconds since UNIX_EPOCH to ensure precision across
    /// different filesystems and platforms.
    pub mtime_nanos: u128,
}

/// The metadata containing all tracked file states.
///
/// This is the main data structure that gets serialized to disk.
/// It provides efficient lookups by file path and tracks the metadata format
/// version.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct StateMetadata {
    /// Version of the metadata format for forward compatibility.
    ///
    /// This allows newer versions of cargo-hold to detect metadata created by
    /// even newer versions and provide helpful error messages.
    pub version: u32,

    /// A hash map providing O(1) average-case lookup time for a file's state by
    /// its path.
    ///
    /// Keys are UTF-8 string paths (relative to the Git repository root).
    /// Values are the complete state information for each file.
    pub files: HashMap<String, FileState>,

    /// The maximum mtime from the previous metadata save operation.
    ///
    /// This is used by the garbage collector to preserve artifacts from the
    /// most recent build, ensuring better cache hit ratios. When None, it
    /// means this is either the first save or we're dealing with v1
    /// metadata that was migrated.
    pub last_gc_mtime_nanos: Option<u128>,

    /// Rolling garbage-collection telemetry used to auto-tune cache sizing.
    pub gc_metrics: GcMetrics,
}

impl StateMetadata {
    /// Creates a new empty state metadata with the current metadata version.
    pub fn new() -> Self {
        Self {
            version: METADATA_VERSION,
            files: HashMap::new(),
            last_gc_mtime_nanos: None,
            gc_metrics: GcMetrics::default(),
        }
    }

    /// Returns the most recent timestamp from all files in the metadata.
    ///
    /// Returns `None` if the metadata is empty. The timestamp is in nanoseconds
    /// since UNIX_EPOCH.
    pub fn max_mtime_nanos(&self) -> Option<u128> {
        self.files.values().map(|state| state.mtime_nanos).max()
    }

    /// Updates an existing file state or inserts a new one.
    ///
    /// If a file with the same path already exists in the metadata, it will be
    /// replaced with the new state.
    ///
    /// Returns an error if the path contains invalid UTF-8.
    pub fn upsert(&mut self, state: FileState) -> Result<()> {
        let key = state
            .path
            .to_str()
            .ok_or_else(|| HoldError::InvalidUtf8Path(state.path.clone()))?
            .to_string();
        self.files.insert(key, state);
        Ok(())
    }

    /// Removes a file state from the metadata.
    ///
    /// Returns the removed `FileState` if the file was in the metadata,
    /// or `None` if it wasn't found.
    ///
    /// Returns an error if the path contains invalid UTF-8.
    pub fn remove(&mut self, path: &Path) -> Result<Option<FileState>> {
        let key = path
            .to_str()
            .ok_or_else(|| HoldError::InvalidUtf8Path(path.to_path_buf()))?;
        Ok(self.files.remove(key))
    }

    /// Gets a file state by its path.
    ///
    /// Returns a reference to the `FileState` if found, or `None` if the
    /// file is not in the metadata.
    ///
    /// Returns an error if the path contains invalid UTF-8.
    pub fn get(&self, path: &Path) -> Result<Option<&FileState>> {
        let key = path
            .to_str()
            .ok_or_else(|| HoldError::InvalidUtf8Path(path.to_path_buf()))?;
        Ok(self.files.get(key))
    }

    /// Checks if a file exists in the metadata.
    ///
    /// Returns `true` if the file is tracked in the metadata, `false`
    /// otherwise.
    ///
    /// Returns an error if the path contains invalid UTF-8.
    pub fn contains(&self, path: &Path) -> Result<bool> {
        let key = path
            .to_str()
            .ok_or_else(|| HoldError::InvalidUtf8Path(path.to_path_buf()))?;
        Ok(self.files.contains_key(key))
    }

    /// Returns the number of files tracked in the metadata.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Returns `true` if the metadata contains no files.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl Default for StateMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// Rolling statistics captured from `heave` runs to derive cache sizing hints.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Default)]
pub struct GcMetrics {
    /// Total number of GC runs recorded.
    pub runs: u32,
    /// Size of the first full build observed; used as a baseline.
    pub seed_initial_size: Option<u64>,
    /// Bounded window of recent initial target directory sizes before GC
    /// (bytes).
    pub recent_initial_sizes: Vec<u64>,
    /// Bounded window of recent freed byte counts (bytes).
    pub recent_bytes_freed: Vec<u64>,
    /// Last suggested cap (bytes) recorded by auto-sizing.
    pub last_suggested_cap: Option<u64>,
    /// Bounded window of final target directory sizes after GC (bytes).
    pub recent_final_sizes: Vec<u64>,
    /// Last recorded cap computation trace for observability/debugging.
    pub last_cap_trace: Option<CapTrace>,
}

/// Diagnostic trace of the most recent auto-cap computation.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Default)]
pub struct CapTrace {
    /// Median-ish footprint the algorithm targeted.
    pub baseline: u64,
    /// Growth headroom that was added.
    pub growth_budget: u64,
    /// Observed p90 growth percentage over recent finals.
    pub observed_growth_pct: u64,
    /// Why the final clamp decision was chosen.
    pub clamp_reason: String,
}
