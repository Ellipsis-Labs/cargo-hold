use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use regex::Regex;

use super::size::format_size;
use crate::error::{HoldError, Result};
use crate::logging::Logger;
use crate::timestamp::saturating_duration_from_nanos;

/// Information about a single artifact
#[derive(Debug, Clone)]
pub(crate) struct ArtifactInfo {
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    pub(crate) _modified: SystemTime,
}

/// A crate artifact group (all related files for a single crate)
#[derive(Debug)]
pub(crate) struct CrateArtifact {
    pub(crate) name: String,
    pub(crate) hash: String,
    pub(crate) artifacts: Vec<ArtifactInfo>,
    pub(crate) total_size: u64,
    pub(crate) newest_mtime: SystemTime,
}

/// Collect all crate artifacts from a profile directory
pub(crate) fn collect_crate_artifacts(profile_dir: &Path) -> Result<Vec<CrateArtifact>> {
    let fingerprint_dir = profile_dir.join(".fingerprint");
    if !fingerprint_dir.exists() {
        return Ok(Vec::new());
    }

    let mut crate_map: HashMap<(String, String), CrateArtifact> = HashMap::new();

    // Scan fingerprint directory to identify crates
    let entries = fs::read_dir(&fingerprint_dir).map_err(|source| HoldError::IoError {
        path: fingerprint_dir.clone(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| HoldError::IoError {
            path: fingerprint_dir.clone(),
            source,
        })?;
        let path = entry.path();

        if path.is_dir()
            && let Some((name, hash)) = parse_crate_artifact_name(&path)
        {
            let key = (name.clone(), hash.clone());
            let crate_artifact = crate_map.entry(key).or_insert_with(|| CrateArtifact {
                name,
                hash,
                artifacts: Vec::new(),
                total_size: 0,
                newest_mtime: SystemTime::UNIX_EPOCH,
            });

            // Add the fingerprint directory itself as an artifact
            add_artifact_file(&path, crate_artifact)?;
        }
    }

    // Now find related artifacts in deps and build directories
    for (subdir, _patterns) in &[("deps", vec!["*"]), ("build", vec!["*"])] {
        let dir = profile_dir.join(subdir);
        if !dir.exists() {
            continue;
        }

        let entries = fs::read_dir(&dir).map_err(|source| HoldError::IoError {
            path: dir.clone(),
            source,
        })?;

        for entry in entries {
            let entry = entry.map_err(|source| HoldError::IoError {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();

            // Try to match this file to a crate
            if let Some((name, hash)) = parse_crate_artifact_name(&path) {
                let key = (name.clone(), hash.clone());
                if let Some(crate_artifact) = crate_map.get_mut(&key) {
                    add_artifact_file(&path, crate_artifact)?;
                } else {
                    // This file doesn't have a corresponding fingerprint entry
                    // Create a new crate artifact for orphaned files
                    let mut artifact = CrateArtifact {
                        name: name.clone(),
                        hash: hash.clone(),
                        artifacts: Vec::new(),
                        total_size: 0,
                        newest_mtime: SystemTime::UNIX_EPOCH,
                    };
                    add_artifact_file(&path, &mut artifact)?;
                    crate_map.insert(key, artifact);
                }
            }
        }
    }

    Ok(crate_map.into_values().collect())
}

/// Parse a crate artifact filename to extract name and hash
pub(crate) fn parse_crate_artifact_name(path: &Path) -> Option<(String, String)> {
    static CRATE_ARTIFACT_RE: OnceLock<Regex> = OnceLock::new();

    let filename = path.file_name()?.to_str()?;
    let re = CRATE_ARTIFACT_RE.get_or_init(|| {
        Regex::new(r"^(.+)-([0-9a-f]{16})(?:\.|$)").expect("crate artifact regex should compile")
    });
    let captures = re.captures(filename)?;

    Some((captures[1].to_string(), captures[2].to_string()))
}

/// Add artifact files to a crate artifact
fn add_artifact_files(path: &Path, crate_artifact: &mut CrateArtifact) -> Result<()> {
    if path.is_file() {
        add_artifact_file(path, crate_artifact)?;
    } else if path.is_dir() {
        let entries = fs::read_dir(path).map_err(|source| HoldError::IoError {
            path: path.to_path_buf(),
            source,
        })?;

        for entry in entries {
            let entry = entry.map_err(|source| HoldError::IoError {
                path: path.to_path_buf(),
                source,
            })?;
            add_artifact_files(&entry.path(), crate_artifact)?;
        }
    }

    Ok(())
}

/// Add a single artifact file to a crate artifact
fn add_artifact_file(path: &Path, crate_artifact: &mut CrateArtifact) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|source| HoldError::IoError {
        path: path.to_path_buf(),
        source,
    })?;

    // If it's a directory, add all its contents but not the directory itself
    if metadata.is_dir() {
        add_artifact_files(path, crate_artifact)?;
        // Also add the directory itself as an artifact to ensure it gets removed
        let artifact_info = ArtifactInfo {
            path: path.to_path_buf(),
            size: 0,                           // Directories don't have meaningful size
            _modified: SystemTime::UNIX_EPOCH, // Don't use directory mtime for age calculation
        };
        crate_artifact.artifacts.push(artifact_info);
    } else {
        // For files, track their modification time
        let modified = metadata.modified().map_err(|source| HoldError::IoError {
            path: path.to_path_buf(),
            source,
        })?;

        let artifact_info = ArtifactInfo {
            path: path.to_path_buf(),
            size: metadata.len(),
            _modified: modified,
        };

        crate_artifact.total_size += artifact_info.size;
        if modified > crate_artifact.newest_mtime {
            crate_artifact.newest_mtime = modified;
        }

        crate_artifact.artifacts.push(artifact_info);
    }

    Ok(())
}

/// Select artifacts to remove based on both size and age constraints
///
/// This function implements a two-phase cleanup strategy:
/// 1. **Size enforcement**: If a size limit is specified and exceeded, removes
///    oldest artifacts first until the target directory is under the limit
/// 2. **Age cleanup**: After size compliance, removes any remaining artifacts
///    older than the specified age threshold
///
/// Both phases are always executed, ensuring consistent and predictable cleanup
/// behavior.
///
/// # Arguments
///
/// * `crate_artifacts` - List of crate artifacts to consider for removal
/// * `current_size` - Current total size of all artifacts in bytes
/// * `max_size` - Optional maximum size limit in bytes
/// * `age_threshold_days` - Age threshold in days (artifacts older than this
///   are removed)
/// * `previous_build_mtime_nanos` - Optional timestamp of the previous build to
///   preserve
/// * `verbose` - Verbosity level for debug output
/// * `quiet` - Suppress logging
///
/// # Returns
///
/// A vector of references to artifacts that should be removed
pub(crate) fn select_artifacts_for_removal(
    crate_artifacts: &[CrateArtifact],
    current_size: u64,
    max_size: Option<u64>,
    age_threshold_days: u32,
    previous_build_mtime_nanos: Option<u128>,
    verbose: u8,
    quiet: bool,
) -> Vec<&CrateArtifact> {
    let remaining = preserve_previous_build_artifacts(
        crate_artifacts.iter().collect(),
        previous_build_mtime_nanos,
        age_threshold_days,
        verbose,
        quiet,
    );

    let (mut to_remove, remaining) = select_for_size(remaining, current_size, max_size, quiet);
    let age_selected = select_for_age(remaining, age_threshold_days, verbose, quiet);
    to_remove.extend(age_selected);

    to_remove
}

fn preserve_previous_build_artifacts(
    artifacts: Vec<&CrateArtifact>,
    previous_build_mtime_nanos: Option<u128>,
    age_threshold_days: u32,
    verbose: u8,
    quiet: bool,
) -> Vec<&CrateArtifact> {
    let log = Logger::new(verbose, quiet);
    if let Some(previous_mtime_nanos) = previous_build_mtime_nanos {
        let (duration, saturated) = saturating_duration_from_nanos(previous_mtime_nanos);
        if saturated && !log.quiet() {
            eprintln!(
                "Warning: previous_build_mtime_nanos ({previous_mtime_nanos}) exceeds \
                 representable range; clamping to ~year 2554.",
            );
        }

        let mut previous_mtime = SystemTime::UNIX_EPOCH + duration;
        let now = SystemTime::now();
        if previous_mtime > now {
            previous_mtime = now;
        }

        if age_threshold_days == 0 {
            log.verbose(
                2,
                "  Skipping previous build preservation because age threshold is 0 days",
            );
            return artifacts;
        }

        let age_threshold =
            std::time::Duration::from_secs(age_threshold_days as u64 * 24 * 60 * 60);
        let elapsed_since_previous = now
            .duration_since(previous_mtime)
            .unwrap_or(std::time::Duration::ZERO);

        if elapsed_since_previous > age_threshold {
            log.verbose(
                1,
                format!(
                    "  Previous build timestamp is {elapsed_since_previous:?} old; exceeding \
                     threshold, skipping preservation"
                ),
            );
            return artifacts;
        }

        // Add a generous buffer to account for clock drift and build finishing before
        // GC.
        let buffer = std::time::Duration::from_secs(5 * 60);
        let cutoff_time = previous_mtime
            .checked_sub(buffer)
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let (preserved, eligible): (Vec<_>, Vec<_>) = artifacts
            .into_iter()
            .partition(|artifact| artifact.newest_mtime >= cutoff_time);

        if !log.quiet() && !preserved.is_empty() {
            let preserved_size: u64 = preserved.iter().map(|a| a.total_size).sum();
            eprintln!(
                "  Preserving {} artifacts ({}) from previous build",
                preserved.len(),
                format_size(preserved_size)
            );
            if log.level() > 1 {
                for artifact in &preserved {
                    eprintln!("    Preserving: {}-{}", artifact.name, artifact.hash);
                }
            }
        }

        return eligible;
    }

    artifacts
}

fn select_for_size(
    mut remaining_artifacts: Vec<&CrateArtifact>,
    current_size: u64,
    max_size: Option<u64>,
    quiet: bool,
) -> (Vec<&CrateArtifact>, Vec<&CrateArtifact>) {
    let mut to_remove = Vec::new();
    let log = Logger::new(0, quiet);

    if let Some(max_size) = max_size {
        if !log.quiet() {
            eprintln!(
                "  Size-based cleanup: current={}, max={}",
                format_size(current_size),
                format_size(max_size)
            );
        }

        if current_size > max_size {
            let needed = current_size - max_size;
            if !log.quiet() {
                eprintln!("  Need to free: {}", format_size(needed));
            }

            // Sort by age (oldest first)
            remaining_artifacts.sort_by_key(|a| a.newest_mtime);

            let mut freed = 0u64;
            let mut kept_artifacts = Vec::new();

            for artifact in remaining_artifacts {
                if freed < needed {
                    to_remove.push(artifact);
                    freed += artifact.total_size;
                } else {
                    kept_artifacts.push(artifact);
                }
            }

            remaining_artifacts = kept_artifacts;

            if !log.quiet() {
                eprintln!(
                    "  Size cleanup will remove {} crates, freeing {}",
                    to_remove.len(),
                    format_size(freed)
                );
            }
        } else if !log.quiet() {
            eprintln!("  Already within target size");
        }
    }

    (to_remove, remaining_artifacts)
}

fn select_for_age(
    remaining_artifacts: Vec<&CrateArtifact>,
    age_threshold_days: u32,
    verbose: u8,
    quiet: bool,
) -> Vec<&CrateArtifact> {
    let mut to_remove = Vec::new();
    let log = Logger::new(verbose, quiet);

    if !log.quiet() {
        eprintln!("  Age-based cleanup: removing artifacts older than {age_threshold_days} days");
    }

    let cutoff = SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(
            age_threshold_days as u64 * 24 * 60 * 60,
        ))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let now = SystemTime::now();
    let mut age_removed_count = 0;
    let mut age_removed_size = 0u64;

    for artifact in remaining_artifacts {
        let age_days = now
            .duration_since(artifact.newest_mtime)
            .map(|d| d.as_secs() / (24 * 60 * 60))
            .unwrap_or(0);

        if artifact.newest_mtime < cutoff {
            log.verbose(
                2,
                format!(
                    "    Removing old crate {}: age={} days",
                    artifact.name, age_days
                ),
            );
            age_removed_count += 1;
            age_removed_size += artifact.total_size;
            to_remove.push(artifact);
        }
    }

    if !log.quiet() {
        eprintln!(
            "  Age cleanup will remove {} additional crates, freeing {}",
            age_removed_count,
            format_size(age_removed_size)
        );
    }

    to_remove
}

/// Remove all artifacts for a crate
pub(crate) fn remove_crate_artifacts(crate_artifact: &CrateArtifact) -> Result<()> {
    for artifact in &crate_artifact.artifacts {
        if artifact.path.exists() {
            if artifact.path.is_dir() {
                fs::remove_dir_all(&artifact.path).map_err(|source| HoldError::IoError {
                    path: artifact.path.clone(),
                    source,
                })?;
            } else {
                fs::remove_file(&artifact.path).map_err(|source| HoldError::IoError {
                    path: artifact.path.clone(),
                    source,
                })?;
            }
        }
    }

    Ok(())
}
