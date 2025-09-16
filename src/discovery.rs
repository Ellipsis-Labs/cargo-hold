use std::path::{Path, PathBuf};

use git2::{Index, Repository};

use crate::error::HoldError;

/// Discovers all tracked files in the Git repository.
///
/// This function uses the Git index to find all files that are tracked by Git,
/// automatically respecting `.gitignore` rules. The returned paths are relative
/// to the repository root. Symbolic links tracked by Git are included in the
/// results but can be filtered by the caller if needed.
///
/// # Arguments
///
/// * `repo_path` - A path within the Git repository (will search upward for the
///   repo root)
///
/// # Returns
///
/// A tuple containing:
/// - The repository root path (absolute)
/// - A vector of file paths relative to the repository root
/// - A count of skipped symbolic links
///
/// # Errors
///
/// Returns an error if:
/// - No Git repository is found at or above the given path
/// - The Git index cannot be accessed
/// - Any file path contains invalid UTF-8
pub fn discover_tracked_files(
    repo_path: &Path,
) -> Result<(PathBuf, Vec<PathBuf>, usize), HoldError> {
    // Open the repository, searching upward from the given path
    let repo = Repository::discover(repo_path).map_err(|_| HoldError::RepoNotFound {
        path: repo_path.to_path_buf(),
    })?;

    // Get the repository root
    let repo_root = repo
        .workdir()
        .ok_or_else(|| HoldError::RepoNotFound {
            path: repo_path.to_path_buf(),
        })?
        .to_path_buf();

    // Access the Git index
    let index = repo.index().map_err(HoldError::IndexError)?;

    // Collect all tracked file paths, filtering out symlinks
    let (tracked_files, symlink_count) = collect_index_paths(&index, &repo_root)?;

    Ok((repo_root, tracked_files, symlink_count))
}

/// Extract all file paths from the Git index, filtering out symlinks
fn collect_index_paths(
    index: &Index,
    repo_root: &Path,
) -> Result<(Vec<PathBuf>, usize), HoldError> {
    let mut paths = Vec::new();
    let mut symlink_count = 0;

    for entry in index.iter() {
        // Skip submodules (mode 160000) - they appear as directories in the filesystem
        // but are special entries in git that we can't set timestamps on
        if entry.mode == 0o160000 {
            continue;
        }

        // Get the path from the index entry - it's already relative to repo root
        let path = entry.path;

        // Convert path bytes to string and then to PathBuf
        let path_str = std::str::from_utf8(&path).map_err(|e| HoldError::InvalidPath {
            message: format!("Invalid UTF-8 in path: {e}"),
        })?;

        let path_buf = PathBuf::from(path_str);

        // Check if the file is a symlink in the actual filesystem
        let full_path = repo_root.join(&path_buf);
        if let Ok(metadata) = std::fs::symlink_metadata(&full_path) {
            if metadata.is_symlink() {
                symlink_count += 1;
                continue; // Skip symlinks
            }
        }

        paths.push(path_buf);
    }

    Ok((paths, symlink_count))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn setup_test_repo() -> (TempDir, Repository) {
        let temp_dir = TempDir::new().unwrap();
        let repo = Repository::init(temp_dir.path()).unwrap();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        // Add to index
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("test.txt")).unwrap();
        index.write().unwrap();

        (temp_dir, repo)
    }

    #[test]
    fn test_discover_tracked_files() {
        let (temp_dir, _repo) = setup_test_repo();

        let (repo_root, files, symlink_count) = discover_tracked_files(temp_dir.path()).unwrap();
        // On macOS, /var is a symlink to /private/var, so we need to canonicalize paths
        assert_eq!(
            repo_root.canonicalize().unwrap(),
            temp_dir.path().canonicalize().unwrap()
        );
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("test.txt"));
        assert_eq!(symlink_count, 0);
    }

    #[test]
    fn test_repo_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let result = discover_tracked_files(temp_dir.path());
        assert!(matches!(result, Err(HoldError::RepoNotFound { .. })));
    }
}
